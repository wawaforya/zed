use fs::Fs as _;
use gpui::{AppContext as _, VisualContext as _};

struct FileTab {
    buffer: gpui::Entity<language::Buffer>,
    focus: gpui::FocusHandle,
    dirty: bool,
}

impl gpui::Focusable for FileTab {
    fn focus_handle(&self, _: &gpui::App) -> gpui::FocusHandle {
        self.focus.clone()
    }
}
impl gpui::EventEmitter<crate::item::ItemEvent> for FileTab {}
impl gpui::Render for FileTab {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        gpui::div()
    }
}
impl crate::Item for FileTab {
    type Event = crate::item::ItemEvent;
    fn tab_content_text(&self, _: usize, _: &gpui::App) -> gpui::SharedString {
        "File".into()
    }
    fn buffer_kind(&self, _: &gpui::App) -> crate::item::ItemBufferKind {
        crate::item::ItemBufferKind::Singleton
    }
    fn for_each_project_item(
        &self,
        cx: &gpui::App,
        callback: &mut dyn FnMut(gpui::EntityId, &dyn project::ProjectItem),
    ) {
        callback(self.buffer.entity_id(), self.buffer.read(cx));
    }
    fn to_item_events(event: &Self::Event, callback: &mut dyn FnMut(crate::item::ItemEvent)) {
        callback(*event);
    }
    fn is_dirty(&self, _: &gpui::App) -> bool {
        self.dirty
    }
    fn can_save(&self, _: &gpui::App) -> bool {
        true
    }
    fn reload(
        &mut self,
        _: gpui::Entity<project::Project>,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) -> gpui::Task<anyhow::Result<()>> {
        self.dirty = false;
        gpui::Task::ready(Ok(()))
    }
}
impl crate::ProjectItem for FileTab {
    type Item = language::Buffer;
    fn for_project_item(
        _: gpui::Entity<project::Project>,
        _: Option<&crate::Pane>,
        buffer: gpui::Entity<language::Buffer>,
        _: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        Self {
            buffer,
            focus: cx.focus_handle(),
            dirty: false,
        }
    }
}

async fn setup(
    cx: &mut gpui::TestAppContext,
) -> (
    std::sync::Arc<fs::FakeFs>,
    gpui::Entity<crate::Workspace>,
    &mut gpui::VisualTestContext,
) {
    crate::tests::init_test(cx);
    cx.update(crate::register_project_item::<FileTab>);
    let fs = fs::FakeFs::new(cx.executor());
    fs.insert_tree(
        util::path!("/project"),
        serde_json::json!({ "a.rs": "a", "b.rs": "b", "c.rs": "c" }),
    )
    .await;
    let project = project::Project::test(fs.clone(), [util::path!("/project").as_ref()], cx).await;
    let (workspace, cx) =
        cx.add_window_view(|window, cx| crate::Workspace::test_new(project, window, cx));
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.initialize_tab_workspaces(window, cx)
    });
    cx.run_until_parked();
    assert!(workspace.read_with(cx, |workspace, _| workspace.tab_workspaces.writer.is_some()));
    (fs, workspace, cx)
}

async fn open(
    workspace: &gpui::Entity<crate::Workspace>,
    name: &str,
    pane: Option<gpui::WeakEntity<crate::Pane>>,
    cx: &mut gpui::VisualTestContext,
) -> Box<dyn crate::ItemHandle> {
    let path = workspace.read_with(cx, |workspace, cx| project::ProjectPath {
        worktree_id: workspace
            .project
            .read(cx)
            .visible_worktrees(cx)
            .next()
            .unwrap()
            .read(cx)
            .id(),
        path: util::rel_path::RelPath::from_unix_str(name).unwrap().into(),
    });
    workspace
        .update_in(cx, |workspace, window, cx| {
            workspace.open_path(path, pane, true, window, cx)
        })
        .await
        .unwrap()
}

