use gpui::TaskExt as _;
use gpui::{
    AppContext as _, Focusable as _, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _,
};
use language::{ToOffset as _, ToPoint as _};
use theme::ActiveTheme as _;
use ui::{ButtonCommon as _, Clickable as _, Disableable as _, StyledExt as _};
use ui::{FluentBuilder as _, LabelCommon as _};
use util::ResultExt as _;
use workspace::ToolbarItemView as _;

#[cfg(test)]
#[path = "workbench_tests.rs"]
mod tests;

const MAX_RUNS: usize = 8;
const MAX_HISTORY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Default, PartialEq, Eq)]
pub(super) enum Environment {
    #[default]
    Inherit,
    None,
    Named(String),
}

impl Environment {
    fn label(&self) -> String {
        match self {
            Self::Inherit => "Inherit configuration".into(),
            Self::None => "No named environment".into(),
            Self::Named(name) => name.clone(),
        }
    }

    pub(super) fn apply<S: std::hash::BuildHasher>(
        &self,
        environment: &mut std::collections::HashMap<String, String, S>,
    ) {
        match self {
            Self::Inherit => {}
            Self::None => {
                environment.insert("ZED_HTTPYAC_ENV".into(), String::new());
            }
            Self::Named(name) => {
                environment.insert("ZED_HTTPYAC_ENV".into(), name.clone());
            }
        }
    }
}

struct Run {
    request: u64,
    environment: String,
    view: gpui::Entity<super::ResponseView>,
    source: String,
    source_version: String,
    origins: Vec<(u32, u64)>,
    responses: Vec<(u64, usize)>,
}

pub(super) struct Workbench {
    source: gpui::WeakEntity<editor::Editor>,
    buffer: gpui::Entity<language::Buffer>,
    project: Option<gpui::Entity<project::Project>>,
    workspace: Option<gpui::WeakEntity<workspace::Workspace>>,
    pub request_mode: bool,
    pub response_visible: bool,
    pub editor: gpui::Entity<editor::Editor>,
    search: gpui::Entity<search::BufferSearchBar>,
    filter: gpui::Entity<editor::Editor>,
    environment_input: gpui::Entity<editor::Editor>,
    editing_environment: bool,
    pub environment: Environment,
    inherited_environment: String,
    environment_names: Vec<String>,
    environment_files: Vec<project::ProjectPath>,
    default_environment_file: Option<project::ProjectPath>,
    environment_buffers: Vec<gpui::Entity<language::Buffer>>,
    environment_subscriptions: Vec<gpui::Subscription>,
    environment_task: Option<gpui::Task<()>>,
    requests: Vec<super::requests::Request>,
    context: Vec<std::ops::Range<language::Anchor>>,
    next_id: u64,
    selected: Option<u64>,
    grouping: super::requests::Grouping,
    file_order: bool,
    collapsed: std::collections::BTreeSet<Vec<String>>,
    pub response: gpui::Entity<super::ResponseView>,
    empty: gpui::Entity<super::ResponseView>,
    runs: Vec<Run>,
    pub running: Option<gpui::WeakEntity<super::ResponseView>>,
    sidebar_ratio: f32,
    editor_ratio: f32,
    message: String,
    _subscriptions: Vec<gpui::Subscription>,
}

struct RequestEditor {
    source_id: u64,
    environment_override: Option<String>,
}

impl editor::Addon for RequestEditor {
    fn extend_task_variables(&self, variables: &mut task::TaskVariables, _: &gpui::App) {
        variables.insert(
            task::VariableName::Custom("SOURCE_EDITOR".into()),
            self.source_id.to_string(),
        );
        variables.insert(
            task::VariableName::Custom("HTTPYAC_ENV_OVERRIDE".into()),
            serde_json::json!(&self.environment_override).to_string(),
        );
    }

