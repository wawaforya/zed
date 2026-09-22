use anyhow::Context as _;
use base64::Engine as _;
use futures::{
    AsyncBufReadExt as _, AsyncWriteExt as _, FutureExt as _, SinkExt as _, StreamExt as _,
};
use gpui::{
    AppContext as _, Focusable as _, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _,
};
use theme::ActiveTheme as _;
use ui::{ButtonCommon as _, Clickable as _, Disableable as _, StyledExt as _};
use util::ResultExt as _;
use workspace::{Item as _, ToolbarItemView as _};

#[cfg(test)]
mod tests;

const MAX_FRAME: usize = 2 * 1024 * 1024;
const MAX_RESULTS: usize = 16;
const MAX_LOG: usize = 256 * 1024;

gpui::actions!(http, [ToggleResponse]);

pub fn init(cx: &mut gpui::App) {
    workspace::tasks::register_native_task_runner(
        |workspace, task, window, cx| {
            if task.env.get("ZED_HTTPYAC_UI").map(String::as_str) != Some("1")
                || task.command.as_deref() != Some("node")
                || !task
                    .args
                    .get(1)
                    .is_some_and(|argument| argument.contains("ZED_CUSTOM_HTTPYAC_EXECUTOR"))
            {
                return None;
            }
            let origin = task
                .env
                .get("ZED_CUSTOM_SOURCE_EDITOR")
                .and_then(|id| id.parse::<u64>().ok());
            let candidates = workspace
                .items_of_type::<editor::Editor>(cx)
                .map(|editor| editor.downgrade())
                .collect::<Vec<_>>();
            let source = if let Some(id) = origin {
                workspace
                    .items_of_type::<editor::Editor>(cx)
                    .find(|editor| editor.entity_id().as_u64() == id)
            } else {
                workspace.active_item_as::<editor::Editor>(cx)
            };
            let Some(source) = source else {
                workspace.show_toast(
                    workspace::Toast::new(
                        workspace::notifications::NotificationId::unique::<ToggleResponse>(),
                        "Open the HTTP source tab before running this task",
                    ),
                    cx,
                );
                return Some(gpui::Task::ready(
                    workspace::tasks::ScheduledTaskResult::SpawnFailed,
                ));
            };
            let source = source.downgrade();
            let project = workspace.project().clone();
            let task = task.clone();
            Some(cx.spawn_in(window, async move |workspace, cx| {
                run_in_tab(source, origin, candidates, project, task, workspace, cx).await
            }))
        },
        cx,
    );
    cx.observe_new(|workspace: &mut workspace::Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleResponse, window, cx| {
            let Some(source) = workspace.active_item_as::<editor::Editor>(cx) else {
                return;
            };
            toggle_response(source, window, cx);
        });
    })
    .detach();
}

async fn run_in_tab(
    source: gpui::WeakEntity<editor::Editor>,
    origin: Option<u64>,
    candidates: Vec<gpui::WeakEntity<editor::Editor>>,
    project: gpui::Entity<project::Project>,
    task: task::SpawnInTerminal,
    workspace: gpui::WeakEntity<workspace::Workspace>,
    cx: &mut gpui::AsyncWindowContext,
) -> workspace::tasks::ScheduledTaskResult {
    // Task scheduling can be invoked while the source Editor is being updated.
    // Access it only after that update has returned.
    let source = if origin.is_none()
        && let Some(file) = task.env.get("ZED_FILE")
    {
        std::iter::once(source.clone())
            .chain(candidates)
            .find(|candidate| {
                candidate
                    .read_with(cx, |source, cx| {
                        source_path(source, &project, cx).as_ref() == Some(file)
                    })
                    .unwrap_or(false)
            })
            .unwrap_or(source)
    } else {
        source
    };
    let view = source.update_in(cx, ensure_response);
    let Ok(view) = view else {
        return workspace::tasks::ScheduledTaskResult::Cancelled;
    };
    if task.args.iter().any(|arg| arg == "reset")
        && view.read_with(cx, |view, _| view.controls.is_some())
    {
        view.update(cx, |view, _| {
            if let Some(cancel) = view.cancel.take()
                && cancel.send(()).is_err()
            {
                log::debug!("HTTP execution already finished");
            }
        });
        while view.read_with(cx, |view, _| view.controls.is_some()) {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(50))
                .await;
        }
    }
    if view.read_with(cx, |view, _| view.controls.is_some()) {
        workspace.update(cx, |workspace, cx| workspace.show_toast(workspace::Toast::new(
            workspace::notifications::NotificationId::unique::<ToggleResponse>(),
            "An HTTP request is already running in this tab; cancel it or wait before sending again",
        ), cx)).log_err();
        return workspace::tasks::ScheduledTaskResult::Cancelled;
    }
    let response_view = view.downgrade();
    // The source tab must be the sole owner while IO is pending, so closing it
    // drops the cancellation sender instead of keeping an invisible run alive.
    drop(view);
    match execute(source, response_view.clone(), project, task, workspace, cx).await {
        Ok(result) => {
            if let Some(view) = response_view.upgrade() {
                view.update_in(cx, |view, window, cx| {
                    let status = match result {
                        workspace::tasks::ScheduledTaskResult::Success => "Finished",
                        workspace::tasks::ScheduledTaskResult::Cancelled => "Cancelled — the request may have been sent; session state may have been reset",
                        _ => "Failed — see Tests / Log",
                    };
                    view.finish(status.into(), window, cx);
                }).log_err();
            }
            result
        }
        Err(error) => {
            if let Some(view) = response_view.upgrade() {
                view.update_in(cx, |view, window, cx| {
                    view.finish(format!("HTTP execution failed: {error:#}"), window, cx);
                })
                .log_err();
            }
            workspace::tasks::ScheduledTaskResult::Failure
        }
    }
}

