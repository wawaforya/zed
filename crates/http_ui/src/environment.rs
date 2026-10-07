use gpui::AppContext as _;
use util::ResultExt as _;

#[derive(Default)]
struct Selections(
    std::collections::HashMap<(gpui::EntityId, project::ProjectPath), gpui::Entity<Selection>>,
);
impl gpui::Global for Selections {}

pub(super) struct Selection {
    pub environment: super::workbench::Environment,
    pub inherited: Option<String>,
    pub error: Option<String>,
    project: gpui::WeakEntity<project::Project>,
    buffer: gpui::WeakEntity<language::Buffer>,
    pub path: project::ProjectPath,
    revision: u64,
    synchronized: Option<(language::LanguageServerId, u64)>,
    server_id: Option<language::LanguageServerId>,
    inheritance_task: Option<gpui::Task<()>>,
    _subscription: gpui::Subscription,
    _settings_subscription: gpui::Subscription,
}

pub(super) fn for_buffer(
    project: &gpui::Entity<project::Project>,
    buffer: &gpui::Entity<language::Buffer>,
    cx: &mut gpui::App,
) -> Option<gpui::Entity<Selection>> {
    let file = buffer.read(cx).file()?;
    let path = project::ProjectPath {
        worktree_id: file.worktree_id(cx),
        path: file.path().clone(),
    };
    let key = (project.entity_id(), path.clone());
    cx.default_global::<Selections>();
    if let Some(selection) = cx.global::<Selections>().0.get(&key).cloned() {
        selection.update(cx, |selection, cx| {
            selection.buffer = buffer.downgrade();
            selection.refresh_inherited(cx);
            selection.synchronize(cx);
        });
        return Some(selection);
    }
    let selection = cx.new(|cx| Selection {
        environment: Default::default(),
        inherited: None,
        error: None,
        project: project.downgrade(),
        buffer: buffer.downgrade(),
        path,
        revision: 0,
        synchronized: None,
        server_id: None,
        inheritance_task: None,
        _settings_subscription: cx.observe_global::<settings::SettingsStore>(
            |selection: &mut Selection, cx| {
                if selection.environment == super::workbench::Environment::Inherit
                    && selection.buffer.upgrade().is_some()
                {
                    selection.refresh_inherited(cx);
                }
            },
        ),
        _subscription: cx.subscribe(project, |selection: &mut Selection, project, event, cx| {
            match event {
                project::Event::LanguageServerBufferRegistered {
                    server_id,
                    buffer_id,
                    ..
                } if project
                    .read(cx)
                    .language_server_statuses(cx)
                    .any(|(id, status)| {
                        id == *server_id
                            && status.name
                                == language::LanguageServerName::new_static(
                                    "httpyac-language-server",
                                )
                    })
                    && selection
                        .buffer
                        .upgrade()
                        .is_some_and(|buffer| buffer.read(cx).remote_id() == *buffer_id) =>
                {
                    selection.server_id = Some(*server_id);
                    selection.synchronized = None;
                    selection.synchronize(cx);
                }
                project::Event::LanguageServerRemoved(server_id)
                    if selection.server_id == Some(*server_id) =>
                {
                    selection.server_id = None;
                    selection.synchronized = None;
                }
                project::Event::LanguageServerAdded(_, name, _)
                    if *name
                        == language::LanguageServerName::new_static("httpyac-language-server") =>
                {
                    selection.synchronize(cx);
                }
                _ => {}
            }
        }),
    });
    cx.global_mut::<Selections>()
        .0
        .insert(key, selection.clone());
    let project_id = project.entity_id();
    cx.observe_release(project, move |_, cx| {
        cx.global_mut::<Selections>()
            .0
            .retain(|(id, _), _| *id != project_id);
    })
    .detach();
    selection.update(cx, |selection, cx| selection.refresh_inherited(cx));
    Some(selection)
}

impl Selection {
    pub fn select(
        &mut self,
        environment: super::workbench::Environment,
        cx: &mut gpui::Context<Self>,
    ) {
        self.environment = environment;
        self.revision += 1;
        self.error = None;
        if self.environment == super::workbench::Environment::Inherit {
            self.refresh_inherited(cx);
        }
        self.synchronize(cx);
        cx.notify();
    }