    fn to_any(&self) -> &dyn std::any::Any {
        self
    }
    fn to_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[derive(Clone)]
struct SidebarDivider;
#[derive(Clone)]
struct ResponseDivider;

impl Workbench {
    pub(super) fn new(
        source: gpui::WeakEntity<editor::Editor>,
        buffer: gpui::Entity<language::Buffer>,
        project: Option<gpui::Entity<project::Project>>,
        workspace: Option<gpui::WeakEntity<workspace::Workspace>>,
        response: gpui::Entity<super::ResponseView>,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let capability = buffer.read(cx).capability();
        let multi_buffer = cx.new(|_| multi_buffer::MultiBuffer::without_headers(capability));
        let editor = cx.new(|cx| {
            let mut editor = editor::Editor::new(
                editor::EditorMode::full(),
                multi_buffer,
                project.clone(),
                window,
                cx,
            );
            editor.set_show_runnables(false, cx);
            editor.register_addon(RequestEditor {
                source_id: source.entity_id().as_u64(),
                environment_override: None,
            });
            editor
        });
        let languages = project
            .as_ref()
            .map(|project| project.read(cx).languages().clone());
        let search = cx.new(|cx| {
            let mut search = search::BufferSearchBar::new(languages, window, cx);
            search.set_active_pane_item(Some(&editor), window, cx);
            search
        });
        let filter = cx.new(|cx| {
            let mut editor = editor::Editor::single_line(window, cx);
            editor.set_placeholder_text("Filter requests…", window, cx);
            editor
        });
        let environment_input = cx.new(|cx| {
            let mut editor = editor::Editor::single_line(window, cx);
            editor.set_placeholder_text("Environment name (or dev,local)", window, cx);
            editor
        });
        let subscriptions = vec![
            cx.subscribe_in(&buffer, window, |this, _, event, window, cx| {
                if matches!(event, language::BufferEvent::Reparsed) {
                    this.reindex(window, cx);
                }
                if matches!(event, language::BufferEvent::Edited { .. }) {
                    this.mark_stale(cx);
                }
            }),
            cx.subscribe_in(&editor, window, |this, _, event, window, cx| {
                if matches!(event, editor::EditorEvent::SelectionsChanged { .. })
                    && this.request_mode
                {
                    this.sync_source_selection(window, cx);
                }
                if matches!(
                    event,
                    editor::EditorEvent::Focused | editor::EditorEvent::FocusedIn
                ) && this.request_mode
                {
                    let source = this.source.clone();
                    cx.defer_in(window, move |_, _, cx| {
                        source
                            .update(cx, |_, cx| cx.emit(editor::EditorEvent::Focused))
                            .log_err();
                    });
                }
            }),
            cx.observe(&filter, |_, _, cx| cx.notify()),
            cx.observe(&search, |_, _, cx| cx.notify()),
        ];
        let mut this = Self {
            source,
            buffer,
            project,
            workspace,
            request_mode: false,
            response_visible: true,
            editor,
            search,
            filter,
            environment_input,
            editing_environment: false,
            environment: Environment::Inherit,
            inherited_environment: String::new(),
            environment_names: Vec::new(),
            environment_files: Vec::new(),
            default_environment_file: None,
            environment_buffers: Vec::new(),
            environment_subscriptions: Vec::new(),
            environment_task: None,
            requests: Vec::new(),
            context: Vec::new(),
            next_id: 0,
            selected: None,
            grouping: Default::default(),
            file_order: false,
            collapsed: Default::default(),
            response: response.clone(),
            empty: response,
            runs: Vec::new(),
            running: None,
            sidebar_ratio: 0.2,
            editor_ratio: 0.5,
            message: String::new(),
            _subscriptions: subscriptions,
        };
        this.reindex(window, cx);
        this.refresh_environments(window, cx);
        this
    }

    fn reindex(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        let snapshot = self.buffer.read(cx).snapshot();
        let Some((requests, context)) =
            super::requests::index(&snapshot, &self.requests, &mut self.next_id, self.grouping)
        else {
            return;
        };
        self.requests = requests;
        self.context = context;
        if self
            .selected
            .is_some_and(|selected| !self.requests.iter().any(|request| request.id == selected))
        {
            self.selected = self.requests.first().map(|request| request.id);
            self.show_response(cx);
        }
        self.update_excerpt(window, cx);
        self.mark_stale(cx);
        cx.notify();
    }

    fn mark_stale(&mut self, cx: &mut gpui::Context<Self>) {
        let snapshot = self.buffer.read(cx).snapshot();
        let version = format!("{:?}", self.buffer.read(cx).version());
        for run in &self.runs {
            let current = self
                .requests
                .iter()
                .find(|request| request.id == run.request)
                .map(|request| {
                    snapshot
                        .text_for_range(request.range.clone())
                        .collect::<String>()
                });
            let stale = if run.request == 0 {
                snapshot.text() != run.source
            } else {
                current.as_ref() != Some(&run.source)
            };
            run.view.update(cx, |view, cx| {
                view.stale |= stale || run.source_version != version;
                cx.notify();
            });
        }
        cx.notify();
    }

    fn update_excerpt(&mut self, _window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        let snapshot = self.buffer.read(cx).snapshot();
        let ranges = self
            .selected
            .and_then(|selected| self.requests.iter().find(|request| request.id == selected))
            .map(|request| vec![request.range.clone()])
            .unwrap_or_else(|| self.context.clone());
        let ranges = ranges
            .into_iter()
            .map(|range| {
                multi_buffer::ExcerptRange::new(
                    range.start.to_point(&snapshot)..range.end.to_point(&snapshot),
                )
            })
            .collect::<Vec<_>>();
        let buffer = self.buffer.clone();
        let path = multi_buffer::PathKey::for_buffer(&buffer, cx);
        self.editor
            .read(cx)
            .buffer()
            .clone()
            .update(cx, |multi_buffer, cx| {
                // The row-expanding convenience API would include the next request's separator.
                multi_buffer.set_excerpt_ranges_for_path(path, buffer, &snapshot, ranges, cx);
            });
    }