fn create(
    workspace: &gpui::Entity<crate::Workspace>,
    name: &str,
    cx: &mut gpui::VisualTestContext,
) -> String {
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.create_tab_workspace(name.into(), window, cx)
    });
    cx.run_until_parked();
    workspace.read_with(cx, |workspace, _| {
        assert!(!workspace.tab_workspaces.busy);
        workspace
            .tab_workspaces
            .document
            .active_workspace_id
            .clone()
            .unwrap()
    })
}

async fn flush(workspace: &gpui::Entity<crate::Workspace>, cx: &mut gpui::VisualTestContext) {
    if let Some(task) = workspace.update(cx, |workspace, cx| workspace.flush_tab_workspace(cx)) {
        task.await.unwrap();
    }
}

fn tab_paths(
    workspace: &gpui::Entity<crate::Workspace>,
    cx: &gpui::VisualTestContext,
) -> Vec<Vec<String>> {
    workspace.read_with(cx, |workspace, cx| {
        workspace
            .center
            .panes()
            .into_iter()
            .map(|pane| {
                pane.read(cx)
                    .items()
                    .filter_map(|item| {
                        item.project_path(cx)
                            .map(|path| path.path.as_unix_str().to_owned())
                    })
                    .collect()
            })
            .collect()
    })
}

#[test]
fn validates_tab_workspace_document() {
    let text = r#"{"version":1,"workspaces":[{"id":"one","name":"One","layout":{"type":"pane","active_tab":0,"tabs":[{"path":"src/main.rs","pinned":true}]}}]}"#;
    let document = super::Document::parse(text).unwrap();
    assert_eq!(
        super::Document::parse(&serde_json::to_string(&document).unwrap()).unwrap(),
        document
    );
    assert!(super::Document::parse(&text.replace("\"version\":1", "\"version\":2")).is_err());
    assert!(super::Document::parse(&text.replace("\"active_tab\":0", "\"active_tab\":4")).is_err());
    assert!(super::Document::parse(&text.replace("src/main.rs", "../main.rs")).is_err());
    assert!(super::Document::parse("not json").is_err());
    let mut duplicate = document.clone();
    duplicate.workspaces.push(document.workspaces[0].clone());
    assert!(super::Document::parse(&serde_json::to_string(&duplicate).unwrap()).is_err());
}

#[gpui::test]
async fn tab_workspaces_restore_order_splits_and_pins(cx: &mut gpui::TestAppContext) {
    let (fs, workspace, cx) = setup(cx).await;
    let a = create(&workspace, "A", cx);
    open(&workspace, "b.rs", None, cx).await;
    open(&workspace, "a.rs", None, cx).await;
    let first = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());
    first.update_in(cx, |pane, window, cx| {
        pane.set_pinned_count(1);
        pane.activate_item(0, true, true, window, cx);
    });
    let second = workspace.update_in(cx, |workspace, window, cx| {
        workspace.split_pane(first.clone(), crate::SplitDirection::Right, window, cx)
    });
    open(&workspace, "c.rs", Some(second.downgrade()), cx).await;
    workspace.update(cx, |workspace, _| {
        if let crate::Member::Axis(axis) = &workspace.center.root {
            *axis.flexes.lock() = vec![0.7, 1.3];
        }
    });
    let expected = workspace.read_with(cx, |workspace, cx| workspace.capture_tab_workspace(cx));
    let b = create(&workspace, "B", cx);
    assert_eq!(tab_paths(&workspace, cx), vec![Vec::<String>::new()]);
    flush(&workspace, cx).await;
    cx.update_global(|store: &mut settings::SettingsStore, cx| {
        store.update_user_settings(cx, |settings| {
            settings.workspace.max_tabs = std::num::NonZeroUsize::new(1)
        });
    });
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.switch_tab_workspace(a.clone(), window, cx)
    });
    cx.run_until_parked();
    assert_eq!(
        tab_paths(&workspace, cx),
        vec![vec!["b.rs", "a.rs"], vec!["c.rs"]]
    );
    assert_eq!(
        workspace.read_with(cx, |workspace, cx| workspace.capture_tab_workspace(cx)),
        expected
    );
    let document = super::Document::parse(
        &fs.load(std::path::Path::new(util::path!(
            "/project/.zed/tab-workspaces.json"
        )))
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(document.active_workspace_id, Some(a));
    assert!(
        document
            .workspaces
            .iter()
            .any(|workspace| workspace.id == b)
    );
}

