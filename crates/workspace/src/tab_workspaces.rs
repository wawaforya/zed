use futures::FutureExt as _;
use gpui::{
    AppContext as _, Focusable as _, InteractiveElement as _, IntoElement as _, ParentElement as _,
    Styled as _,
};
use ui::{ButtonCommon as _, Clickable as _, Disableable as _, LabelCommon as _, StyledExt as _};
use util::ResultExt as _;

#[cfg(test)]
#[path = "tab_workspaces_tests.rs"]
mod tests;

gpui::actions!(workspace, [NewTabWorkspace]);

type WriteResult = Result<(), std::sync::Arc<str>>;
type PendingWrite = futures::future::Shared<gpui::Task<WriteResult>>;
type LoadedTab = (
    Option<project::ProjectEntryId>,
    project::ProjectPath,
    crate::WorkspaceItemBuilder,
);

#[derive(Default)]
pub(crate) struct State {
    root: Option<std::path::PathBuf>,
    document: Document,
    writer: Option<Writer>,
    loading: bool,
    busy: bool,
    save_failed: bool,
    save_task: Option<gpui::Task<()>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Document {
    version: u32,
    #[serde(default)]
    active_workspace_id: Option<String>,
    workspaces: Vec<TabWorkspace>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            version: 1,
            active_workspace_id: None,
            workspaces: Vec::new(),
        }
    }
}

impl Document {
    fn parse(text: &str) -> anyhow::Result<Self> {
        let document: Self = serde_json::from_str(text)?;
        anyhow::ensure!(
            document.version == 1,
            "Unsupported tab workspace version {}",
            document.version
        );
        let mut ids = std::collections::HashSet::new();
        let mut names = std::collections::HashSet::new();
        for workspace in &document.workspaces {
            anyhow::ensure!(
                !workspace.id.is_empty() && ids.insert(&workspace.id),
                "Duplicate or empty tab workspace ID"
            );
            anyhow::ensure!(
                !workspace.name.trim().is_empty() && names.insert(&workspace.name),
                "Duplicate or empty tab workspace name"
            );
            workspace.layout.validate(0)?;
        }
        Ok(document)
    }