    fn sync_source_selection(&self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        let editor = self.editor.read(cx);
        let snapshot = editor.buffer().read(cx).snapshot(cx);
        let Some((anchor, _)) =
            snapshot.anchor_to_buffer_anchor(editor.selections.newest_anchor().head())
        else {
            return;
        };
        let buffer = self.buffer.clone();
        let source = self.source.clone();
        let effects = if self.request_mode {
            editor::SelectionEffects::no_scroll()
        } else {
            editor::SelectionEffects::default()
        };
        cx.defer_in(window, move |_, window, cx| {
            source
                .update(cx, |source, cx| {
                    let point = anchor.to_point(&buffer.read(cx).snapshot());
                    source.change_selections(effects, window, cx, |selections| {
                        selections.select_ranges([point..point])
                    });
                })
                .log_err();
        });
    }

    pub(super) fn toggle_mode(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        self.request_mode = !self.request_mode;
        let focus = self.request_mode.then(|| self.editor.focus_handle(cx));
        self.source
            .update(cx, |source, cx| {
                if let Some(addon) = source.addon_mut::<super::EmbeddedResponse>() {
                    addon.request_focus = focus;
                }
                cx.notify();
            })
            .log_err();
        if self.request_mode {
            let offset = self
                .source
                .read_with(cx, |source, cx| {
                    let snapshot = source.buffer().read(cx).snapshot(cx);
                    snapshot
                        .anchor_to_buffer_anchor(source.selections.newest_anchor().head())
                        .map(|(anchor, _)| anchor.to_offset(&self.buffer.read(cx).snapshot()))
                })
                .ok()
                .flatten();
            let snapshot = self.buffer.read(cx).snapshot();
            let selected = offset
                .and_then(|offset| {
                    self.requests.iter().find(|request| {
                        request.range.start.to_offset(&snapshot) <= offset
                            && offset <= request.range.end.to_offset(&snapshot)
                    })
                })
                .map(|request| request.id);
            self.select(selected, window, cx);
            if let Some(offset) = offset {
                let anchor = snapshot.anchor_after(offset);
                self.editor.update(cx, |editor, cx| {
                    let excerpt = editor
                        .buffer()
                        .read(cx)
                        .snapshot(cx)
                        .anchor_in_excerpt(anchor);
                    if let Some(anchor) = excerpt {
                        editor.change_selections(
                            editor::SelectionEffects::default(),
                            window,
                            cx,
                            |selections| selections.select_ranges([anchor..anchor]),
                        );
                    }
                });
            }
        } else {
            self.sync_source_selection(window, cx);
            self.source
                .update(cx, |source, cx| source.focus_handle(cx).focus(window, cx))
                .log_err();
        }
        cx.notify();
    }

    fn select(
        &mut self,
        selected: Option<u64>,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        self.selected = selected;
        self.update_excerpt(window, cx);
        self.show_response(cx);
        self.editor.update(cx, |editor, cx| {
            editor.change_selections(
                editor::SelectionEffects::default(),
                window,
                cx,
                |selections| {
                    selections.select_ranges([language::Point::zero()..language::Point::zero()])
                },
            );
            editor.focus_handle(cx).focus(window, cx);
        });
        self.sync_source_selection(window, cx);
        cx.notify();
    }

    fn environment_key(&self) -> String {
        let value = match &self.environment {
            Environment::Inherit => self.inherited_environment.as_str(),
            Environment::None => "",
            Environment::Named(name) => name,
        };
        environment_key(value)
    }

    fn show_response(&mut self, cx: &mut gpui::Context<Self>) {
        let environment = self.environment_key();
        let run = self.runs.iter().rev().find(|run| {
            run.environment == environment
                && self.selected.is_some_and(|selected| {
                    run.request == selected
                        || run
                            .responses
                            .iter()
                            .any(|(request, _)| *request == selected)
                })
        });
        self.response = run
            .map(|run| run.view.clone())
            .unwrap_or_else(|| self.empty.clone());
        if run.is_none() {
            self.empty.update(cx, |view, cx| {
                view.environment = environment;
                view.status = "Not sent in this environment".into();
                cx.notify();
            });
        }
        if let Some(run) = run
            && let Some((_, index)) = run
                .responses
                .iter()
                .find(|(request, _)| Some(*request) == self.selected)
        {
            let index = *index;
            self.response.update(cx, |view, cx| {
                view.selected = index;
                view.pending_refresh = true;
                cx.notify();
            });
        }
        cx.notify();
    }