pub fn is_http_editor(source: &gpui::Entity<editor::Editor>, cx: &gpui::App) -> bool {
    source
        .read(cx)
        .buffer()
        .read(cx)
        .as_singleton()
        .is_some_and(|buffer| {
            buffer
                .read(cx)
                .language()
                .is_some_and(|language| language.name().as_ref() == "HTTP")
        })
}

pub fn toggle_response(
    source: gpui::Entity<editor::Editor>,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) {
    if !is_http_editor(&source, cx) {
        return;
    }
    source.update(cx, |source, cx| {
        if let Some(addon) = source.addon_mut::<EmbeddedResponse>() {
            addon.visible = !addon.visible;
        } else {
            ensure_response(source, window, cx);
        }
        source.focus_handle(cx).focus(window, cx);
        cx.notify();
    });
}

fn source_path(
    source: &editor::Editor,
    project: &gpui::Entity<project::Project>,
    cx: &gpui::App,
) -> Option<String> {
    let buffer = source.buffer().read(cx).as_singleton()?;
    let file = buffer.read(cx).file()?;
    project
        .read(cx)
        .absolute_path(
            &project::ProjectPath {
                worktree_id: file.worktree_id(cx),
                path: file.path().clone(),
            },
            cx,
        )
        .map(|path| path.to_string_lossy().into_owned())
}

struct EmbeddedResponse {
    view: gpui::Entity<ResponseView>,
    source: gpui::WeakEntity<editor::Editor>,
    ratio: f32,
    visible: bool,
}

#[derive(Clone)]
struct Divider;

impl editor::Addon for EmbeddedResponse {
    fn render_editor(
        &self,
        editor: gpui::AnyElement,
        _: &mut gpui::Window,
        cx: &gpui::App,
    ) -> gpui::AnyElement {
        if !self.visible {
            return editor;
        }
        let source = self.source.clone();
        let reset_source = self.source.clone();
        ui::h_flex()
            .id("http-tab-split")
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .on_drag_move::<Divider>(move |event, _, cx| {
                if event.bounds.size.width <= gpui::px(0.) {
                    return;
                }
                source
                    .update(cx, |source, cx| {
                        if let Some(addon) = source.addon_mut::<EmbeddedResponse>() {
                            addon.ratio = ((event.event.position.x - event.bounds.left())
                                / event.bounds.size.width)
                                .clamp(0.1, 0.9);
                            cx.notify();
                        }
                    })
                    .log_err();
            })
            .child(
                gpui::div()
                    .h_full()
                    .min_w_0()
                    .flex_shrink_1()
                    .flex_basis(gpui::DefiniteLength::Fraction(self.ratio))
                    .overflow_hidden()
                    .child(editor),
            )
            .child(
                gpui::div()
                    .relative()
                    .h_full()
                    .w(gpui::px(1.))
                    .flex_shrink_0()
                    .bg(cx.theme().colors().border_variant)
                    .child(
                        gpui::div()
                            .id("http-divider")
                            .absolute()
                            .left(gpui::px(-4.))
                            .w(gpui::px(8.))
                            .h_full()
                            .cursor_col_resize()
                            .block_mouse_except_scroll()
                            .on_click(move |event, _, cx| {
                                if event.click_count() == 2 {
                                    reset_source
                                        .update(cx, |source, cx| {
                                            if let Some(addon) =
                                                source.addon_mut::<EmbeddedResponse>()
                                            {
                                                addon.ratio = 0.5;
                                                cx.notify();
                                            }
                                        })
                                        .log_err();
                                }
                                cx.stop_propagation();
                            })
                            .on_drag(Divider, |_, _, _, cx| cx.new(|_| gpui::Empty)),
                    ),
            )
            .child(
                gpui::div()
                    .h_full()
                    .min_w_0()
                    .flex_shrink_1()
                    .flex_basis(gpui::DefiniteLength::Fraction(1.0 - self.ratio))
                    .overflow_hidden()
                    .child(self.view.clone()),
            )
            .into_any_element()
    }