    fn active_mut(&mut self) -> Option<&mut TabWorkspace> {
        let id = self.active_workspace_id.as_ref()?;
        self.workspaces
            .iter_mut()
            .find(|workspace| &workspace.id == id)
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct TabWorkspace {
    id: String,
    name: String,
    layout: Layout,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Layout {
    Pane {
        #[serde(default)]
        active: bool,
        active_tab: Option<usize>,
        tabs: Vec<Tab>,
    },
    Split {
        axis: SplitAxis,
        flexes: Vec<f32>,
        children: Vec<Layout>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum SplitAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Tab {
    path: std::path::PathBuf,
    #[serde(default)]
    pinned: bool,
}

impl Layout {
    fn empty() -> Self {
        Self::Pane {
            active: true,
            active_tab: None,
            tabs: Vec::new(),
        }
    }

    fn validate(&self, depth: usize) -> anyhow::Result<()> {
        anyhow::ensure!(depth < 32, "Tab workspace split tree is too deep");
        match self {
            Self::Pane {
                active_tab, tabs, ..
            } => {
                anyhow::ensure!(
                    active_tab.is_none_or(|index| index < tabs.len()),
                    "Invalid active tab index"
                );
                let mut paths = std::collections::HashSet::new();
                let mut unpinned = false;
                for tab in tabs {
                    anyhow::ensure!(
                        !tab.path.as_os_str().is_empty() && paths.insert(&tab.path),
                        "Duplicate or empty tab path"
                    );
                    anyhow::ensure!(
                        !tab.pinned || !unpinned,
                        "Pinned tabs must precede unpinned tabs"
                    );
                    unpinned |= !tab.pinned;
                    anyhow::ensure!(
                        tab.path.is_absolute()
                            || tab.path.components().all(|component| matches!(
                                component,
                                std::path::Component::Normal(_)
                            )),
                        "Invalid relative tab path"
                    );
                }
            }
            Self::Split {
                children, flexes, ..
            } => {
                anyhow::ensure!(
                    !children.is_empty() && children.len() == flexes.len(),
                    "Invalid split children or proportions"
                );
                anyhow::ensure!(
                    flexes.iter().all(|flex| flex.is_finite() && *flex > 0.),
                    "Invalid split proportions"
                );
                for child in children {
                    child.validate(depth + 1)?;
                }
            }
        }
        Ok(())
    }

    fn paths(&self, root: &std::path::Path, paths: &mut Vec<std::path::PathBuf>) {
        match self {
            Self::Pane { tabs, .. } => paths.extend(tabs.iter().map(|tab| root.join(&tab.path))),
            Self::Split { children, .. } => {
                for child in children {
                    child.paths(root, paths);
                }
            }
        }
    }
}

#[derive(Default)]
struct WriteLock(std::sync::Arc<futures::lock::Mutex<()>>);
impl gpui::Global for WriteLock {}

struct Writer {
    path: std::path::PathBuf,
    expected: std::sync::Arc<futures::lock::Mutex<Option<String>>>,
    lock: std::sync::Arc<futures::lock::Mutex<()>>,
    pending: Option<PendingWrite>,
}

async fn read_file(fs: &dyn fs::Fs, path: &std::path::Path) -> anyhow::Result<Option<String>> {
    if fs.metadata(path).await?.is_none() {
        return Ok(None);
    }
    Ok(Some(fs.load(path).await?))
}

impl Writer {
    fn enqueue(
        &mut self,
        text: String,
        fs: std::sync::Arc<dyn fs::Fs>,
        cx: &gpui::App,
    ) -> PendingWrite {
        let previous = self.pending.clone();
        let path = self.path.clone();
        let expected = self.expected.clone();
        let lock = self.lock.clone();
        let task = cx.background_spawn(async move {
            if let Some(previous) = previous { previous.await?; }
            let result: anyhow::Result<()> = async {
                let _guard = lock.lock().await;
                let mut expected = expected.lock().await;
                anyhow::ensure!(read_file(fs.as_ref(), &path).await? == *expected,
                    "{} changed outside this window. Reopen the project before saving tab workspaces to avoid overwriting it.", path.display());
                let parent = path.parent().ok_or_else(|| anyhow::anyhow!("Missing workspace directory"))?;
                fs.create_dir(parent).await?;
                fs.atomic_write(path, text.clone()).await?;
                *expected = Some(text);
                Ok(())
            }.await;
            result.map_err(|error| std::sync::Arc::<str>::from(format!("{error:#}")))
        }).shared();
        self.pending = Some(task.clone());
        cx.background_spawn({
            let task = task.clone();
            async move { task.await }
        })
        .detach();
        task
    }
}

impl crate::Workspace {
    pub(crate) fn initialize_tab_workspaces(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.tab_workspaces.loading
            || self.tab_workspaces.root.is_some()
            || !self.project.read(cx).is_local()
        {
            return;
        }
        let root = self
            .project
            .read(cx)
            .visible_worktrees(cx)
            .find_map(|worktree| {
                let worktree = worktree.read(cx);
                worktree
                    .root_entry()
                    .filter(|entry| entry.is_dir())
                    .map(|_| worktree.abs_path().to_path_buf())
            });
        let Some(root) = root else {
            return;
        };
        let path = root.join(".zed/tab-workspaces.json");
        let fs = self.app_state.fs.clone();
        self.tab_workspaces.loading = true;
        cx.spawn_in(window, async move |workspace, cx| {
            let result = async {
                let text = read_file(fs.as_ref(), &path).await?;
                let document = text
                    .as_deref()
                    .map(Document::parse)
                    .transpose()?
                    .unwrap_or_default();
                anyhow::Ok((text, document))
            }
            .await;
            workspace
                .update_in(cx, |workspace, _, cx| {
                    workspace.tab_workspaces.loading = false;
                    match result {
                        Ok((text, document)) => {
                            workspace.tab_workspaces.root = Some(root);
                            workspace.tab_workspaces.document = document;
                            workspace.tab_workspaces.writer = Some(Writer {
                                path,
                                expected: std::sync::Arc::new(futures::lock::Mutex::new(text)),
                                lock: cx.default_global::<WriteLock>().0.clone(),
                                pending: None,
                            });
                            // Native restoration is authoritative. Only subsequent changes update the named list.
                        }
                        Err(error) => workspace
                            .show_error(format!("Could not load tab workspaces: {error:#}"), cx),
                    }
                    workspace.notify_tab_workspace_panes(cx);
                })
                .log_err();
        })
        .detach();
    }

    fn notify_tab_workspace_panes(&self, cx: &mut gpui::Context<Self>) {
        for pane in &self.panes {
            pane.update(cx, |_, cx| cx.notify());
        }
        cx.notify();
    }

    fn capture_tab_workspace(&self, cx: &gpui::App) -> Layout {
        fn capture(
            member: &crate::Member,
            root: &std::path::Path,
            workspace: &crate::Workspace,
            cx: &gpui::App,
        ) -> Option<Layout> {
            match member {
                crate::Member::Pane(pane_handle) => {
                    let pane = pane_handle.read(cx);
                    let mut tabs = Vec::new();
                    let mut active_tab = None;
                    let mut paths = std::collections::HashSet::new();
                    for (index, item) in pane.items().enumerate() {
                        if item.buffer_kind(cx) != crate::item::ItemBufferKind::Singleton {
                            continue;
                        }
                        // Preview/diff views may expose a file path without being reopenable by that path.
                        if !cx
                            .try_global::<crate::ProjectItemRegistry>()
                            .is_some_and(|registry| {
                                registry
                                    .tab_workspace_item_types
                                    .contains(&item.to_any_view().entity_type())
                            })
                        {
                            continue;
                        }
                        let Some(path) = item
                            .project_path(cx)
                            .and_then(|path| workspace.project.read(cx).absolute_path(&path, cx))
                        else {
                            continue;
                        };
                        let path = match path.strip_prefix(root) {
                            Ok(relative) => relative
                                .components()
                                .map(|component| component.as_os_str().to_str())
                                .collect::<Option<Vec<_>>>()
                                .map(|components| std::path::PathBuf::from(components.join("/"))),
                            Err(_) => path.to_str().map(std::path::PathBuf::from),
                        };
                        let Some(path) = path else {
                            continue;
                        };
                        if !paths.insert(path.clone()) {
                            continue;
                        }
                        if index == pane.active_item_index() {
                            active_tab = Some(tabs.len());
                        }
                        tabs.push(Tab {
                            path,
                            pinned: index < pane.pinned_count(),
                        });
                    }
                    if tabs.is_empty() {
                        return None;
                    }
                    Some(Layout::Pane {
                        active: workspace.active_pane() == pane_handle,
                        active_tab: active_tab.or(Some(0)),
                        tabs,
                    })
                }
                crate::Member::Axis(axis) => {
                    let original_flexes = axis.flexes.lock();
                    let mut children = Vec::new();
                    let mut flexes = Vec::new();
                    for (index, member) in axis.members.iter().enumerate() {
                        if let Some(child) = capture(member, root, workspace, cx) {
                            children.push(child);
                            flexes.push(original_flexes.get(index).copied().unwrap_or(1.));
                        }
                    }
                    if children.is_empty() {
                        return None;
                    }
                    if children.len() == 1 {
                        return children.pop();
                    }
                    let total: f32 = flexes.iter().sum();
                    let count = flexes.len() as f32;
                    for flex in &mut flexes {
                        *flex *= count / total;
                    }
                    Some(Layout::Split {
                        axis: match axis.axis {
                            gpui::Axis::Horizontal => SplitAxis::Horizontal,
                            gpui::Axis::Vertical => SplitAxis::Vertical,
                        },
                        flexes,
                        children,
                    })
                }
            }
        }
        self.tab_workspaces
            .root
            .as_deref()
            .and_then(|root| capture(&self.center.root, root, self, cx))
            .unwrap_or_else(Layout::empty)
    }

    pub(crate) fn schedule_tab_workspace_save(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.restoring_workspace
            || self.tab_workspaces.busy
            || self.tab_workspaces.save_failed
            || self.tab_workspaces.writer.is_none()
            || self.tab_workspaces.document.active_mut().is_none()
        {
            return;
        }
        self.tab_workspaces.save_task = Some(cx.spawn_in(window, async move |workspace, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(300))
                .await;
            workspace
                .update(cx, |workspace, cx| {
                    workspace.tab_workspaces.save_task.take();
                    workspace.flush_tab_workspace(cx);
                })
                .log_err();
        }));
    }

    pub(crate) fn flush_tab_workspace(&mut self, cx: &mut gpui::App) -> Option<PendingWrite> {
        self.tab_workspaces.save_task.take();
        if !self.tab_workspaces.busy
            && !self.tab_workspaces.save_failed
            && !self.restoring_workspace
            && self.tab_workspaces.writer.is_some()
        {
            let layout = self.capture_tab_workspace(cx);
            if let Some(active) = self.tab_workspaces.document.active_mut()
                && active.layout != layout
            {
                active.layout = layout;
                let document = self.tab_workspaces.document.clone();
                return self.write_tab_workspace_document(&document, cx);
            }
        }
        self.tab_workspaces
            .writer
            .as_ref()
            .and_then(|writer| writer.pending.clone())
    }

    fn write_tab_workspace_document(
        &mut self,
        document: &Document,
        cx: &mut gpui::App,
    ) -> Option<PendingWrite> {
        let text = match serde_json::to_string_pretty(document) {
            Ok(text) => format!("{text}\n"),
            Err(error) => {
                let workspace = self.weak_self.clone();
                cx.defer(move |cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.show_error(anyhow::Error::from(error), cx)
                        })
                        .log_err();
                });
                return None;
            }
        };
        let task =
            self.tab_workspaces
                .writer
                .as_mut()?
                .enqueue(text, self.app_state.fs.clone(), cx);
        cx.spawn({
            let task = task.clone();
            let workspace = self.weak_self.clone();
            async move |cx| {
                if let Err(error) = task.await {
                    workspace
                        .update(cx, |workspace, cx| {
                            if !workspace.tab_workspaces.save_failed {
                                workspace.tab_workspaces.save_failed = true;
                                workspace.show_error(
                                    format!("Could not save tab workspaces: {error}. Reopen the project to retry."),
                                    cx,
                                );
                                workspace.notify_tab_workspace_panes(cx);
                            }
                        })
                        .log_err();
                }
            }
        })
        .detach();
        Some(task)
    }