    fn refresh_inherited(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(project) = self.project.upgrade() else {
            return;
        };
        let Some(worktree) = project.read(cx).worktree_for_id(self.path.worktree_id, cx) else {
            return;
        };
        let root = Some(worktree.read(cx).abs_path().to_path_buf());
        let settings = project
            .read(cx)
            .terminal_settings(&root, cx)
            .env
            .get("ZED_HTTPYAC_ENV")
            .cloned();
        let environment = project.read(cx).environment().clone();
        let task = environment.update(cx, |environment, cx| {
            environment.worktree_environment(worktree, cx)
        });
        self.inheritance_task = Some(cx.spawn(async move |this, cx| {
            let environment = task.await;
            this.update(cx, |this, cx| {
                this.inherited = settings.or_else(|| environment.map(|environment| {
                    environment.get("ZED_HTTPYAC_ENV").cloned().unwrap_or_default()
                }));
                if this.inherited.is_none() && this.environment == super::workbench::Environment::Inherit {
                    this.error = Some("Could not resolve the inherited HTTP environment; check the project's shell environment or select a named environment".into());
                }
                this.revision += 1;
                this.synchronize(cx);
                cx.notify();
            })
            .log_err();
        }));
    }

    fn synchronize(&mut self, cx: &mut gpui::Context<Self>) {
        let value = match &self.environment {
            super::workbench::Environment::Inherit => match &self.inherited {
                Some(value) => value.as_str(),
                None => return,
            },
            super::workbench::Environment::None => "",
            super::workbench::Environment::Named(value) => value.as_str(),
        };
        let environments =
            match serde_json::from_str::<Vec<String>>(&super::workbench::environment_key(value)) {
                Ok(environments) => environments,
                Err(error) => {
                    self.error = Some(format!("Invalid HTTP environment selection: {error}"));
                    cx.notify();
                    return;
                }
            };
        let Some(project) = self.project.upgrade() else {
            return;
        };
        let Some(_buffer) = self.buffer.upgrade() else {
            return;
        };
        let Some(server_id) = self.server_id.or_else(|| {
            let project = project.read(cx);
            let worktree = project.worktree_for_id(self.path.worktree_id, cx)?;
            let root = worktree.read(cx).abs_path();
            let root = if project.path_style(cx).is_windows() {
                root.to_string_lossy().replace('\\', "/")
            } else {
                root.to_string_lossy().into_owned()
            };
            project
                .language_server_statuses(cx)
                .find_map(|(id, status)| {
                    (status.name
                        == language::LanguageServerName::new_static("httpyac-language-server")
                        && (status.worktree == Some(self.path.worktree_id)
                            || status.workspace_folders.iter().any(|folder| {
                                // Encode with the URL's own platform-independent path handling.
                                // from_file_path on a Windows client cannot encode a Unix remote root.
                                let Ok(mut expected) = folder.join("/") else {
                                    return false;
                                };
                                expected.set_path(&root);
                                expected.path().trim_end_matches('/')
                                    == folder.path().trim_end_matches('/')
                            })))
                    .then_some(id)
                })
        }) else {
            return;
        };
        let identity = (server_id, self.revision);
        if self.synchronized.as_ref() == Some(&identity) {
            return;
        }
        let Some(path) = project.read(cx).absolute_path(&self.path, cx) else {
            return;
        };
        let store = project.read(cx).lsp_store();
        let task = store.update(cx, |store, cx| store.execute_lsp_command(
            server_id, "httpyac.setEnvironment".into(),
            vec![serde_json::json!({"path": path, "environments": environments, "revision": self.revision})], cx,
        ));
        self.synchronized = Some(identity);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.synchronized.as_ref() == Some(&identity) {
                    this.error = result
                        .err()
                        .map(|error| format!("HTTP environment synchronization: {error:#}"));
                    if this.error.is_some() {
                        this.synchronized = None;
                    }
                    cx.notify();
                }
            })
            .log_err();
        })
        .detach();
    }
}