    fn select_environment(&mut self, environment: Environment, cx: &mut gpui::Context<Self>) {
        let value = match &environment {
            Environment::Inherit => None,
            Environment::None => Some(String::new()),
            Environment::Named(name) => Some(name.clone()),
        };
        self.editor.update(cx, |editor, _| {
            if let Some(addon) = editor.addon_mut::<RequestEditor>() {
                addon.environment_override = value.clone();
            }
        });
        self.source
            .update(cx, |source, _| {
                if let Some(addon) = source.addon_mut::<super::EmbeddedResponse>() {
                    addon.environment_override = value;
                }
            })
            .log_err();
        self.environment = environment;
        self.show_response(cx);
    }

    pub(super) fn preflight(&self, cx: &gpui::App) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self
                .environment_buffers
                .iter()
                .any(|buffer| buffer.read(cx).is_dirty()),
            "Save the environment configuration before sending"
        );
        Ok(())
    }

    pub(super) fn prepare_run(
        &mut self,
        task: &mut task::SpawnInTerminal,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> gpui::Entity<super::ResponseView> {
        self.inherited_environment = task.env.get("ZED_HTTPYAC_ENV").cloned().unwrap_or_default();
        if let Some(captured) = task.env.get("ZED_CUSTOM_HTTPYAC_ENV_OVERRIDE") {
            if let Ok(Some(value)) = serde_json::from_str::<Option<String>>(captured) {
                task.env.insert("ZED_HTTPYAC_ENV".into(), value);
            }
        } else {
            self.environment.apply(&mut task.env);
        }
        let environment = environment_key(
            task.env
                .get("ZED_HTTPYAC_ENV")
                .map(String::as_str)
                .unwrap_or_default(),
        );
        let snapshot = self.buffer.read(cx).snapshot();
        let row = task
            .env
            .get("ZED_ROW")
            .and_then(|row| row.parse::<u32>().ok())
            .unwrap_or(1)
            .saturating_sub(1);
        let request = if task.args.iter().any(|arg| arg == "--all" || arg == "reset") {
            None
        } else {
            self.requests.iter().find(|request| {
                request.range.start.to_point(&snapshot).row <= row
                    && row <= request.range.end.to_point(&snapshot).row
            })
        };
        let request_id = request.map(|request| request.id).unwrap_or(0);
        let source_text = request
            .map(|request| {
                snapshot
                    .text_for_range(request.range.clone())
                    .collect::<String>()
            })
            .unwrap_or_else(|| snapshot.text());
        let anchor = request.map(|request| request.anchor);
        let languages = self
            .project
            .as_ref()
            .map(|project| project.read(cx).languages().clone());
        let source = self.source.clone();
        let view = cx.new(|cx| {
            let mut view = super::ResponseView::new(source, languages, window, cx);
            view.environment = environment.clone();
            view.request_anchor = anchor;
            view
        });
        self.runs
            .retain(|run| !(run.request == request_id && run.environment == environment));
        let origins = self
            .requests
            .iter()
            .map(|request| (request.anchor.to_point(&snapshot).row + 1, request.id))
            .collect();
        self.runs.push(Run {
            request: request_id,
            environment,
            view: view.clone(),
            source: source_text,
            source_version: format!("{:?}", self.buffer.read(cx).version()),
            origins,
            responses: Vec::new(),
        });
        self.response_visible = true;
        self.running = Some(view.downgrade());
        self.trim_history(cx);
        if self.environment_key() == view.read(cx).environment
            && (!self.request_mode || self.selected == Some(request_id) || request_id == 0)
        {
            self.response = view.clone();
        }
        cx.notify();
        view
    }

    pub(super) fn finished(&mut self, cx: &mut gpui::Context<Self>) {
        if let Some(running) = self.running.take().and_then(|view| view.upgrade()) {
            if let Some(run) = self.runs.iter_mut().find(|run| run.view == running) {
                for (index, response) in running.read(cx).responses.iter().enumerate() {
                    if response.source_document
                        && let Some((_, request)) = run
                            .origins
                            .iter()
                            .find(|(row, _)| Some(*row) == response.source_line)
                    {
                        // A loop may produce several responses; select the last one for this request.
                        run.responses.retain(|(other, _)| other != request);
                        run.responses.push((*request, index));
                    }
                }
            }
        }
        if self.request_mode && self.selected.is_some() {
            self.show_response(cx);
        }
        self.trim_history(cx);
        cx.notify();
    }

    fn trim_history(&mut self, cx: &mut gpui::Context<Self>) {
        loop {
            let bytes: usize = self
                .runs
                .iter()
                .map(|run| run.view.read(cx).retained_bytes())
                .sum();
            if self.runs.len() <= MAX_RUNS && bytes <= MAX_HISTORY_BYTES {
                break;
            }
            let index = self.runs.iter().position(|run| {
                self.running
                    .as_ref()
                    .is_none_or(|running| *running != run.view.downgrade())
                    && run.view != self.response
            });
            let Some(index) = index else {
                break;
            };
            self.runs.remove(index);
        }
    }

    fn send(&mut self, all: bool, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        if self.running.is_some() {
            self.message = "A request is already running in this tab".into();
            cx.notify();
            return;
        }
        let Some(source) = self.source.upgrade() else {
            return;
        };
        if self.workspace.is_none() {
            self.workspace = source
                .read(cx)
                .workspace()
                .map(|workspace| workspace.downgrade());
        }
        let Some(workspace) = self.workspace.clone() else {
            return;
        };
        let row = if self.request_mode && !all {
            let snapshot = self.buffer.read(cx).snapshot();
            let Some(request) = self
                .selected
                .and_then(|selected| self.requests.iter().find(|request| request.id == selected))
            else {
                return;
            };
            Some(request.anchor.to_point(&snapshot).row + 1)
        } else {
            None
        };
        let context = source.update(cx, |source, cx| source.task_context(window, cx));
        let environment = self.environment.clone();
        let origin = source.entity_id().as_u64().to_string();
        self.message.clear();
        cx.spawn_in(window, async move |this, cx| {
            let result: anyhow::Result<()> = async {
                let mut context = context.await.ok_or_else(|| {
                    anyhow::anyhow!(
                        "Cannot build HTTP task context; save this file in a project first"
                    )
                })?;
                context
                    .task_variables
                    .insert(task::VariableName::Custom("SOURCE_EDITOR".into()), origin);
                if let Some(row) = row {
                    context
                        .task_variables
                        .insert(task::VariableName::Row, row.to_string());
                }
                let templates: task::TaskTemplates =
                    serde_json::from_str(include_str!("../../grammars/src/http/tasks.json"))?;
                let mut template = templates
                    .0
                    .into_iter()
                    .find(|template| {
                        template.label.starts_with(if all {
                            "HTTP: Send all requests"
                        } else {
                            "HTTP: Send request body"
                        })
                    })
                    .ok_or_else(|| anyhow::anyhow!("Missing built-in HTTP task"))?;
                let environment = match environment {
                    Environment::Inherit => None,
                    Environment::None => Some(String::new()),
                    Environment::Named(name) => Some(name),
                };
                context.task_variables.insert(
                    task::VariableName::Custom("HTTPYAC_ENV_OVERRIDE".into()),
                    serde_json::json!(environment).to_string(),
                );
                let resolved = template
                    .resolve_task("http-workbench", &context)
                    .ok_or_else(|| anyhow::anyhow!("Cannot resolve HTTP task variables"))?;
                workspace.update_in(cx, |workspace, window, cx| {
                    workspace.schedule_resolved_task(
                        project::TaskSourceKind::UserInput,
                        resolved,
                        true,
                        window,
                        cx,
                    )
                })?;
                Ok(())
            }
            .await;
            if let Err(error) = result {
                this.update(cx, |this, cx| {
                    this.message = format!("Cannot send request: {error:#}");
                    cx.notify();
                })
                .log_err();
            }
        })
        .detach();
    }

    fn refresh_environments(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let Some(file) = self.buffer.read(cx).file() else {
            return;
        };
        let worktree_id = file.worktree_id(cx);
        let mut directory = file.path().parent().map(|path| path.to_owned());
        let mut paths = Vec::new();
        while let Some(path) = directory {
            for filename in ["http-client.env.json", "http-client.private.env.json"] {
                let filename = util::rel_path::RelPath::from_unix_str(filename)
                    .expect("static environment filename");
                paths.push(project::ProjectPath {
                    worktree_id,
                    path: path.join(filename).into(),
                });
            }
            directory = path.parent().map(|path| path.to_owned());
        }
        self.default_environment_file = paths.first().cloned();
        self.environment_task = Some(cx.spawn_in(window, async move |this, cx| {
            let mut loaded = Vec::new();
            let mut errors = Vec::new();
            for path in paths {
                let result = project
                    .update(cx, |project, cx| project.open_buffer(path.clone(), cx))
                    .await;
                match result {
                    Ok(buffer) => {
                        // Opening a missing path can return a new, unsaved buffer.
                        let exists = buffer.read_with(cx, |buffer, _| {
                            buffer.file().is_some_and(|file| file.disk_state().exists())
                        });
                        if exists {
                            loaded.push((path, buffer));
                        }
                    }
                    Err(error) => {
                        if !error.to_string().to_lowercase().contains("not found")
                            && !error.to_string().to_lowercase().contains("no such file")
                        {
                            errors.push(error.to_string());
                        }
                    }
                }
            }
            this.update_in(cx, |this, window, cx| {
                this.environment_files = loaded.iter().map(|(path, _)| path.clone()).collect();
                this.environment_buffers = loaded.into_iter().map(|(_, buffer)| buffer).collect();
                this.environment_subscriptions = this
                    .environment_buffers
                    .iter()
                    .map(|buffer| {
                        cx.subscribe_in(buffer, window, |this, _, event, _, cx| {
                            if matches!(
                                event,
                                language::BufferEvent::Edited { .. }
                                    | language::BufferEvent::Reloaded
                            ) {
                                this.read_environments(cx);
                                for run in &this.runs {
                                    run.view.update(cx, |view, cx| {
                                        view.stale = true;
                                        cx.notify();
                                    });
                                }
                            }
                        })
                    })
                    .collect();
                this.read_environments(cx);
                if !errors.is_empty() {
                    this.message = format!("Environment discovery: {}", errors.join("; "));
                }
                cx.notify();
                let _ = window;
            })
            .log_err();
        }));
    }

    fn read_environments(&mut self, cx: &mut gpui::Context<Self>) {
        if self.message.starts_with("Invalid environment JSON:") {
            self.message.clear();
        }
        let mut names = std::collections::BTreeSet::new();
        for buffer in &self.environment_buffers {
            let text = buffer.read(cx).text();
            match super::requests::environments(&text) {
                Ok(values) => names.extend(values),
                Err(error) => self.message = format!("Invalid environment JSON: {error}"),
            }
        }
        self.environment_names = names.into_iter().collect();
        self.environment_names
            .sort_by(|a, b| super::requests::natural_cmp(a, b));
        cx.notify();
    }

    fn open_environment(
        &mut self,
        path: project::ProjectPath,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if let Some(workspace) = &self.workspace {
            workspace
                .update(cx, |workspace, cx| {
                    workspace
                        .open_path(path, None, true, window, cx)
                        .detach_and_log_err(cx)
                })
                .log_err();
        }
    }

    fn toolbar(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> gpui::AnyElement {
        let weak = cx.weak_entity();
        let environment_menu = ui::ContextMenu::build(window, cx, |menu, _, _| {
            let mut menu = menu;
            for environment in std::iter::once(Environment::Inherit)
                .chain(std::iter::once(Environment::None))
                .chain(
                    self.environment_names
                        .iter()
                        .cloned()
                        .map(Environment::Named),
                )
            {
                let weak = weak.clone();
                let label = environment.label();
                menu = menu.entry(label, None, move |_, cx| {
                    weak.update(cx, |this, cx| {
                        this.select_environment(environment.clone(), cx)
                    })
                    .log_err();
                });
            }
            let custom = weak.clone();
            let refresh = weak.clone();
            menu = menu
                .separator()
                .entry("Enter environment name…", None, move |window, cx| {
                    custom
                        .update(cx, |this, cx| {
                            this.editing_environment = true;
                            this.environment_input.focus_handle(cx).focus(window, cx);
                            cx.notify();
                        })
                        .log_err();
                })
                .entry("Refresh environments", None, move |window, cx| {
                    refresh
                        .update(cx, |this, cx| this.refresh_environments(window, cx))
                        .log_err();
                });
            let paths = self.environment_files.iter().cloned().chain(
                self.default_environment_file
                    .iter()
                    .filter(|path| !self.environment_files.contains(path))
                    .cloned(),
            );
            for path in paths {
                let path = path.clone();
                let weak = weak.clone();
                menu = menu.entry(format!("Open {}", path.path), None, move |window, cx| {
                    weak.update(cx, |this, cx| {
                        this.open_environment(path.clone(), window, cx)
                    })
                    .log_err();
                });
            }
            menu
        });
        let weak_options = cx.weak_entity();
        let options = ui::ContextMenu::build(window, cx, |menu, _, _| {
            let mut menu = menu;
            for (label, grouping) in [
                ("Group by __", super::requests::Grouping::DoubleUnderscore),
                (
                    "Group by first _",
                    super::requests::Grouping::FirstUnderscore,
                ),
                (
                    "Explicit groups only",
                    super::requests::Grouping::ExplicitOnly,
                ),
            ] {
                let weak = weak_options.clone();
                menu = menu.entry(label, None, move |window, cx| {
                    weak.update(cx, |this, cx| {
                        this.grouping = grouping;
                        this.reindex(window, cx);
                    })
                    .log_err();
                });
            }
            for (label, file_order) in [("Natural name order", false), ("File order", true)] {
                let weak = weak_options.clone();
                menu = menu.entry(label, None, move |_, cx| {
                    weak.update(cx, |this, cx| {
                        this.file_order = file_order;
                        cx.notify();
                    })
                    .log_err();
                });
            }
            menu
        });
        let mut toolbar = ui::h_flex()
            .gap_1()
            .p_1()
            .flex_shrink_0()
            .child(
                ui::Button::new("http-text", "Text")
                    .style(if self.request_mode {
                        ui::ButtonStyle::Subtle
                    } else {
                        ui::ButtonStyle::Filled
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.request_mode {
                            this.toggle_mode(window, cx);
                        }
                    })),
            )
            .child(
                ui::Button::new("http-requests", "Requests")
                    .style(if self.request_mode {
                        ui::ButtonStyle::Filled
                    } else {
                        ui::ButtonStyle::Subtle
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        if !this.request_mode {
                            this.toggle_mode(window, cx);
                        }
                    })),
            )
            .child(ui::DropdownMenu::new(
                "http-environment",
                format!("Env: {}", self.environment.label()),
                environment_menu,
            ))
            .child(ui::DropdownMenu::new("http-view-options", "View", options))
            .child(
                ui::Button::new("http-toggle-response", "Response").on_click(cx.listener(
                    |this, _, _, cx| {
                        if this.request_mode {
                            this.response_visible = !this.response_visible;
                        } else {
                            this.source
                                .update(cx, |source, cx| {
                                    if let Some(addon) =
                                        source.addon_mut::<super::EmbeddedResponse>()
                                    {
                                        addon.visible = !addon.visible;
                                    }
                                    cx.notify();
                                })
                                .log_err();
                        }
                        cx.notify();
                    },
                )),
            )
            .child(gpui::div().flex_1())
            .child(
                ui::Button::new("http-send", "Send")
                    .disabled(
                        self.running.is_some() || (self.request_mode && self.selected.is_none()),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.send(false, window, cx))),
            )
            .child(
                ui::Button::new("http-send-all", "Send All")
                    .disabled(self.running.is_some())
                    .tooltip(ui::Tooltip::text(
                        "Execute in file / httpyac dependency order, not list order",
                    ))
                    .on_click(cx.listener(|this, _, window, cx| this.send(true, window, cx))),
            );
        if let Some(running) = self.running.clone() {
            toolbar = toolbar.child(ui::Button::new("http-cancel-run", "Cancel").on_click(
                move |_, _, cx| {
                    running
                        .update(cx, |view, cx| {
                            if let Some(cancel) = view.cancel.take()
                                && cancel.send(()).is_err()
                            {
                                log::debug!("HTTP execution already finished");
                            }
                            view.status =
                                "Cancelling — the request may already have reached the server"
                                    .into();
                            cx.notify();
                        })
                        .log_err();
                },
            ));
        }
        let mut header = ui::v_flex().flex_shrink_0().child(toolbar);
        if self.editing_environment {
            header =
                header.child(
                    ui::h_flex()
                        .gap_1()
                        .p_1()
                        .child(gpui::div().flex_1().child(self.environment_input.clone()))
                        .child(ui::Button::new("http-use-environment", "Use").on_click(
                            cx.listener(|this, _, _, cx| {
                                let value =
                                    this.environment_input.read(cx).text(cx).trim().to_owned();
                                this.select_environment(
                                    if value.is_empty() {
                                        Environment::None
                                    } else {
                                        Environment::Named(value)
                                    },
                                    cx,
                                );
                                this.editing_environment = false;
                                cx.notify();
                            }),
                        )),
                );
        }
        if !self.message.is_empty() {
            header = header.child(ui::Label::new(self.message.clone()));
        }
        header.into_any_element()
    }

    fn request_list(&self, cx: &mut gpui::Context<Self>) -> gpui::AnyElement {
        let filter = self.filter.read(cx).text(cx).to_lowercase();
        let rows =
            super::requests::list_rows(&self.requests, &filter, self.file_order, &self.collapsed);
        let mut list = ui::v_flex()
            .id("http-request-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                ui::Button::new("http-file-context", "File context")
                    .style(if self.selected.is_none() {
                        ui::ButtonStyle::Filled
                    } else {
                        ui::ButtonStyle::Subtle
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.select(None, window, cx))),
            );
        for row in rows {
            match row {
                super::requests::ListRow::Group { path, label, depth } => {
                    let label = format!(
                        "{} {}",
                        if self.collapsed.contains(&path) && filter.is_empty() {
                            "▸"
                        } else {
                            "▾"
                        },
                        label
                    );
                    list = list.child(
                        gpui::div().pl(gpui::px(depth as f32 * 12.)).child(
                            ui::Button::new(
                                gpui::SharedString::from(format!("http-group-{:?}", path)),
                                label,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if !this.collapsed.remove(&path) {
                                        this.collapsed.insert(path.clone());
                                    }
                                    cx.notify();
                                },
                            )),
                        ),
                    );
                }
                super::requests::ListRow::Request { index, depth } => {
                    let Some(request) = self.requests.get(index) else {
                        continue;
                    };
                    let id = request.id;
                    let tooltip = format!("{}\n{} {}", request.name, request.method, request.url);
                    let color = match request.method.as_str() {
                        "GET" | "HEAD" => ui::Color::Success,
                        "POST" => ui::Color::Warning,
                        "DELETE" => ui::Color::Error,
                        _ => ui::Color::Info,
                    };
                    list = list.child(
                        gpui::div()
                            .id(("http-request", id))
                            .pl(gpui::px(depth as f32 * 12.))
                            .pr_1()
                            .py_1()
                            .cursor_pointer()
                            .when(self.selected == Some(id), |row| {
                                row.bg(cx.theme().colors().element_selected)
                            })
                            .tooltip(ui::Tooltip::text(tooltip))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.select(Some(id), window, cx)
                            }))
                            .child(
                                ui::h_flex()
                                    .gap_1()
                                    .child(ui::Label::new(request.method.clone()).color(color))
                                    .child(ui::Label::new(request.title.clone()).truncate()),
                            )
                            .child(
                                ui::Label::new(request.url.clone())
                                    .color(ui::Color::Muted)
                                    .size(ui::LabelSize::Small)
                                    .truncate(),
                            ),
                    );
                }
            }
        }
        if self.requests.is_empty() {
            list = list.child(ui::Label::new(
                "No requests yet. Edit the file in Text view.",
            ));
        }
        ui::v_flex()
            .size_full()
            .min_h_0()
            .child(gpui::div().p_1().child(self.filter.clone()))
            .child(list)
            .into_any_element()
    }
}