#[gpui::test]
async fn tab_workspaces_native_tabs_are_not_replaced_and_unsupported_tabs_are_filtered(
    cx: &mut gpui::TestAppContext,
) {
    let (fs, workspace, cx) = setup(cx).await;
    let id = create(&workspace, "A", cx);
    open(&workspace, "a.rs", None, cx).await;
    flush(&workspace, cx).await;
    let saved = fs
        .load(std::path::Path::new(util::path!(
            "/project/.zed/tab-workspaces.json"
        )))
        .await
        .unwrap();
    let (project, app_state) = workspace.read_with(cx, |workspace, _| {
        (workspace.project.clone(), workspace.app_state.clone())
    });
    let other = cx.new_window_entity(|window, cx| {
        crate::Workspace::new(None, project, app_state, window, cx)
    });
    let file = open(&other, "b.rs", None, cx).await;
    let unsupported = cx.new(crate::item::test::TestItem::new);
    other.update_in(cx, |workspace, window, cx| {
        workspace.add_item_to_active_pane(Box::new(unsupported), None, true, window, cx);
        workspace.initialize_tab_workspaces(window, cx);
    });
    cx.run_until_parked();
    assert_eq!(
        other.read_with(cx, |workspace, cx| workspace
            .active_pane()
            .read(cx)
            .items_len()),
        2
    );
    assert_eq!(
        other.read_with(cx, |workspace, _| workspace
            .tab_workspaces
            .document
            .active_workspace_id
            .clone()),
        Some(id)
    );
    assert_eq!(
        fs.load(std::path::Path::new(util::path!(
            "/project/.zed/tab-workspaces.json"
        )))
        .await
        .unwrap(),
        saved
    );
    flush(&other, cx).await;
    let document = super::Document::parse(
        &fs.load(std::path::Path::new(util::path!(
            "/project/.zed/tab-workspaces.json"
        )))
        .await
        .unwrap(),
    )
    .unwrap();
    let super::Layout::Pane { tabs, .. } = &document.workspaces[0].layout else {
        panic!("expected a pane")
    };
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0].path, std::path::Path::new("b.rs"));
    assert!(other.read_with(cx, |workspace, cx| {
        workspace
            .items(cx)
            .any(|item| item.item_id() == file.item_id())
    }));
}

#[gpui::test]
async fn tab_workspaces_cancel_and_missing_file_preserve_tabs(cx: &mut gpui::TestAppContext) {
    let (_, workspace, cx) = setup(cx).await;
    let a = create(&workspace, "A", cx);
    let b = create(&workspace, "B", cx);
    let item = open(&workspace, "a.rs", None, cx).await;
    let file = item.downcast::<FileTab>().unwrap();
    file.update(cx, |file, cx| {
        file.dirty = true;
        cx.emit(crate::item::ItemEvent::UpdateTab);
    });
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.switch_tab_workspace(a.clone(), window, cx)
    });
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    assert_eq!(
        workspace.read_with(cx, |workspace, _| workspace
            .tab_workspaces
            .document
            .active_workspace_id
            .clone()),
        Some(b.clone())
    );
    assert_eq!(tab_paths(&workspace, cx), vec![vec!["a.rs"]]);
    assert!(file.read_with(cx, |file, _| file.dirty));
    workspace.update(cx, |workspace, _| {
        let target = workspace
            .tab_workspaces
            .document
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == a)
            .unwrap();
        target.layout = super::Layout::Pane {
            active: true,
            active_tab: Some(0),
            tabs: vec![super::Tab {
                path: "missing.rs".into(),
                pinned: false,
            }],
        };
    });
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.switch_tab_workspace(a, window, cx)
    });
    cx.run_until_parked();
    assert!(!cx.has_pending_prompt());
    assert_eq!(tab_paths(&workspace, cx), vec![vec!["a.rs"]]);
    assert_eq!(
        workspace.read_with(cx, |workspace, _| workspace
            .tab_workspaces
            .document
            .active_workspace_id
            .clone()),
        Some(b)
    );
}

