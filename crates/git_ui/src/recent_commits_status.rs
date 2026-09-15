use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _,
};
use settings::Settings as _;
use theme::ActiveTheme as _;
use ui::{
    ButtonCommon as _, Clickable as _, Disableable as _, LabelCommon as _, StyledExt as _,
    WithScrollbar as _,
};
use util::ResultExt as _;

const PAGE_SIZE: usize = 8;

pub struct RecentCommitsStatus {
    git_store: gpui::Entity<project::git_store::GitStore>,
    workspace: gpui::WeakEntity<workspace::Workspace>,
    history: Option<gpui::Entity<RecentCommitsPopover>>,
    menu_handle: ui::PopoverMenuHandle<RecentCommitsPopover>,
}

impl RecentCommitsStatus {
    pub fn new(workspace: &workspace::Workspace, cx: &mut gpui::Context<Self>) -> Self {
        let git_store = workspace.project().read(cx).git_store().clone();
        cx.subscribe(&git_store, |this, _, event, cx| match event {
            project::git_store::GitStoreEvent::ActiveRepositoryChanged(_)
            | project::git_store::GitStoreEvent::RepositoryAdded
            | project::git_store::GitStoreEvent::RepositoryRemoved(_)
            | project::git_store::GitStoreEvent::RepositoryUpdated(
                _,
                project::git_store::RepositoryEvent::HeadChanged,
                true,
            ) => this.refresh(cx),
            _ => {}
        })
        .detach();
        cx.observe_global::<settings::SettingsStore>(|this, cx| this.refresh(cx))
            .detach();
        let mut this = Self {
            git_store,
            workspace: workspace.weak_handle(),
            history: None,
            menu_handle: ui::PopoverMenuHandle::default(),
        };
        this.refresh(cx);
        this
    }

    fn enabled(cx: &gpui::App) -> bool {
        project::project_settings::ProjectSettings::get_global(cx)
            .git
            .enabled
            .status
    }

    fn refresh(&mut self, cx: &mut gpui::Context<Self>) {
        let target = Self::enabled(cx)
            .then(|| {
                let repository = self.git_store.read(cx).active_repository()?;
                let snapshot = repository.read(cx);
                let head = snapshot.head_commit.clone()?;
                let branch: gpui::SharedString = snapshot
                    .branch
                    .as_ref()
                    .map(|branch| branch.name().to_owned().into())
                    .unwrap_or_else(|| "Detached HEAD".into());
                Some((repository.clone(), head, branch))
            })
            .flatten();
        if let Some((repository, head, branch)) = &target {
            if self.history.as_ref().is_some_and(|history| {
                let history = history.read(cx);
                history.repository == *repository
                    && history.head.sha == head.sha
                    && history.branch == *branch
            }) {
                cx.notify();
                return;
            }
        }
        self.menu_handle.hide(cx);
        if let Some(history) = self.history.take() {
            history.update(cx, |history, _| {
                history.page_task = None;
                history.detail_task = None;
            });
        }
        self.history = target.and_then(|(repository, head, branch)| {
            let head_sha = head.sha.parse::<git::Oid>().log_err()?;
            Some(cx.new(|cx| RecentCommitsPopover {
                repository,
                git_store: self.git_store.clone(),
                workspace: self.workspace.clone(),
                head,
                head_sha,
                branch,
                commits: Vec::new(),
                has_more: true,
                page_task: None,
                page_error: None,
                head_details: None,
                detail_task: None,
                detail_error: None,
                selected_index: 0,
                scroll_handle: gpui::UniformListScrollHandle::new(),
                focus_handle: cx.focus_handle(),
            }))
        });
        cx.notify();
    }
}

impl workspace::StatusItemView for RecentCommitsStatus {
    fn set_active_pane_item(
        &mut self,
        _: Option<&dyn workspace::ItemHandle>,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) {
    }

    fn hide_setting(&self, _: &gpui::App) -> Option<workspace::HideStatusItem> {
        None
    }
}