    fn to_any(&self) -> &dyn std::any::Any {
        self
    }
    fn to_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

fn ensure_response(
    source: &mut editor::Editor,
    window: &mut gpui::Window,
    cx: &mut gpui::Context<editor::Editor>,
) -> gpui::Entity<ResponseView> {
    if let Some(addon) = source.addon_mut::<EmbeddedResponse>() {
        addon.visible = true;
        let view = addon.view.clone();
        cx.notify();
        return view;
    }
    let weak_source = cx.weak_entity();
    let languages = source
        .project()
        .map(|project| project.read(cx).languages().clone());
    let view = cx.new(|cx| ResponseView::new(weak_source.clone(), languages, window, cx));
    source.register_addon(EmbeddedResponse {
        view: view.clone(),
        source: weak_source,
        ratio: 0.5,
        visible: true,
    });
    cx.notify();
    view
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Response {
    name: String,
    status: u16,
    status_message: String,
    headers: String,
    request: String,
    body: String,
    raw_body: String,
    body_bytes: usize,
    truncated: bool,
    text_truncated: bool,
    content_type: String,
    timings: serde_json::Value,
}

#[derive(serde::Deserialize)]
struct Prompt {
    id: String,
    kind: String,
    message: String,
    #[serde(default, rename = "defaultValue")]
    default_value: serde_json::Value,
    #[serde(default)]
    choices: Vec<serde_json::Value>,
}

#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum Event {
    Started { streaming: bool },
    Response(Response),
    Output { stream: String, text: String },
    Test { status: String, message: String },
    Prompt(Prompt),
    Error { message: String },
    Done { code: i32 },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Body,
    Headers,
    Request,
    Tests,
    Log,
}

struct ResponseView {
    source: gpui::WeakEntity<editor::Editor>,
    body: gpui::Entity<editor::Editor>,
    search: gpui::Entity<search::BufferSearchBar>,
    prompt_input: gpui::Entity<editor::Editor>,
    languages: Option<std::sync::Arc<language::LanguageRegistry>>,
    responses: std::collections::VecDeque<Response>,
    selected: usize,
    section: Section,
    select_initial_section: bool,
    pretty: bool,
    status: String,
    logs: String,
    tests: String,
    prompt: Option<Prompt>,
    controls: Option<futures::channel::mpsc::UnboundedSender<serde_json::Value>>,
    cancel: Option<futures::channel::oneshot::Sender<()>>,
    language_task: Option<gpui::Task<()>>,
    _search_subscription: gpui::Subscription,
}

impl ResponseView {
    fn new(
        source: gpui::WeakEntity<editor::Editor>,
        languages: Option<std::sync::Arc<language::LanguageRegistry>>,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let body = cx.new(|cx| {
            let mut editor = editor::Editor::multi_line(window, cx);
            editor.set_read_only(true);
            editor.set_custom_context_menu(|editor, _, window, cx| {
                let has_selection = editor.has_non_empty_selection(&editor.display_snapshot(cx));
                let focus = editor.focus_handle(cx);
                let body = cx.weak_entity();
                Some(ui::ContextMenu::build(window, cx, |menu, _, _| {
                    menu.context(focus)
                        .action_disabled_when(
                            !has_selection,
                            "Copy",
                            Box::new(editor::actions::Copy),
                        )
                        .entry("Copy All", None, move |_, cx| {
                            if let Some(body) = body.upgrade() {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                    body.read(cx).text(cx),
                                ));
                            }
                        })
                        .action("Select All", Box::new(editor::actions::SelectAll))
                        .separator()
                        .action_disabled_when(
                            !has_selection,
                            "Find Selection",
                            Box::new(search::buffer_search::UseSelectionForFind),
                        )
                }))
            });
            editor
        });
        let search = cx.new(|cx| {
            let mut search = search::BufferSearchBar::new(languages.clone(), window, cx);
            search.set_active_pane_item(Some(&body), window, cx);
            search.disallow_replacement();
            search
        });
        let prompt_input = cx.new(|cx| editor::Editor::single_line(window, cx));
        let search_subscription = cx.observe(&search, |_, _, cx| cx.notify());
        Self {
            source,
            body,
            search,
            prompt_input,
            languages,
            responses: Default::default(),
            selected: 0,
            section: Section::Body,
            select_initial_section: false,
            pretty: true,
            status: "No response yet".into(),
            logs: String::new(),
            tests: String::new(),
            prompt: None,
            controls: None,
            cancel: None,
            language_task: None,
            _search_subscription: search_subscription,
        }
    }

    fn status_summary(&self) -> String {
        let status = if self.status.starts_with("HTTP execution failed:")
            || self.status.starts_with("Failed")
        {
            "Failed"
        } else if self.status.starts_with("Cancelled") {
            "Cancelled"
        } else if self.status.starts_with("Cancelling") {
            "Cancelling…"
        } else if self.status.starts_with("Execution disconnected") {
            "Disconnected"
        } else if self.status.starts_with("Cannot export response:")
            || self.status.starts_with("Export failed:")
        {
            "Export failed"
        } else {
            &self.status
        };
        self.responses
            .get(self.selected)
            .map(|response| {
                format!(
                    "{status} · {} {} · {} bytes · {} ms{}",
                    response.status,
                    response.status_message,
                    response.body_bytes,
                    response
                        .timings
                        .get("total")
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "?".into()),
                    if response.text_truncated || response.truncated {
                        " · preview truncated"
                    } else {
                        ""
                    },
                )
            })
            .unwrap_or_else(|| status.to_owned())
    }

    fn text(&self) -> String {
        match self.section {
            Section::Tests => self.tests.clone(),
            Section::Log => self.logs.clone(),
            section => self
                .responses
                .get(self.selected)
                .map(|response| match section {
                    Section::Body if self.pretty && !response.text_truncated => {
                        pretty_json(&response.body).unwrap_or_else(|| response.body.clone())
                    }
                    Section::Body => response.body.clone(),
                    Section::Headers => response.headers.clone(),
                    Section::Request => response.request.clone(),
                    _ => String::new(),
                })
                .unwrap_or_default(),
        }
    }

    fn refresh(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        let text = self.text();
        self.body.update(cx, |body, cx| {
            if body.text(cx) != text {
                body.set_text(text, window, cx);
            }
        });
        self.language_task = None;
        let buffer = self.body.read(cx).buffer().read(cx).as_singleton();
        if let Some(buffer) = buffer {
            buffer.update(cx, |buffer, cx| buffer.set_language(None, cx));
            let language = match self.section {
                Section::Headers | Section::Request => "HTTP",
                Section::Body => self
                    .responses
                    .get(self.selected)
                    .map(|response| {
                        if response.content_type.contains("json") {
                            "JSON"
                        } else if response.content_type.contains("html") {
                            "HTML"
                        } else if response.content_type.contains("xml") {
                            "XML"
                        } else {
                            "Plain Text"
                        }
                    })
                    .unwrap_or("Plain Text"),
                _ => "Plain Text",
            };
            if let Some(languages) = self.languages.clone() {
                self.language_task = Some(cx.spawn(async move |_, cx| {
                    if let Ok(language) = languages.language_for_name(language).await {
                        buffer.update(cx, |buffer, cx| buffer.set_language(Some(language), cx));
                    }
                }));
            }
        }
        cx.notify();
    }

    fn refresh_stream(
        &mut self,
        discarded: usize,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let text = self.text();
        self.body.update(cx, |body, cx| {
            let snapshot = body.snapshot(window, cx);
            let display = &snapshot.display_snapshot;
            let old_len = display.buffer_snapshot().len().0;
            let old_trailing_newline = display
                .buffer_snapshot()
                .reversed_chars_at(editor::MultiBufferOffset(old_len))
                .next()
                == Some('\n');
            let position = snapshot.scroll_position();
            let visible_lines = body.visible_line_count().map(|lines| lines.floor().max(1.));
            let bottom = (f64::from(display.max_point().row().0) + 1.
                - f64::from(old_trailing_newline)
                - visible_lines.unwrap_or(0.))
            .max(0.);
            let mut selections = body.selections.all::<editor::MultiBufferOffset>(display);
            let follow = visible_lines.is_some()
                && position.y >= bottom - 0.5
                && selections.iter().all(|selection| selection.is_empty());
            let top_offset = editor::DisplayPoint::new(
                editor::display_map::DisplayRow(
                    (position.y.floor() as u32).min(display.max_point().row().0),
                ),
                0,
            )
            .to_offset(display, language::Bias::Left)
            .0;
            let retained = old_len.saturating_sub(discarded);
            let Some(appended) = text.get(retained..) else {
                log::error!("HTTP stream update does not match the displayed buffer");
                return;
            };
            let Some(buffer) = body.buffer().read(cx).as_singleton() else {
                log::error!("HTTP stream editor must have a singleton buffer");
                return;
            };
            // Keep retained fragments alive so reading positions survive incoming messages.
            buffer.update(cx, |buffer, cx| {
                buffer.edit(
                    [
                        (0..discarded.min(old_len), ""),
                        (old_len..old_len, appended),
                    ],
                    None,
                    cx,
                );
            });
            for selection in &mut selections {
                selection.start.0 = selection.start.0.saturating_sub(discarded);
                selection.end.0 = selection.end.0.saturating_sub(discarded);
            }
            body.change_selections(
                editor::SelectionEffects::no_scroll(),
                window,
                cx,
                |current| {
                    current.select(selections);
                },
            );
            let y = if follow {
                // The final newline is a separator, not a message to put at the top of the view.
                Some(
                    (f64::from(body.max_point(cx).row().0) + 1.
                        - f64::from(text.ends_with('\n'))
                        - visible_lines.unwrap_or(0.))
                    .max(0.),
                )
            } else if top_offset < discarded {
                Some(0.)
            } else {
                None
            };
            if let Some(y) = y {
                body.set_scroll_position(gpui::point(position.x, y), window, cx);
            }
        });
    }

    fn event(&mut self, event: Event, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        let mut refresh = false;
        match event {
            Event::Started { streaming } => {
                if std::mem::take(&mut self.select_initial_section) && streaming {
                    self.section = Section::Log;
                    refresh = true;
                }
            }
            Event::Response(response) => {
                if self.responses.len() == MAX_RESULTS {
                    self.responses.pop_front();
                }
                self.responses.push_back(response);
                self.selected = self.responses.len().saturating_sub(1);
                refresh = true;
            }
            Event::Output { stream, text } => {
                let discarded = append_log(&mut self.logs, &format!("[{stream}] {text}"));
                if self.section == Section::Log {
                    self.refresh_stream(discarded, window, cx);
                }
            }
            Event::Test { status, message } => {
                let discarded = append_log(&mut self.tests, &format!("{status}: {message}\n"));
                if self.section == Section::Tests {
                    self.refresh_stream(discarded, window, cx);
                }
            }
            Event::Error { message } => {
                append_log(&mut self.logs, &format!("{message}\n"));
                self.status = "Failed".into();
                self.section = Section::Log;
                refresh = true;
            }
            Event::Prompt(prompt) => {
                self.prompt_input.update(cx, |input, cx| {
                    input.set_masked(prompt.kind == "password", cx);
                    input.set_text(
                        prompt.default_value.as_str().unwrap_or_default(),
                        window,
                        cx,
                    );
                });
                self.prompt = Some(prompt);
            }
            Event::Done { code } => {
                self.status = if code == 0 {
                    "Finished"
                } else {
                    "Failed — see Tests / Log"
                }
                .into()
            }
        }
        if refresh {
            self.refresh(window, cx);
        } else {
            cx.notify();
        }
    }

    fn finish(&mut self, status: String, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        self.status = status;
        self.select_initial_section = false;
        self.cancel = None;
        self.controls = None;
        self.prompt = None;
        self.prompt_input
            .update(cx, |input, cx| input.set_text("", window, cx));
        if self.status.starts_with("HTTP execution failed:") {
            append_log(&mut self.logs, &format!("{}\n", self.status));
            self.section = Section::Log;
            self.refresh(window, cx);
        } else {
            cx.notify();
        }
    }

    fn answer(
        &mut self,
        value: serde_json::Value,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if let Some(prompt) = self.prompt.take()
            && let Some(controls) = &self.controls
            && controls
                .unbounded_send(serde_json::json!({"answer": prompt.id, "value": value}))
                .is_err()
        {
            self.status = "Execution disconnected; nothing was retried".into();
            let discarded = append_log(&mut self.logs, &format!("{}\n", self.status));
            if self.section == Section::Log {
                self.refresh_stream(discarded, window, cx);
            }
        }
        self.prompt_input
            .update(cx, |input, cx| input.set_text("", window, cx));
        cx.notify();
    }

    fn save(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        let Some(response) = self
            .responses
            .get(self.selected)
            .filter(|response| !response.truncated)
        else {
            return;
        };
        let data = match base64::engine::general_purpose::STANDARD.decode(&response.raw_body) {
            Ok(data) => data,
            Err(error) => {
                self.status = format!("Cannot export response: {error}");
                let discarded = append_log(&mut self.logs, &format!("{}\n", self.status));
                if self.section == Section::Log {
                    self.refresh_stream(discarded, window, cx);
                }
                cx.notify();
                return;
            }
        };
        let path = cx.prompt_for_new_path(std::path::Path::new(""), Some("response.body"));
        cx.spawn_in(window, async move |view, cx| {
            let result: anyhow::Result<()> = async {
                if let Some(path) = path.await?? {
                    cx.background_spawn(async move { std::fs::write(path, data) })
                        .await?;
                }
                Ok(())
            }
            .await;
            if let Err(error) = result {
                view.update_in(cx, |view, window, cx| {
                    view.status = format!("Export failed: {error:#}");
                    let discarded = append_log(&mut view.logs, &format!("{}\n", view.status));
                    if view.section == Section::Log {
                        view.refresh_stream(discarded, window, cx);
                    }
                    cx.notify();
                })
                .log_err();
            }
        })
        .detach();
    }
}