#[gpui::test]
async fn tab_workspaces_continuous_save_and_external_conflict(cx: &mut gpui::TestAppContext) {
    let (fs, workspace, cx) = setup(cx).await;
    create(&workspace, "A", cx);
    open(&workspace, "a.rs", None, cx).await;
    open(&workspace, "b.rs", None, cx).await;
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(350));
    cx.run_until_parked();
    let path = std::path::Path::new(util::path!("/project/.zed/tab-workspaces.json"));
    let saved = fs.load(path).await.unwrap();
    let document = super::Document::parse(&saved).unwrap();
    let super::Layout::Pane { tabs, .. } = &document.workspaces[0].layout else {
        panic!("expected a pane")
    };
    assert_eq!(
        tabs.iter().map(|tab| tab.path.clone()).collect::<Vec<_>>(),
        vec![
            std::path::PathBuf::from("a.rs"),
            std::path::PathBuf::from("b.rs")
        ]
    );
    fs.atomic_write(path.to_path_buf(), "external edit".into())
        .await
        .unwrap();
    open(&workspace, "c.rs", None, cx).await;
    let task = workspace
        .update(cx, |workspace, cx| workspace.flush_tab_workspace(cx))
        .unwrap();
    assert!(task.await.is_err());
    assert_eq!(fs.load(path).await.unwrap(), "external edit");
}

#[gpui::test]
async fn tab_workspaces_empty_workspace_and_discard(cx: &mut gpui::TestAppContext) {
    let (_, workspace, cx) = setup(cx).await;
    let empty = create(&workspace, "Empty", cx);
    create(&workspace, "Files", cx);
    let file = open(&workspace, "a.rs", None, cx)
        .await
        .downcast::<FileTab>()
        .unwrap();
    file.update(cx, |file, cx| {
        file.dirty = true;
        cx.emit(crate::item::ItemEvent::UpdateTab);
    });
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.switch_tab_workspace(empty.clone(), window, cx)
    });
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Don't Save");
    cx.run_until_parked();
    assert_eq!(tab_paths(&workspace, cx), vec![Vec::<String>::new()]);
    assert_eq!(
        workspace.read_with(cx, |workspace, _| workspace
            .tab_workspaces
            .document
            .active_workspace_id
            .clone()),
        Some(empty)
    );
}

#[gpui::test]
async fn tab_workspaces_notify_title_bar_observers(cx: &mut gpui::TestAppContext) {
    let (_, workspace, cx) = setup(cx).await;
    let observed = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let _subscription = cx.update(|_, cx| {
        let observed = observed.clone();
        cx.observe(&workspace, move |workspace, cx| {
            let (disabled, name, _) = workspace.read(cx).tab_workspace_menu_entries();
            observed.borrow_mut().push((disabled, name));
        })
    });
    let first = create(&workspace, "First", cx);
    assert_eq!(observed.borrow().last(), Some(&(false, "First".into())));
    assert!(observed.borrow().iter().any(|(disabled, _)| *disabled));
    create(&workspace, "Second", cx);
    assert_eq!(observed.borrow().last(), Some(&(false, "Second".into())));
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.switch_tab_workspace(first, window, cx);
    });
    cx.run_until_parked();
    assert_eq!(observed.borrow().last(), Some(&(false, "First".into())));
}