    pub(crate) fn new_tab_workspace(
        &mut self,
        _: &NewTabWorkspace,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.tab_workspaces.busy || self.tab_workspaces.save_failed {
            return;
        }
        if self.tab_workspaces.writer.is_none() {
            self.initialize_tab_workspaces(window, cx);
            self.show_error("Tab workspaces require a loaded local project folder. Try again after loading completes.", cx);
            return;
        }
        let workspace = cx.weak_entity();
        self.toggle_modal(window, cx, |window, cx| NameModal {
            workspace,
            input: cx.new(|cx| ui_input::InputField::new(window, cx, "Workspace name")),
            error: None,
        });
    }

    fn create_tab_workspace(
        &mut self,
        name: String,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.tab_workspaces.busy || self.tab_workspaces.save_failed {
            return;
        }
        self.activate_tab_workspace(
            TabWorkspace {
                id: uuid::Uuid::new_v4().to_string(),
                name,
                layout: Layout::empty(),
            },
            window,
            cx,
        );
    }

    pub(crate) fn switch_tab_workspace(
        &mut self,
        id: String,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.tab_workspaces.busy
            || self.tab_workspaces.save_failed
            || self.tab_workspaces.document.active_workspace_id.as_ref() == Some(&id)
        {
            return;
        }
        let Some(target) = self
            .tab_workspaces
            .document
            .workspaces
            .iter()
            .find(|workspace| workspace.id == id)
            .cloned()
        else {
            return;
        };
        self.activate_tab_workspace(target, window, cx);
    }