impl gpui::Render for ResponseView {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        let mut registrar = search::buffer_search::DivRegistrar::new(
            |view: &Self, _, _| Some(view.search.clone()),
            cx,
        );
        search::BufferSearchBar::register(&mut registrar);
        let container = registrar.into_div();
        let source = self.source.clone();
        let header = ui::h_flex()
            .gap_1()
            .p_2()
            .flex_shrink_0()
            .child(
                gpui::div()
                    .id("http-response-summary")
                    .tooltip(ui::Tooltip::text(self.status.clone()))
                    .child(ui::Label::new(self.status_summary())),
            )
            .child(gpui::div().flex_1())
            .children(self.controls.as_ref().map(|_| {
                ui::Button::new("cancel", "Cancel")
                    .disabled(self.cancel.is_none())
                    .on_click(cx.listener(|view, _, _, cx| {
                        if let Some(cancel) = view.cancel.take()
                            && cancel.send(()).is_err()
                        {
                            log::debug!("HTTP execution already finished");
                        }
                        view.status =
                            "Cancelling — the request may already have reached the server".into();
                        cx.notify();
                    }))
            }))
            .child(
                ui::Button::new("hide", "Hide").on_click(move |_, window, cx| {
                    source
                        .update(cx, |source, cx| {
                            if let Some(addon) = source.addon_mut::<EmbeddedResponse>() {
                                addon.visible = false;
                            }
                            source.focus_handle(cx).focus(window, cx);
                            cx.notify();
                        })
                        .log_err();
                }),
            );
        let requests = ui::h_flex().gap_1().px_2().flex_shrink_0().children(
            self.responses.iter().enumerate().map(|(index, response)| {
                let name = if self
                    .responses
                    .iter()
                    .filter(|other| other.name == response.name)
                    .count()
                    > 1
                {
                    format!("{} ({})", response.name, index + 1)
                } else {
                    response.name.clone()
                };
                ui::Button::new(("http-result", index), name)
                    .style(if self.selected == index {
                        ui::ButtonStyle::Filled
                    } else {
                        ui::ButtonStyle::Subtle
                    })
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.selected = index;
                        view.refresh(window, cx);
                    }))
            }),
        );
        let sections = ui::h_flex().gap_1().children(
            [
                (Section::Body, false, "Raw"),
                (Section::Body, true, "Pretty"),
                (Section::Headers, false, "Headers"),
                (Section::Request, false, "Request"),
                (Section::Tests, false, "Tests"),
                (Section::Log, false, "Log"),
            ]
            .into_iter()
            .map(|(section, pretty, label)| {
                ui::Button::new(label, label)
                    .style(
                        if self.section == section
                            && (section != Section::Body || self.pretty == pretty)
                        {
                            ui::ButtonStyle::Filled
                        } else {
                            ui::ButtonStyle::Subtle
                        },
                    )
                    .tooltip(ui::Tooltip::text(match section {
                        Section::Tests => "Assertions for the entire run",
                        Section::Log => "Logs for the entire run",
                        _ => label,
                    }))
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.select_initial_section = false;
                        view.section = section;
                        if section == Section::Body {
                            view.pretty = pretty;
                        }
                        view.refresh(window, cx);
                    }))
            }),
        );
        let actions = ui::h_flex()
            .gap_1()
            .pl_2()
            .border_l_1()
            .border_color(cx.theme().colors().border_variant)
            .flex_shrink_0()
            .child(
                ui::Button::new("copy", "Copy")
                    .tooltip(ui::Tooltip::text("Copy all text in the current view"))
                    .on_click(cx.listener(|view, _, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(view.text()));
                    })),
            )
            .child(
                ui::Button::new("save", "Save")
                    .tooltip(ui::Tooltip::text("Save response body bytes"))
                    .disabled(
                        self.responses
                            .get(self.selected)
                            .is_none_or(|response| response.truncated),
                    )
                    .on_click(cx.listener(|view, _, window, cx| view.save(window, cx))),
            )
            .child(
                ui::Button::new("find", "Find").on_click(cx.listener(|view, _, window, cx| {
                    view.search.update(cx, |search, cx| {
                        search.deploy(&search::buffer_search::Deploy::find(), None, window, cx);
                    });
                })),
            );
        let mut content = container
            .id("http-response")
            .key_context("HttpResponse")
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .bg(cx.theme().colors().editor_background)
            .child(header)
            .child(requests)
            .child(
                ui::h_flex()
                    .gap_2()
                    .p_2()
                    .flex_shrink_0()
                    .child(sections)
                    .child(gpui::div().flex_1())
                    .child(actions),
            );
        if let Some(prompt) = &self.prompt {
            let mut panel = ui::v_flex()
                .p_2()
                .gap_1()
                .child(ui::Label::new(prompt.message.clone()));
            if prompt.kind == "confirm" {
                panel = panel.child(
                    ui::h_flex()
                        .gap_1()
                        .child(ui::Button::new("yes", "Yes").on_click(
                            cx.listener(|view, _, window, cx| view.answer(true.into(), window, cx)),
                        ))
                        .child(ui::Button::new("no", "No").on_click(cx.listener(
                            |view, _, window, cx| view.answer(false.into(), window, cx),
                        ))),
                );
            } else if prompt.kind == "list" {
                panel = panel.child(
                    gpui::div()
                        .id("http-prompt-choices")
                        .max_h(gpui::px(180.))
                        .overflow_y_scroll()
                        .children(prompt.choices.iter().enumerate().map(|(index, choice)| {
                            let value = choice
                                .get("value")
                                .cloned()
                                .unwrap_or_else(|| choice.clone());
                            let label = choice
                                .as_str()
                                .or_else(|| choice.get("name").and_then(|name| name.as_str()))
                                .unwrap_or("Choice")
                                .to_owned();
                            ui::Button::new(("choice", index), label).on_click(cx.listener(
                                move |view, _, window, cx| view.answer(value.clone(), window, cx),
                            ))
                        })),
                );
            } else {
                panel = panel.child(self.prompt_input.clone()).child(
                    ui::Button::new("answer", "Submit").on_click(cx.listener(
                        |view, _, window, cx| {
                            view.answer(view.prompt_input.read(cx).text(cx).into(), window, cx)
                        },
                    )),
                );
            }
            content = content.child(panel);
        }
        if !self.search.read(cx).is_dismissed() {
            content = content.child(self.search.clone());
        }
        content.child(gpui::div().flex_1().min_h_0().child(self.body.clone()))
    }
}