impl gpui::Render for RecentCommitsStatus {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        if !Self::enabled(cx) || self.git_store.read(cx).active_repository().is_none() {
            return gpui::div().into_any_element();
        }
        let Some(history) = self.history.clone() else {
            return ui::Button::new("git-last-commit", "No HEAD commit")
                .label_size(ui::LabelSize::Small)
                .disabled(true)
                .into_any_element();
        };
        let subject = history
            .read(cx)
            .head
            .message
            .lines()
            .next()
            .filter(|subject| !subject.is_empty())
            .unwrap_or("(no commit message)")
            .to_owned();
        let tooltip_history = history.clone();
        gpui::div()
            .id("git-last-commit-status")
            .max_w(gpui::rems(24.))
            .min_w_0()
            .child(
                ui::PopoverMenu::new("git-recent-commits")
                    .with_handle(self.menu_handle.clone())
                    .anchor(gpui::Anchor::BottomLeft)
                    .attach(gpui::Anchor::TopLeft)
                    .trigger(
                        ui::Button::new("git-last-commit", subject)
                            .label_size(ui::LabelSize::Small)
                            .truncate(true)
                            .tab_index(0isize)
                            .start_icon(
                                ui::Icon::new(ui::IconName::FileGit).size(ui::IconSize::Small),
                            ),
                    )
                    .menu(move |_, cx| {
                        history.update(cx, |history, cx| {
                            if history.commits.is_empty() {
                                history.load_more(cx);
                            }
                        });
                        Some(history.clone())
                    }),
            )
            .when(!self.menu_handle.is_deployed(), |element| {
                element.hoverable_tooltip(move |_, cx| {
                    tooltip_history.update(cx, |history, cx| history.load_head_details(cx));
                    cx.new(|cx| {
                        cx.observe(&tooltip_history, |_, _, cx| cx.notify())
                            .detach();
                        RecentCommitTooltip {
                            history: Some(tooltip_history.clone()),
                            commit: None,
                            scroll_handle: gpui::ScrollHandle::new(),
                        }
                    })
                    .into()
                })
            })
            .into_any_element()
    }
}

struct RecentCommitsPopover {
    repository: gpui::Entity<project::git_store::Repository>,
    git_store: gpui::Entity<project::git_store::GitStore>,
    workspace: gpui::WeakEntity<workspace::Workspace>,
    head: git::repository::CommitDetails,
    head_sha: git::Oid,
    branch: gpui::SharedString,
    commits: Vec<git::repository::RecentCommit>,
    has_more: bool,
    page_task: Option<gpui::Task<()>>,
    page_error: Option<String>,
    head_details: Option<git::repository::RecentCommit>,
    detail_task: Option<gpui::Task<()>>,
    detail_error: Option<String>,
    selected_index: usize,
    scroll_handle: gpui::UniformListScrollHandle,
    focus_handle: gpui::FocusHandle,
}

impl RecentCommitsPopover {
    fn is_current(&self, cx: &gpui::App) -> bool {
        RecentCommitsStatus::enabled(cx)
            && self.git_store.read(cx).active_repository().as_ref() == Some(&self.repository)
            && self
                .repository
                .read(cx)
                .head_commit
                .as_ref()
                .is_some_and(|head| head.sha == self.head.sha)
    }