    fn activate_tab_workspace(
        &mut self,
        target: TabWorkspace,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(root) = self.tab_workspaces.root.clone() else {
            return;
        };
        self.tab_workspaces.save_task.take();
        self.tab_workspaces.busy = true;
        self.notify_tab_workspace_panes(cx);
        let fs = self.app_state.fs.clone();
        cx.spawn_in(window, async move |workspace, cx| {
            let mut wrote_target = false;
            let result: anyhow::Result<()> = async {
                let mut paths = Vec::new();
                target.layout.paths(&root, &mut paths);
                let mut loaded = std::collections::VecDeque::new();
                for path in paths {
                    anyhow::ensure!(fs.metadata(&path).await?.is_some_and(|metadata| !metadata.is_dir), "Cannot restore {}: file is missing or is a directory. The current tabs were kept.", path.display());
                    let project_path = workspace.update(cx, |workspace, cx| {
                        Self::project_path_for_path(workspace.project.clone(), &path, false, cx)
                    })?.await?.1;
                    let (entry, builder) = workspace.update_in(cx, |workspace, window, cx| {
                        workspace.load_path(project_path.clone(), window, cx)
                    })?.await?;
                    loaded.push_back((entry, project_path, builder));
                }
                let confirmed = workspace.update_in(cx, |workspace, window, cx| {
                    workspace.save_all_internal(crate::SaveIntent::Close, false, window, cx)
                })?.await?;
                if !confirmed { return Ok(()); }
                let edited = std::rc::Rc::new(std::cell::Cell::new(false));
                let (document, write, original_items, subscriptions) = workspace.update_in(cx, |workspace, window, cx| {
                    let mut document = workspace.tab_workspaces.document.clone();
                    if let Some(active) = document.active_mut() { active.layout = workspace.capture_tab_workspace(cx); }
                    if !document.workspaces.iter().any(|workspace| workspace.id == target.id) {
                        document.workspaces.push(target.clone());
                    }
                    document.active_workspace_id = Some(target.id.clone());
                    let original_items = workspace.center.panes().into_iter().flat_map(|pane| pane.read(cx).items().map(|item| item.item_id())).collect::<Vec<_>>();
                    let items = workspace.center.panes().into_iter().flat_map(|pane| pane.read(cx).items().cloned()).collect::<Vec<_>>();
                    let subscriptions = items.into_iter().map(|item| {
                        let edited = edited.clone();
                        item.subscribe_to_item_events(window, cx, Box::new(move |event, _, _| {
                            if matches!(event, crate::item::ItemEvent::Edit) { edited.set(true); }
                        }))
                    }).collect::<Vec<_>>();
                    let write = workspace.write_tab_workspace_document(&document, cx);
                    (document, write, original_items, subscriptions)
                })?;
                let write = write.ok_or_else(|| anyhow::anyhow!("Tab workspace storage is unavailable"))?;
                write.await.map_err(|error| anyhow::anyhow!("{error}"))?;
                wrote_target = true;
                workspace.update_in(cx, |workspace, window, cx| {
                    let current_items = workspace.center.panes().into_iter().flat_map(|pane| pane.read(cx).items().map(|item| item.item_id())).collect::<Vec<_>>();
                    // Do not close tabs opened while the filesystem write was in flight.
                    anyhow::ensure!(current_items == original_items && !edited.get(), "Tabs changed during the switch. Please try again.");
                    let old_root = workspace.center.root.clone();
                    let original_panes = workspace.panes.clone();
                    let previous_active = workspace.active_pane().clone();
                    let mut active_pane = None;
                    let new_root = match workspace.build_tab_workspace_layout(&target.layout, &mut loaded, &mut active_pane, window, cx) {
                        Ok(root) => root,
                        Err(error) => {
                            let staged = workspace.panes.iter().filter(|pane| !original_panes.contains(pane)).cloned().collect::<Vec<_>>();
                            workspace.discard_tab_workspace_panes(&staged, window, cx);
                            workspace.set_active_pane(&previous_active, window, cx);
                            window.focus(&previous_active.focus_handle(cx), cx);
                            return Err(error);
                        }
                    };
                    workspace.center = crate::PaneGroup::with_root(new_root);
                    workspace.center.set_is_center(true);
                    workspace.center.mark_positions(cx);
                    if workspace.zoomed_position.is_none() {
                        workspace.zoomed = None;
                    }
                    workspace.maximized_pane = None;
                    let old_panes = crate::PaneGroup::with_root(old_root);
                    let old_panes = old_panes.panes().into_iter().cloned().collect::<Vec<_>>();
                    workspace.discard_tab_workspace_panes(&old_panes, window, cx);
                    let pane = active_pane.unwrap_or_else(|| workspace.center.first_pane());
                    workspace.set_active_pane(&pane, window, cx);
                    window.focus(&pane.focus_handle(cx), cx);
                    workspace.tab_workspaces.document = document;
                    workspace.serialize_workspace(window, cx);
                    anyhow::Ok(())
                })??;
                drop(subscriptions);
                Ok(())
            }.await;
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.tab_workspaces.busy = false;
                if let Err(error) = result {
                    workspace.show_error(format!("Could not switch tab workspace: {error:#}"), cx);
                    if wrote_target {
                        // Restore the active ID if the final UI commit was cancelled after writing it.
                        let layout = workspace.capture_tab_workspace(cx);
                        if let Some(active) = workspace.tab_workspaces.document.active_mut() {
                            active.layout = layout;
                        }
                        let document = workspace.tab_workspaces.document.clone();
                        workspace.write_tab_workspace_document(&document, cx);
                    }
                }
                workspace.notify_tab_workspace_panes(cx);
                workspace.schedule_tab_workspace_save(window, cx);
            }).log_err();
        }).detach();
    }

    pub(crate) fn tab_workspace_operation_in_progress(&self) -> bool {
        self.tab_workspaces.busy
    }

    fn discard_tab_workspace_panes(
        &mut self,
        panes: &[gpui::Entity<crate::Pane>],
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        for pane in panes {
            pane.update(cx, |pane, cx| {
                let ids = pane.items().map(|item| item.item_id()).collect::<Vec<_>>();
                for id in ids {
                    pane.remove_item(id, false, false, window, cx);
                }
            });
            self.force_remove_pane(pane, &None, window, cx);
        }
    }

    fn build_tab_workspace_layout(
        &mut self,
        layout: &Layout,
        loaded: &mut std::collections::VecDeque<LoadedTab>,
        active_pane: &mut Option<gpui::Entity<crate::Pane>>,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> anyhow::Result<crate::Member> {
        match layout {
            Layout::Pane {
                active,
                active_tab,
                tabs,
            } => {
                let pane = self.add_pane(window, cx);
                let result = pane.update(cx, |pane, cx| {
                    pane.restoring_tab_workspace = true;
                    for index in 0..tabs.len() {
                        let (entry, path, builder) = loaded
                            .pop_front()
                            .ok_or_else(|| anyhow::anyhow!("Missing preloaded tab"))?;
                        let item = pane.open_item(
                            entry,
                            path,
                            false,
                            false,
                            false,
                            Some(index),
                            window,
                            cx,
                            builder,
                        );
                        anyhow::ensure!(
                            item.downcast::<crate::invalid_item_view::InvalidItemView>()
                                .is_none(),
                            "A target file could not be opened. The current tabs were kept."
                        );
                    }
                    pane.set_pinned_count(tabs.iter().take_while(|tab| tab.pinned).count());
                    if let Some(index) = active_tab {
                        pane.activate_item(*index, false, false, window, cx);
                    }
                    anyhow::Ok(())
                });
                pane.update(cx, |pane, _| pane.restoring_tab_workspace = false);
                result?;
                if *active {
                    *active_pane = Some(pane.clone());
                }
                Ok(crate::Member::Pane(pane))
            }
            Layout::Split {
                axis,
                flexes,
                children,
            } => {
                let members = children
                    .iter()
                    .map(|child| {
                        self.build_tab_workspace_layout(child, loaded, active_pane, window, cx)
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;
                Ok(crate::Member::Axis(crate::PaneAxis::load(
                    match axis {
                        SplitAxis::Horizontal => gpui::Axis::Horizontal,
                        SplitAxis::Vertical => gpui::Axis::Vertical,
                    },
                    members,
                    Some(flexes.clone()),
                )))
            }
        }
    }

    pub fn render_tab_workspace_menu(
        &self,
        handle: ui::PopoverMenuHandle<ui::ContextMenu>,
    ) -> gpui::AnyElement {
        let workspace = self.weak_self.clone();
        let (disabled, name, entries) = self.tab_workspace_menu_entries();
        ui::h_flex()
            .min_w_0()
            .max_w(gpui::rems(18.))
            .child(
                ui::PopoverMenu::new("tab-workspaces")
                    .trigger(
                        ui::Button::new("active-tab-workspace", format!("TABS {name}"))
                            .label_size(ui::LabelSize::Small)
                            .truncate(true)
                            .end_icon(
                                ui::Icon::new(ui::IconName::ChevronDown).size(ui::IconSize::XSmall),
                            )
                            .disabled(disabled),
                    )
                    .anchor(gpui::Anchor::TopRight)
                    .with_handle(handle)
                    .menu(move |window, cx| {
                        Some(ui::ContextMenu::build(window, cx, |menu, _, _| {
                            let mut menu = menu.action("New Workspace", Box::new(NewTabWorkspace));
                            if !entries.is_empty() {
                                menu = menu.separator();
                            }
                            for (id, label) in &entries {
                                let workspace = workspace.clone();
                                let id = id.clone();
                                menu = menu.entry(label.clone(), None, move |window, cx| {
                                    workspace
                                        .update(cx, |workspace, cx| {
                                            workspace.switch_tab_workspace(id.clone(), window, cx);
                                        })
                                        .log_err();
                                });
                            }
                            menu
                        }))
                    }),
            )
            .into_any_element()
    }

    pub(crate) fn tab_workspace_menu_entries(&self) -> (bool, String, Vec<(String, String)>) {
        let active = self
            .tab_workspaces
            .document
            .workspaces
            .iter()
            .find(|workspace| {
                Some(&workspace.id) == self.tab_workspaces.document.active_workspace_id.as_ref()
            });
        (
            self.tab_workspaces.busy
                || self.tab_workspaces.loading
                || self.tab_workspaces.save_failed,
            active
                .map(|workspace| workspace.name.clone())
                .unwrap_or_else(|| "Workspaces".into()),
            self.tab_workspaces
                .document
                .workspaces
                .iter()
                .map(|workspace| {
                    let label = if Some(&workspace.id)
                        == self.tab_workspaces.document.active_workspace_id.as_ref()
                    {
                        format!("✓ {}", workspace.name)
                    } else {
                        workspace.name.clone()
                    };
                    (workspace.id.clone(), label)
                })
                .collect(),
        )
    }
}

struct NameModal {
    workspace: gpui::WeakEntity<crate::Workspace>,
    input: gpui::Entity<ui_input::InputField>,
    error: Option<String>,
}

impl gpui::Focusable for NameModal {
    fn focus_handle(&self, cx: &gpui::App) -> gpui::FocusHandle {
        self.input.focus_handle(cx)
    }
}
impl gpui::EventEmitter<gpui::DismissEvent> for NameModal {}
impl crate::ModalView for NameModal {}

impl NameModal {
    fn confirm(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        let name = self.input.read(cx).text(cx).trim().to_owned();
        let duplicate = self
            .workspace
            .read_with(cx, |workspace, _| {
                workspace
                    .tab_workspaces
                    .document
                    .workspaces
                    .iter()
                    .any(|workspace| workspace.name == name)
            })
            .unwrap_or(false);
        if name.is_empty() || duplicate {
            self.error = Some(
                if duplicate {
                    "A workspace with this name already exists."
                } else {
                    "Enter a workspace name."
                }
                .into(),
            );
            cx.notify();
            return;
        }
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.create_tab_workspace(name, window, cx)
            })
            .log_err();
        cx.emit(gpui::DismissEvent);
    }
}

impl gpui::Render for NameModal {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        ui::v_flex()
            .elevation_3(cx)
            .key_context("TabWorkspaceName")
            .track_focus(&self.focus_handle(cx))
            .w(gpui::rems(24.))
            .p_4()
            .gap_3()
            .on_action(cx.listener(|this, _: &menu::Confirm, window, cx| this.confirm(window, cx)))
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| cx.emit(gpui::DismissEvent)))
            .child(ui::Label::new("New Workspace"))
            .child(self.input.clone())
            .children(
                self.error
                    .clone()
                    .map(|error| ui::Label::new(error).color(ui::Color::Error)),
            )
            .child(
                ui::h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        ui::Button::new("cancel-tab-workspace", "Cancel")
                            .size(ui::ButtonSize::Compact)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(gpui::DismissEvent))),
                    )
                    .child(
                        ui::Button::new("create-tab-workspace", "Create")
                            .size(ui::ButtonSize::Compact)
                            .on_click(cx.listener(|this, _, window, cx| this.confirm(window, cx))),
                    ),
            )
    }
}