fn pretty_json(text: &str) -> Option<String> {
    // Formatting through Value would round large numbers and discard duplicate keys.
    serde_json::from_str::<Box<serde_json::value::RawValue>>(text).ok()?;
    let mut result = String::with_capacity(text.len());
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    let mut previous = None;
    for character in text.chars() {
        if quoted {
            result.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
                previous = Some('"');
            }
            continue;
        }
        if character.is_ascii_whitespace() {
            continue;
        }
        if matches!(previous, Some('{' | '[')) && !matches!(character, '}' | ']') {
            result.push('\n');
            result.push_str(&"  ".repeat(depth));
        }
        match character {
            '"' => {
                quoted = true;
                result.push(character);
            }
            '{' | '[' => {
                result.push(character);
                depth += 1;
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                if !matches!(previous, Some('{' | '[')) {
                    result.push('\n');
                    result.push_str(&"  ".repeat(depth));
                }
                result.push(character);
            }
            ',' => {
                result.push_str(",\n");
                result.push_str(&"  ".repeat(depth));
            }
            ':' => result.push_str(": "),
            _ => result.push(character),
        }
        previous = Some(character);
    }
    Some(result)
}

fn append_log(log: &mut String, text: &str) -> usize {
    // Match the editor's normalization so byte offsets also agree for CRLF output.
    let text = language::LineEnding::normalize_cow(std::borrow::Cow::Borrowed(text));
    log.push_str(&text);
    if log.len() > MAX_LOG {
        let mut start = log.len() - MAX_LOG;
        while !log.is_char_boundary(start) {
            start += 1;
        }
        log.drain(..start);
        start
    } else {
        0
    }
}