    fn load_head_details(&mut self, cx: &mut gpui::Context<Self>) {
        if self.head_details.is_some() || self.detail_task.is_some() || !self.is_current(cx) {
            return;
        }
        self.detail_error = None;
        let request = self.repository.update(cx, |repository, _| {
            repository.recent_commits(self.head_sha, 0, 1)
        });
        self.detail_task = Some(cx.spawn(async move |this, cx| {
            let result = request
                .await
                .map_err(anyhow::Error::from)
                .and_then(|result| result)
                .and_then(|commits| {
                    commits
                        .into_iter()
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("HEAD commit was not returned"))
                });
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| {
                    this.detail_task = None;
                    if !this.is_current(cx) {
                        return;
                    }
                    match result {
                        Ok(commit) => this.head_details = Some(commit),
                        Err(error) => {
                            log::error!("Failed to load HEAD commit details: {error:#}");
                            this.detail_error = Some(error.to_string());
                        }
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    fn load_more(&mut self, cx: &mut gpui::Context<Self>) {
        if self.page_task.is_some() || !self.has_more || !self.is_current(cx) {
            return;
        }
        self.page_error = None;
        let offset = self.commits.len();
        let request = self.repository.update(cx, |repository, _| {
            repository.recent_commits(self.head_sha, offset as u64, (PAGE_SIZE + 1) as u32)
        });
        self.page_task = Some(cx.spawn(async move |this, cx| {
            let result = request
                .await
                .map_err(anyhow::Error::from)
                .and_then(|result| result);
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| {
                    this.page_task = None;
                    if !this.is_current(cx) || this.commits.len() != offset {
                        return;
                    }
                    match result {
                        Ok(mut commits) => {
                            this.has_more = commits.len() > PAGE_SIZE;
                            commits.truncate(PAGE_SIZE);
                            if offset == 0 {
                                this.head_details = commits.first().cloned();
                                this.detail_error = None;
                            }
                            this.commits.extend(commits);
                        }
                        Err(error) => {
                            log::error!("Failed to load recent commits: {error:#}");
                            this.page_error = Some(error.to_string());
                        }
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    fn open_commit(
        &mut self,
        index: usize,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if !self.is_current(cx) {
            cx.emit(gpui::DismissEvent);
            return;
        }
        let Some(commit) = self.commits.get(index) else {
            return;
        };
        let sha = commit.sha.to_string();
        self.selected_index = index;
        let repository_id = self.repository.read(cx).id;
        let workspace = self.workspace.clone();
        let git_store = self.git_store.clone();
        cx.emit(gpui::DismissEvent);
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    crate::git_graph_next::open_or_reuse_graph_next(
                        workspace,
                        repository_id,
                        git_store,
                        git::repository::LogSource::All,
                        Some(sha),
                        window,
                        cx,
                    );
                })
                .log_err();
        });
    }

    fn move_selection(&mut self, next: bool, cx: &mut gpui::Context<Self>) {
        self.selected_index = if next {
            (self.selected_index + 1).min(self.commits.len().saturating_sub(1))
        } else {
            self.selected_index.saturating_sub(1)
        };
        self.scroll_handle
            .scroll_to_item(self.selected_index, gpui::ScrollStrategy::Nearest);
        if next && self.selected_index + 2 >= self.commits.len() && self.page_error.is_none() {
            self.load_more(cx);
        }
        cx.notify();
    }
}

impl gpui::Focusable for RecentCommitsPopover {
    fn focus_handle(&self, _: &gpui::App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui::EventEmitter<gpui::DismissEvent> for RecentCommitsPopover {}

impl gpui::Render for RecentCommitsPopover {
    fn render(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        let history = cx.entity();
        let commit_count = self.commits.len();
        let list = gpui::uniform_list("recent-commits-list", commit_count, move |range, _, cx| {
            let should_load = {
                let history = history.read(cx);
                range.end >= history.commits.len().saturating_sub(2)
                    && history.has_more
                    && history.page_task.is_none()
                    && history.page_error.is_none()
            };
            // The range renderer runs during prepaint; defer mutations until after the frame.
            if should_load {
                let history = history.downgrade();
                cx.defer(move |cx| {
                    if let Some(history) = history.upgrade() {
                        history.update(cx, |history, cx| history.load_more(cx));
                    }
                });
            }
            let state = history.read(cx);
            range
                .filter_map(|index| {
                    let commit = state.commits.get(index)?.clone();
                    let selected = index == state.selected_index;
                    let is_head = commit.sha == state.head_sha;
                    let tooltip_commit = commit.clone();
                    let history = history.downgrade();
                    Some(
                        gpui::div()
                            .id(("recent-commit", index))
                            .h(gpui::rems(3.25))
                            .px_2()
                            .py_1()
                            .cursor_pointer()
                            .when(selected, |element| {
                                element.bg(cx.theme().colors().element_selected)
                            })
                            .hover(|style| style.bg(cx.theme().colors().element_hover))
                            .child(
                                ui::v_flex()
                                    .gap_0p5()
                                    .child(
                                        ui::Label::new(if commit.subject.is_empty() {
                                            "(no commit message)".into()
                                        } else {
                                            commit.subject.clone()
                                        })
                                        .size(ui::LabelSize::Small)
                                        .truncate(),
                                    )
                                    .child(
                                        ui::Label::new(format!(
                                            "{}{} · {} · {}",
                                            if is_head { "HEAD · " } else { "" },
                                            commit.sha.display_short(),
                                            commit.committer_name,
                                            format_timestamp(commit.committer_timestamp, false)
                                        ))
                                        .size(ui::LabelSize::XSmall)
                                        .color(ui::Color::Muted)
                                        .truncate(),
                                    ),
                            )
                            .on_click(move |_, window, cx| {
                                if let Some(history) = history.upgrade() {
                                    history.update(cx, |history, cx| {
                                        history.open_commit(index, window, cx)
                                    });
                                }
                            })
                            .hoverable_tooltip(move |_, cx| {
                                cx.new(|_| RecentCommitTooltip {
                                    history: None,
                                    commit: Some(tooltip_commit.clone()),
                                    scroll_handle: gpui::ScrollHandle::new(),
                                })
                                .into()
                            }),
                    )
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.scroll_handle)
        .size_full();
        let footer = if let Some(error) = &self.page_error {
            ui::Button::new("retry-recent-commits", "Failed to load commits · Retry")
                .label_size(ui::LabelSize::Small)
                .tooltip(ui::Tooltip::text(error.clone()))
                .on_click(cx.listener(|this, _, _, cx| this.load_more(cx)))
                .into_any_element()
        } else {
            ui::Label::new(if self.page_task.is_some() {
                "Loading commits…"
            } else if self.has_more {
                "Scroll for older commits"
            } else {
                "No more commits"
            })
            .size(ui::LabelSize::Small)
            .color(ui::Color::Muted)
            .into_any_element()
        };
        ui::v_flex()
            .id("recent-commits-popover")
            .debug_selector(|| "RECENT_COMMITS_POPOVER".into())
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(gpui::DismissEvent)))
            .key_context("RecentCommits")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| cx.emit(gpui::DismissEvent)))
            .on_action(cx.listener(|this, _: &menu::Confirm, window, cx| {
                this.open_commit(this.selected_index, window, cx);
            }))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.modifiers.modified() {
                    return;
                }
                match event.keystroke.key.as_str() {
                    "up" => this.move_selection(false, cx),
                    "down" => this.move_selection(true, cx),
                    "enter" => this.open_commit(this.selected_index, window, cx),
                    "escape" => cx.emit(gpui::DismissEvent),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .elevation_2(cx)
            .w(gpui::rems(34.))
            .max_w(ui::vw(0.9, window))
            .child(
                gpui::div().px_2().py_1().child(
                    ui::Label::new(format!("{} · Recent commits", self.branch))
                        .size(ui::LabelSize::Small)
                        .truncate()
                        .into_any_element(),
                ),
            )
            .child(ui::Divider::horizontal())
            .child(
                gpui::div()
                    .id("recent-commits-scroll")
                    .h(gpui::rems(3.25 * commit_count.clamp(1, 5) as f32))
                    .max_h(ui::vh(0.5, window))
                    .overflow_hidden()
                    .child(list)
                    .vertical_scrollbar_for(&self.scroll_handle, window, cx),
            )
            .child(ui::Divider::horizontal())
            .child(gpui::div().px_2().py_1().child(footer))
    }
}

struct RecentCommitTooltip {
    history: Option<gpui::Entity<RecentCommitsPopover>>,
    commit: Option<git::repository::RecentCommit>,
    scroll_handle: gpui::ScrollHandle,
}

impl gpui::Render for RecentCommitTooltip {
    fn render(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        let history = self.history.as_ref().map(|history| history.read(cx));
        let commit = self
            .commit
            .as_ref()
            .or_else(|| history.and_then(|history| history.head_details.as_ref()));
        let message = commit
            .map(|commit| commit.message.clone())
            .or_else(|| history.map(|history| history.head.message.clone()))
            .unwrap_or_default();
        let metadata = if let Some(commit) = commit {
            format!(
                "{}\nCommitter: {} <{}>\nCommitted: {}",
                commit.sha,
                commit.committer_name,
                commit.committer_email,
                format_timestamp(commit.committer_timestamp, true)
            )
        } else if let Some(error) = history.and_then(|history| history.detail_error.as_ref()) {
            format!("Failed to load commit details: {error}\nHover again to retry.")
        } else {
            "Loading commit details…".to_owned()
        };
        let branch = history.map(|history| history.branch.clone());
        ui::tooltip_container(cx, |element, _| {
            element.occlude().child(
                ui::v_flex()
                    .w(gpui::rems(32.))
                    .max_w(ui::vw(0.9, window))
                    .gap_1()
                    .when_some(branch, |element, branch| {
                        element.child(ui::Label::new(branch).size(ui::LabelSize::Small))
                    })
                    .child(gpui::div().text_size(gpui::rems(0.8)).child(metadata))
                    .child(ui::Divider::horizontal())
                    .child(
                        gpui::div()
                            .id("recent-commit-message")
                            .max_h(ui::vh(0.4, window))
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll_handle)
                            .text_size(gpui::rems(0.8))
                            .child(message),
                    ),
            )
        })
    }
}

fn format_timestamp(timestamp: i64, absolute: bool) -> String {
    let Ok(timestamp) = time::OffsetDateTime::from_unix_timestamp(timestamp) else {
        return "Unknown time".to_owned();
    };
    let offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    let formatted = time_format::format_localized_timestamp(
        timestamp,
        time::OffsetDateTime::now_utc(),
        offset,
        if absolute {
            time_format::TimestampFormat::Absolute
        } else {
            time_format::TimestampFormat::Relative
        },
    );
    if absolute {
        format!("{formatted} {offset}")
    } else {
        formatted
    }
}

#[cfg(test)]
mod tests {
    use gpui::AppContext as _;

    async fn setup(
        count: u8,
        cx: &mut gpui::TestAppContext,
    ) -> (
        std::sync::Arc<fs::FakeFs>,
        gpui::Entity<super::RecentCommitsStatus>,
        gpui::Entity<workspace::Workspace>,
    ) {
        cx.update(|cx| {
            let settings = settings::SettingsStore::test(cx);
            cx.set_global(settings);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            language_model::init(cx);
            crate::init(cx);
        });
        let fs = fs::FakeFs::new(cx.executor());
        let root = std::path::Path::new(util::path!("/project"));
        let dot_git = root.join(".git");
        fs.insert_tree(root, serde_json::json!({".git": {}, "file.txt": "content"}))
            .await;
        let commits = (1..=count)
            .rev()
            .map(|index| {
                std::sync::Arc::new(git::repository::InitialGraphCommitData {
                    sha: git::Oid::from_bytes(&[index; 20]).unwrap(),
                    parents: if index > 1 {
                        smallvec::smallvec![git::Oid::from_bytes(&[index - 1; 20]).unwrap()]
                    } else {
                        smallvec::smallvec![]
                    },
                    ref_names: Vec::new(),
                })
            })
            .collect::<Vec<_>>();
        fs.set_commit_data(
            &dot_git,
            commits.iter().map(|commit| {
                (
                    git::repository::CommitData {
                        sha: commit.sha,
                        parents: commit.parents.clone(),
                        author_name: "Author".into(),
                        author_email: "author@example.com".into(),
                        commit_timestamp: 1_700_000_000,
                        subject: format!("Commit {}", commit.sha.display_short()).into(),
                        message: "Subject\n\nBody".into(),
                    },
                    false,
                )
            }),
        );
        fs.set_graph_commits(&dot_git, commits);
        fs.set_head_for_repo(
            &dot_git,
            &[],
            git::Oid::from_bytes(&[count; 20]).unwrap().to_string(),
        );
        let project = project::Project::test(fs.clone(), [root], cx).await;
        cx.run_until_parked();
        let window = cx.add_window(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = window
            .read_with(cx, |multi, _| multi.workspace().clone())
            .unwrap();
        let status = workspace.update(cx, |workspace, cx| {
            cx.new(|cx| super::RecentCommitsStatus::new(workspace, cx))
        });
        (fs, status, workspace)
    }

    #[gpui::test]
    async fn test_recent_commits_pages_retry_and_exhaustion(cx: &mut gpui::TestAppContext) {
        let (fs, status, _) = setup(18, cx).await;
        let history = status.read_with(cx, |status, _| status.history.clone().unwrap());
        history.read_with(cx, |history, _| assert!(history.commits.is_empty()));
        history.update(cx, |history, cx| history.load_head_details(cx));
        cx.run_until_parked();
        history.read_with(cx, |history, _| {
            assert!(history.head_details.is_some());
            assert!(history.commits.is_empty());
        });
        history.update(cx, |history, cx| {
            history.load_more(cx);
            history.load_more(cx);
        });
        cx.run_until_parked();
        history.read_with(cx, |history, cx| {
            assert_eq!(history.commits.len(), 8);
            assert!(history.has_more);
            assert!(
                history
                    .repository
                    .read(cx)
                    .get_graph_data(
                        git::repository::LogSource::All,
                        git::repository::LogOrder::DateOrder,
                    )
                    .is_none()
            );
        });
        let dot_git = std::path::Path::new(util::path!("/project/.git"));
        fs.set_graph_error(dot_git, Some("test failure".to_owned()));
        history.update(cx, |history, cx| history.load_more(cx));
        cx.run_until_parked();
        history.read_with(cx, |history, _| {
            assert_eq!(history.commits.len(), 8);
            assert!(history.page_error.is_some());
            assert!(history.page_task.is_none());
        });
        fs.set_graph_error(dot_git, None);
        history.update(cx, |history, cx| history.load_more(cx));
        cx.run_until_parked();
        history.read_with(cx, |history, _| {
            assert_eq!(history.commits.len(), 16);
            assert!(history.page_error.is_none());
            assert!(history.has_more);
        });
        history.update(cx, |history, cx| history.load_more(cx));
        cx.run_until_parked();
        history.read_with(cx, |history, _| {
            assert_eq!(history.commits.len(), 18);
            assert!(!history.has_more);
            assert_eq!(
                history
                    .commits
                    .iter()
                    .map(|commit| commit.sha)
                    .collect::<Vec<_>>(),
                (1..=18)
                    .rev()
                    .map(|index| git::Oid::from_bytes(&[index; 20]).unwrap())
                    .collect::<Vec<_>>()
            );
        });
        history.update(cx, |history, cx| {
            history.load_more(cx);
            assert!(history.page_task.is_none());
        });
    }

    #[gpui::test]
    async fn test_recent_commits_scroll_and_open_next(cx: &mut gpui::TestAppContext) {
        let (_, status, workspace) = setup(24, cx).await;
        let window = *cx.windows().first().unwrap();
        let cx = &mut gpui::VisualTestContext::from_window(window, cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.status_bar().update(cx, |bar, cx| {
                bar.add_left_item(status.clone(), window, cx);
            });
        });
        cx.refresh().unwrap();
        cx.run_until_parked();
        let handle = status.read_with(cx, |status, _| status.menu_handle.clone());
        cx.update(|window, cx| handle.show(window, cx));
        cx.run_until_parked();
        let history = status.read_with(cx, |status, _| status.history.clone().unwrap());
        history.read_with(cx, |history, _| assert_eq!(history.commits.len(), 8));
        history.update(cx, |history, cx| {
            history
                .scroll_handle
                .scroll_to_item(7, gpui::ScrollStrategy::Bottom);
            cx.notify();
        });
        cx.refresh().unwrap();
        cx.run_until_parked();
        history.read_with(cx, |history, _| {
            assert_eq!(history.commits.len(), 16);
            assert!(history.scroll_handle.0.borrow().base_handle.offset().y < gpui::px(0.));
        });
        let sha = history.read_with(cx, |history, _| history.commits[10].sha);
        history.update_in(cx, |history, window, cx| {
            history.open_commit(10, window, cx)
        });
        cx.run_until_parked();
        assert!(!handle.is_deployed());
        workspace.read_with(cx, |workspace, cx| {
            let graph = workspace
                .active_item_as::<crate::git_graph_next::GitGraphNext>(cx)
                .unwrap();
            assert_eq!(graph.read(cx).selected_commit_sha_for_test(), Some(sha));
            assert_eq!(
                workspace
                    .items_of_type::<crate::git_graph::GitGraph>(cx)
                    .count(),
                0
            );
        });
        cx.update(|window, cx| handle.show(window, cx));
        cx.run_until_parked();
        history.update_in(cx, |history, window, cx| history.open_commit(0, window, cx));
        cx.run_until_parked();
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(
                workspace
                    .items_of_type::<crate::git_graph_next::GitGraphNext>(cx)
                    .count(),
                1
            );
            let graph = workspace
                .active_item_as::<crate::git_graph_next::GitGraphNext>(cx)
                .unwrap();
            assert_eq!(
                graph.read(cx).selected_commit_sha_for_test(),
                Some(git::Oid::from_bytes(&[24; 20]).unwrap())
            );
        });
    }

    #[gpui::test]
    async fn test_recent_commits_dismiss_on_outside_click(cx: &mut gpui::TestAppContext) {
        let (_, status, workspace) = setup(18, cx).await;
        let window = *cx.windows().first().unwrap();
        let cx = &mut gpui::VisualTestContext::from_window(window, cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.status_bar().update(cx, |bar, cx| {
                bar.add_left_item(status.clone(), window, cx);
            });
        });
        cx.refresh().unwrap();
        cx.run_until_parked();
        let handle = status.read_with(cx, |status, _| status.menu_handle.clone());
        for button in [gpui::MouseButton::Left, gpui::MouseButton::Right] {
            cx.update(|window, cx| handle.show(window, cx));
            cx.run_until_parked();
            assert!(handle.is_deployed());
            let bounds = cx.debug_bounds("RECENT_COMMITS_POPOVER").unwrap();
            let header = bounds.origin + gpui::point(gpui::px(8.), gpui::px(8.));
            cx.simulate_click(header, gpui::Modifiers::none());
            assert!(handle.is_deployed());
            let outside = bounds.origin + gpui::point(gpui::px(8.), gpui::px(-8.));
            cx.simulate_mouse_down(outside, button, gpui::Modifiers::none());
            assert!(!handle.is_deployed());
            cx.simulate_mouse_up(outside, button, gpui::Modifiers::none());
        }
        cx.update(|window, cx| handle.show(window, cx));
        cx.run_until_parked();
        assert!(handle.is_deployed());
        status.read_with(cx, |status, cx| {
            assert_eq!(status.history.as_ref().unwrap().read(cx).commits.len(), 8);
        });
    }

    #[gpui::test(iterations = 5)]
    async fn test_recent_commits_head_change_discards_old_pages(cx: &mut gpui::TestAppContext) {
        let (fs, status, _) = setup(18, cx).await;
        let old_history = status.read_with(cx, |status, _| status.history.clone().unwrap());
        old_history.update(cx, |history, cx| history.load_more(cx));
        let head = git::Oid::from_bytes(&[3; 20]).unwrap();
        fs.set_head_for_repo(
            std::path::Path::new(util::path!("/project/.git")),
            &[],
            head.to_string(),
        );
        cx.run_until_parked();
        let history = status.read_with(cx, |status, _| status.history.clone().unwrap());
        assert_ne!(history, old_history);
        history.read_with(cx, |history, _| {
            assert_eq!(history.head_sha, head);
            assert!(history.commits.is_empty());
        });
        old_history.update(cx, |history, cx| {
            assert!(!history.is_current(cx));
            history.load_more(cx);
            assert!(history.page_task.is_none());
        });
        history.update(cx, |history, cx| history.load_more(cx));
        cx.run_until_parked();
        history.read_with(cx, |history, _| {
            assert_eq!(history.commits.len(), 3);
            assert_eq!(history.commits[0].sha, head);
            assert!(!history.has_more);
        });
    }
}