pub(super) fn environment_key(value: &str) -> String {
    let names = if value.starts_with('[') {
        serde_json::from_str::<Vec<String>>(value).ok()
    } else {
        Some(
            value
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect(),
        )
    };
    names
        .and_then(|names| serde_json::to_string(&names).ok())
        .unwrap_or_else(|| value.to_owned())
}

impl gpui::Render for Workbench {
    fn render(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        let toolbar = self.toolbar(window, cx);
        if !self.request_mode {
            return toolbar;
        }
        let list = self.request_list(cx);
        let mut registrar = search::buffer_search::DivRegistrar::new(
            |view: &Self, _, _| Some(view.search.clone()),
            cx,
        );
        search::BufferSearchBar::register(&mut registrar);
        let mut request_editor = registrar
            .into_div()
            .flex()
            .flex_col()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .flex_basis(gpui::DefiniteLength::Fraction(if self.response_visible {
                self.editor_ratio
            } else {
                1.0
            }));
        if !self.search.read(cx).is_dismissed() {
            request_editor = request_editor.child(self.search.clone());
        }
        request_editor =
            request_editor.child(gpui::div().flex_1().min_h_0().child(self.editor.clone()));
        let right = ui::v_flex()
            .h_full()
            .min_w_0()
            .min_h_0()
            .flex_basis(gpui::DefiniteLength::Fraction(1.0 - self.sidebar_ratio))
            .overflow_hidden()
            .on_drag_move::<ResponseDivider>(cx.listener(
                |this, event: &gpui::DragMoveEvent<ResponseDivider>, _, cx| {
                    if event.bounds.size.height > gpui::px(0.) {
                        this.editor_ratio = ((event.event.position.y - event.bounds.top())
                            / event.bounds.size.height)
                            .clamp(0.15, 0.85);
                        cx.notify();
                    }
                },
            ))
            .child(request_editor)
            .when(self.response_visible, |right| {
                right
                    .child(
                        gpui::div()
                            .id("http-horizontal-divider")
                            .h(gpui::px(5.))
                            .w_full()
                            .flex_shrink_0()
                            .cursor_row_resize()
                            .bg(cx.theme().colors().border_variant)
                            .on_drag(ResponseDivider, |_, _, _, cx| cx.new(|_| gpui::Empty))
                            .on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                                if event.click_count() == 2 {
                                    this.editor_ratio = 0.5;
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        gpui::div()
                            .min_h_0()
                            .min_w_0()
                            .flex_basis(gpui::DefiniteLength::Fraction(1.0 - self.editor_ratio))
                            .overflow_hidden()
                            .child(self.response.clone()),
                    )
            });
        ui::v_flex()
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .child(toolbar)
            .child(
                ui::h_flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .on_drag_move::<SidebarDivider>(cx.listener(
                        |this, event: &gpui::DragMoveEvent<SidebarDivider>, _, cx| {
                            if event.bounds.size.width > gpui::px(0.) {
                                this.sidebar_ratio = ((event.event.position.x
                                    - event.bounds.left())
                                    / event.bounds.size.width)
                                    .clamp(0.12, 0.45);
                                cx.notify();
                            }
                        },
                    ))
                    .child(
                        gpui::div()
                            .h_full()
                            .min_w(gpui::px(140.))
                            .flex_basis(gpui::DefiniteLength::Fraction(self.sidebar_ratio))
                            .overflow_hidden()
                            .child(list),
                    )
                    .child(
                        gpui::div()
                            .id("http-sidebar-divider")
                            .w(gpui::px(5.))
                            .h_full()
                            .flex_shrink_0()
                            .cursor_col_resize()
                            .bg(cx.theme().colors().border_variant)
                            .on_drag(SidebarDivider, |_, _, _, cx| cx.new(|_| gpui::Empty))
                            .on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                                if event.click_count() == 2 {
                                    this.sidebar_ratio = 0.2;
                                    cx.notify();
                                }
                            })),
                    )
                    .child(right),
            )
            .into_any_element()
    }
}