async fn execute(
    source: gpui::WeakEntity<editor::Editor>,
    view: gpui::WeakEntity<ResponseView>,
    project: gpui::Entity<project::Project>,
    task: task::SpawnInTerminal,
    workspace: gpui::WeakEntity<workspace::Workspace>,
    cx: &mut gpui::AsyncWindowContext,
) -> anyhow::Result<workspace::tasks::ScheduledTaskResult> {
    let (cancel_tx, cancel_rx) = futures::channel::oneshot::channel();
    let (controls_tx, controls_rx) = futures::channel::mpsc::unbounded();
    let accepted = view.update_in(cx, |view, window, cx| {
        if view.controls.is_some() {
            return false;
        }
        view.cancel = Some(cancel_tx);
        view.controls = Some(controls_tx);
        view.responses.clear();
        view.logs.clear();
        view.tests.clear();
        view.selected = 0;
        view.status = "Preparing request…".into();
        view.pretty = true;
        view.section = match task.args.last().map(String::as_str) {
            Some("headers") => Section::Headers,
            Some("exchange") => Section::Request,
            _ => Section::Body,
        };
        view.select_initial_section = view.section == Section::Body;
        view.refresh(window, cx);
        true
    })?;
    if !accepted {
        return Ok(workspace::tasks::ScheduledTaskResult::Cancelled);
    }
    let execution = async {
        let mut environment = project.read_with(cx, |project, cx| {
            project.terminal_settings(&task.cwd, cx).env.clone()
        });
        environment.extend(task.env.clone());
        anyhow::ensure!(
            environment.contains_key("ZED_CUSTOM_HTTPYAC_EXECUTOR"),
            "Missing HTTP executor; run this task from an HTTP file"
        );
        let operation = if task.args.iter().any(|arg| arg == "reset") {
            "reset"
        } else {
            "send"
        };
        if operation == "send" {
            source.read_with(cx, |source, cx| -> anyhow::Result<()> {
                let buffer = source
                    .buffer()
                    .read(cx)
                    .as_singleton()
                    .context("HTTP requests require a single source file")?;
                let buffer = buffer.read(cx);
                anyhow::ensure!(
                    buffer
                        .language()
                        .is_some_and(|language| language.name().as_ref() == "HTTP"),
                    "The source tab is not an HTTP file"
                );
                let file = buffer.file().context("Save the HTTP file before sending")?;
                let actual = project
                    .read(cx)
                    .absolute_path(
                        &project::ProjectPath {
                            worktree_id: file.worktree_id(cx),
                            path: file.path().clone(),
                        },
                        cx,
                    )
                    .context("HTTP worktree is no longer available")?
                    .to_string_lossy()
                    .into_owned();
                let expected = task.env.get("ZED_FILE").context("Missing request file")?;
                anyhow::ensure!(
                    actual == *expected,
                    "Task source changed; send again from the original HTTP tab"
                );
                Ok(())
            })??;
        }
        if matches!(task.save, task::SaveStrategy::All) {
            workspace::Workspace::save_for_task(&workspace, task.save, cx).await;
        } else if matches!(task.save, task::SaveStrategy::Current) {
            source
                .update_in(cx, |source, window, cx| {
                    source.save(
                        workspace::item::SaveOptions {
                            format: false,
                            ..Default::default()
                        },
                        project.clone(),
                        window,
                        cx,
                    )
                })?
                .await?;
        }
        let mut args = vec![
            "-e".into(),
            "require(process.env.ZED_CUSTOM_HTTPYAC_EXECUTOR)".into(),
            "--".into(),
            operation.into(),
            "--json".into(),
        ];
        if task.args.iter().any(|arg| arg == "--all") {
            args.push("--all".into());
        }
        let command = project.read_with(
            cx,
            |project, cx| -> anyhow::Result<util::command::Command> {
                anyhow::ensure!(
                    !project.is_via_collab(),
                    "HTTP execution is unavailable in a guest collaboration project"
                );
                if let Some(remote) = project.remote_client() {
                    let template = remote.read(cx).build_command(
                        Some("node".into()),
                        &args,
                        &environment,
                        task.cwd
                            .as_ref()
                            .map(|cwd| cwd.to_string_lossy().into_owned()),
                        None,
                        remote::Interactive::No,
                    )?;
                    let mut command = util::command::new_command(template.program);
                    command.args(template.args).envs(template.env);
                    Ok(command)
                } else {
                    let mut command = util::command::new_command("node");
                    command.args(args).envs(environment);
                    if let Some(cwd) = task.cwd {
                        command.current_dir(cwd);
                    }
                    Ok(command)
                }
            },
        )?;
        view.update(cx, |view, cx| {
            view.status = "Running…".into();
            cx.notify();
        })?;
        let (events_tx, mut events_rx) = futures::channel::mpsc::channel(16);
        let executor = cx.background_executor().clone();
        let process = cx.background_spawn(run_process(command, controls_rx, events_tx, executor));
        let mut code = None;
        while let Some(event) = events_rx.next().await {
            if let Event::Done { code: value } = &event {
                code = Some(*value);
            }
            view.update_in(cx, |view, window, cx| view.event(event, window, cx))?;
        }
        let status = process.await?;
        anyhow::ensure!(
            code.is_some(),
            "Executor disconnected; the request may have been sent. It was NOT retried"
        );
        Ok::<_, anyhow::Error>(if status.success() && code == Some(0) {
            workspace::tasks::ScheduledTaskResult::Success
        } else {
            workspace::tasks::ScheduledTaskResult::Failure
        })
    };
    futures::pin_mut!(execution);
    let result = futures::select_biased! {
        _ = cancel_rx.fuse() => Ok(workspace::tasks::ScheduledTaskResult::Cancelled),
        result = execution.fuse() => result,
    };
    // End the borrowing execution future before updating the view through the same context.
    result
}