#[gpui::test]
async fn tab_workspaces_new_workspace_starts_empty(cx: &mut gpui::TestAppContext) {
    let (fs, workspace, cx) = setup(cx).await;
    open(&workspace, "a.rs", None, cx).await;
    let id = create(&workspace, "Empty", cx);
    assert_eq!(tab_paths(&workspace, cx), vec![Vec::<String>::new()]);
    let document = super::Document::parse(
        &fs.load(std::path::Path::new(util::path!(
            "/project/.zed/tab-workspaces.json"
        )))
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(document.active_workspace_id, Some(id));
    assert_eq!(document.workspaces.len(), 1);
    assert_eq!(document.workspaces[0].layout, super::Layout::empty());
}

#[gpui::test]
async fn tab_workspaces_cancel_or_failed_creation_keeps_current_workspace(
    cx: &mut gpui::TestAppContext,
) {
    let (fs, workspace, cx) = setup(cx).await;
    create(&workspace, "Current", cx);
    let file = open(&workspace, "a.rs", None, cx)
        .await
        .downcast::<FileTab>()
        .unwrap();
    flush(&workspace, cx).await;
    let original =
        workspace.read_with(cx, |workspace, _| workspace.tab_workspaces.document.clone());
    let path = std::path::Path::new(util::path!("/project/.zed/tab-workspaces.json"));
    let saved = fs.load(path).await.unwrap();
    file.update(cx, |file, cx| {
        file.dirty = true;
        cx.emit(crate::item::ItemEvent::UpdateTab);
    });
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.create_tab_workspace("Cancelled".into(), window, cx);
    });
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    assert_eq!(tab_paths(&workspace, cx), vec![vec!["a.rs"]]);
    assert!(file.read_with(cx, |file, _| file.dirty));
    assert_eq!(
        workspace.read_with(cx, |workspace, _| workspace.tab_workspaces.document.clone()),
        original
    );
    assert_eq!(fs.load(path).await.unwrap(), saved);

    file.update(cx, |file, cx| {
        file.dirty = false;
        cx.emit(crate::item::ItemEvent::UpdateTab);
    });
    fs.atomic_write(path.to_path_buf(), "external edit".into())
        .await
        .unwrap();
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.create_tab_workspace("Failed".into(), window, cx);
    });
    cx.run_until_parked();
    assert_eq!(tab_paths(&workspace, cx), vec![vec!["a.rs"]]);
    assert_eq!(
        workspace.read_with(cx, |workspace, _| workspace.tab_workspaces.document.clone()),
        original
    );
    assert_eq!(fs.load(path).await.unwrap(), "external edit");
}

#[gpui::test]
async fn tab_workspaces_corrupt_storage_does_not_replace_native_tabs(
    cx: &mut gpui::TestAppContext,
) {
    let (fs, workspace, cx) = setup(cx).await;
    open(&workspace, "a.rs", None, cx).await;
    let path = std::path::Path::new(util::path!("/project/.zed/tab-workspaces.json"));
    fs.create_dir(path.parent().unwrap()).await.unwrap();
    fs.atomic_write(path.to_path_buf(), "broken json".into())
        .await
        .unwrap();
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.tab_workspaces = super::State::default();
        workspace.initialize_tab_workspaces(window, cx);
    });
    cx.run_until_parked();
    assert_eq!(tab_paths(&workspace, cx), vec![vec!["a.rs"]]);
    assert!(workspace.read_with(cx, |workspace, _| workspace.tab_workspaces.writer.is_none()));
    assert!(
        workspace
            .update(cx, |workspace, cx| workspace.flush_tab_workspace(cx))
            .is_none()
    );
    assert_eq!(fs.load(path).await.unwrap(), "broken json");
}
