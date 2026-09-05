pub(crate) struct GitGraphNextDiff {
    editor: gpui::Entity<editor::SplittableEditor>,
    load_error: Option<gpui::SharedString>,
    _load_task: gpui::Task<anyhow::Result<()>>,
}

impl GitGraphNextDiff {
    pub(crate) fn new(
        commit_sha: git::Oid,
        commit_diff: project::git_store::CommitDiff,
        showing_all_lines: bool,
        repository: gpui::Entity<project::git_store::Repository>,
        project: gpui::Entity<project::Project>,
        workspace: gpui::Entity<workspace::Workspace>,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let language_registry = project.read(cx).languages().clone();
        let multibuffer = gpui::AppContext::new(cx, |cx| {
            let mut multibuffer =
                editor::MultiBuffer::without_headers(language::Capability::ReadOnly);
            multibuffer.set_all_diff_hunks_expanded(cx);
            multibuffer
        });
        let editor = gpui::AppContext::new(cx, |cx| {
            let editor = editor::SplittableEditor::new(
                <editor::EditorSettings as settings::Settings>::get_global(cx).diff_view_style,
                multibuffer,
                project.clone(),
                workspace,
                window,
                cx,
            );
            editor.set_diff_hunk_renderer(
                Some(std::sync::Arc::new(editor::HiddenDiffHunkRenderer)),
                cx,
            );
            editor.rhs_editor().update(cx, |editor, cx| {
                editor.set_show_bookmarks(false, cx);
                editor.set_show_breakpoints(false, cx);
                editor.set_show_vertical_scrollbar(true, cx);
                editor.set_allow_git_diff_scrollbar_markers(showing_all_lines, cx);
            });
            editor
        });

        let repository_for_load = repository;
        let project_for_load = project;
        let editor_for_load = editor.clone();
        let load_task = cx.spawn_in(window, async move |this, cx| {
            let result: anyhow::Result<()> = async {
                for commit_file in commit_diff.files {
                    let is_deleted = commit_file.new_text.is_none();
                    let is_binary = commit_file.is_binary;
                    let new_text = if is_binary {
                        "(binary file not shown)".to_string()
                    } else {
                        commit_file.new_text.unwrap_or_default()
                    };
                    let old_text = if is_binary {
                        None
                    } else {
                        commit_file.old_text
                    };
                    let worktree_id = repository_for_load
                        .read_with(cx, |repository, cx| {
                            crate::commit_view::worktree_id_for_repo_path(
                                repository,
                                project_for_load.read(cx),
                                &commit_file.path,
                                cx,
                            )
                        })
                        .ok_or_else(|| anyhow::anyhow!("project has no worktrees"))?;
                    let commit_sha = commit_sha.to_string();
                    let short_sha = commit_sha
                        .get(0..git::SHORT_SHA_LENGTH)
                        .unwrap_or(commit_sha.as_str());
                    let file_name = commit_file
                        .path
                        .file_name()
                        .map(|name| name.to_string())
                        .unwrap_or_else(|| {
                            commit_file
                                .path
                                .display(util::paths::PathStyle::local())
                                .to_string()
                        });
                    let file = std::sync::Arc::new(crate::commit_view::GitBlob {
                        path: commit_file.path,
                        worktree_id,
                        is_deleted,
                        is_binary,
                        display_name: format!("{short_sha} - {file_name}"),
                    }) as std::sync::Arc<dyn language::File>;
                    let buffer =
                        crate::commit_view::build_buffer(new_text, file, &language_registry, cx)
                            .await?;

                    let buffer_diff = if is_binary {
                        cx.update(|_, cx| {
                            let snapshot = buffer.read(cx).snapshot();
                            gpui::AppContext::new(cx, |cx| {
                                buffer_diff::BufferDiff::new_unchanged(
                                    &snapshot,
                                    snapshot.language().cloned(),
                                    Some(language_registry.clone()),
                                    cx,
                                )
                            })
                        })?
                    } else {
                        build_buffer_diff(old_text, &buffer, &language_registry, cx).await?
                    };

                    let (path, ranges, context_line_count) = cx.update(|_, cx| {
                        let snapshot = buffer.read(cx).snapshot();
                        let file = snapshot
                            .file()
                            .ok_or_else(|| anyhow::anyhow!("commit buffer has no file"))?;
                        let path = multi_buffer::PathKey::with_sort_prefix(1, file.path().clone());
                        if showing_all_lines {
                            return anyhow::Ok((
                                path,
                                vec![language::Point::zero()..snapshot.max_point()],
                                0,
                            ));
                        }

                        let diff_snapshot = buffer_diff.read(cx).snapshot(cx);
                        let mut hunks = diff_snapshot.hunks(&snapshot).peekable();
                        let ranges = if is_binary || hunks.peek().is_none() {
                            vec![language::Point::zero()..snapshot.max_point()]
                        } else {
                            hunks
                                .map(|hunk| {
                                    language::OffsetRangeExt::to_point(
                                        &hunk.buffer_range,
                                        &snapshot,
                                    )
                                })
                                .collect()
                        };
                        anyhow::Ok((path, ranges, editor::multibuffer_context_lines(cx)))
                    })??;

                    editor_for_load.update(cx, |editor, cx| {
                        editor.update_excerpts_for_path(
                            path,
                            buffer,
                            ranges,
                            context_line_count,
                            buffer_diff,
                            cx,
                        );
                    });
                }

                anyhow::Ok(())
            }
            .await;

            if let Err(error) = result {
                this.update(cx, |this, cx| {
                    this.load_error = Some(error.to_string().into());
                    cx.notify();
                })?;
            }
            anyhow::Ok(())
        });

        Self {
            editor,
            load_error: None,
            _load_task: load_task,
        }
    }