async fn run_process(
    mut command: util::command::Command,
    controls: futures::channel::mpsc::UnboundedReceiver<serde_json::Value>,
    events: futures::channel::mpsc::Sender<Event>,
    executor: gpui::BackgroundExecutor,
) -> anyhow::Result<std::process::ExitStatus> {
    let mut child = command
        .stdin(util::command::Stdio::piped())
        .stdout(util::command::Stdio::piped())
        .stderr(util::command::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("Starting HTTP executor (Node must be on the project PATH)")?;
    let stdin = child.stdin.take().context("Missing executor stdin")?;
    let stdout = child.stdout.take().context("Missing executor stdout")?;
    let stderr = child.stderr.take().context("Missing executor stderr")?;
    let mut output_events = events.clone();
    let stdout = async move {
        let mut reader = futures::io::BufReader::new(stdout);
        while let Some(line) = read_frame(&mut reader).await? {
            let event: Event = serde_json::from_slice(&line).context("Invalid executor event")?;
            output_events
                .send(event)
                .await
                .context("Response view closed")?;
        }
        Ok::<_, anyhow::Error>(())
    };
    let stderr = async move {
        let mut events = events;
        let mut reader = futures::io::BufReader::new(stderr);
        while let Some(line) = read_frame(&mut reader).await? {
            events
                .send(Event::Output {
                    stream: "stderr".into(),
                    text: format!("{}\n", String::from_utf8_lossy(&line)),
                })
                .await
                .context("Response view closed")?;
        }
        Ok::<_, anyhow::Error>(())
    };
    let write = write_controls(stdin, controls, executor);
    let read = async {
        futures::try_join!(stdout, stderr)?;
        Ok::<_, anyhow::Error>(child.status().await?)
    };
    futures::pin_mut!(read, write);
    futures::select_biased! {
        result = read.fuse() => result,
        result = write.fuse() => { result?; anyhow::bail!("Executor input disconnected; nothing was retried") },
    }
}

async fn write_controls(
    mut stdin: impl futures::AsyncWrite + Unpin,
    mut controls: futures::channel::mpsc::UnboundedReceiver<serde_json::Value>,
    executor: gpui::BackgroundExecutor,
) -> anyhow::Result<()> {
    loop {
        let message = futures::select_biased! {
            control = controls.next().fuse() => control.context("Response view closed")?,
            _ = executor.timer(std::time::Duration::from_secs(2)).fuse() => serde_json::json!({"type":"ping"}),
        };
        stdin.write_all(format!("{message}\n").as_bytes()).await?;
        stdin.flush().await?;
    }
}

async fn read_frame(
    reader: &mut (impl futures::AsyncBufRead + Unpin),
) -> anyhow::Result<Option<Vec<u8>>> {
    let mut result = Vec::new();
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            return Ok((!result.is_empty()).then_some(result));
        }
        let boundary = buffer.iter().position(|byte| *byte == b'\n');
        let length = boundary.map_or(buffer.len(), |index| index + 1);
        anyhow::ensure!(
            result.len() + length <= MAX_FRAME,
            "Executor event exceeded the size limit"
        );
        result.extend_from_slice(&buffer[..length]);
        reader.consume_unpin(length);
        if boundary.is_some() {
            return Ok(Some(result));
        }
    }
}