    pub(crate) fn editor(&self) -> gpui::Entity<editor::SplittableEditor> {
        self.editor.clone()
    }

    pub(crate) fn load_error(&self) -> Option<gpui::SharedString> {
        self.load_error.clone()
    }

    pub(crate) fn go_to_previous_hunk(
        &self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        window.focus(&gpui::Focusable::focus_handle(&self.editor, cx), cx);
        window.dispatch_action(Box::new(editor::actions::GoToPreviousHunk), cx);
    }

    pub(crate) fn go_to_next_hunk(&self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        window.focus(&gpui::Focusable::focus_handle(&self.editor, cx), cx);
        window.dispatch_action(Box::new(editor::actions::GoToHunk), cx);
    }

    pub(crate) fn toggle_soft_wrap(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let (right_editor, left_editor) = {
            let editor = self.editor.read(cx);
            (editor.rhs_editor().clone(), editor.lhs_editor().cloned())
        };
        right_editor.update(cx, |editor, cx| {
            editor.toggle_soft_wrap(&editor::actions::ToggleSoftWrap, window, cx);
        });
        if let Some(left_editor) = left_editor {
            left_editor.update(cx, |editor, cx| {
                editor.toggle_soft_wrap(&editor::actions::ToggleSoftWrap, window, cx);
            });
        }
    }
}

async fn build_buffer_diff(
    mut old_text: Option<String>,
    buffer: &gpui::Entity<language::Buffer>,
    language_registry: &std::sync::Arc<language::LanguageRegistry>,
    cx: &mut gpui::AsyncWindowContext,
) -> anyhow::Result<gpui::Entity<buffer_diff::BufferDiff>> {
    if let Some(old_text) = &mut old_text {
        language::LineEnding::normalize(old_text);
    }

    let language = cx.update(|_, cx| buffer.read(cx).language().cloned())?;
    let buffer_snapshot = cx.update(|_, cx| buffer.read(cx).snapshot())?;
    let diff = gpui::AppContext::new(cx, |cx| {
        buffer_diff::BufferDiff::new(
            &buffer_snapshot.text,
            language,
            Some(language_registry.clone()),
            cx,
        )
    });
    diff.update(cx, |diff, cx| {
        diff.set_base_text(
            old_text.map(|old_text| std::sync::Arc::from(old_text.as_str())),
            buffer_snapshot.text.clone(),
            cx,
        )
    })
    .await;

    Ok(diff)
}
