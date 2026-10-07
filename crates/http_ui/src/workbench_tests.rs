use gpui::{AppContext as _, Focusable as _};

fn language() -> std::sync::Arc<language::Language> {
    let grammar = grammars::native_grammars()
        .into_iter()
        .find(|(name, _)| *name == "httpyac")
        .unwrap()
        .1;
    std::sync::Arc::new(language::Language::new(
        grammars::load_config("http"),
        Some(grammar),
    ))
}

const DOCUMENT: &str = "@baseUrl = https://example.test\n\n###\n# @name user__2_list\nGET {{baseUrl}}/users\n\n###\n# zed-group: 01_认证/登录\n# @name login\n# @title 01_登录\nPOST {{baseUrl}}/login\nContent-Type: application/json\n\n{\"name\": \"hello\"}\n";

#[gpui::test]
async fn request_index_includes_metadata_but_not_global_regions(cx: &mut gpui::TestAppContext) {
    crate::tests::init(cx);
    let buffer = cx.new(|cx| language::Buffer::local(DOCUMENT, cx).with_language(language(), cx));
    cx.run_until_parked();
    buffer.read_with(cx, |buffer, _| {
        let snapshot = buffer.snapshot();
        let (requests, context) =
            crate::requests::index(&snapshot, &[], &mut 0, Default::default()).unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].name, "user__2_list");
        assert_eq!(requests[0].group, ["user"]);
        assert_eq!(requests[1].group, ["01_认证", "登录"]);
        assert_eq!(requests[1].title, "01_登录");
        assert!(requests[1].source.starts_with("# zed-group:"));
        assert!(requests[1].source.ends_with("{\"name\": \"hello\"}\n"));
        assert_eq!(context.len(), 1);
        assert!(
            snapshot
                .text_for_range(context[0].clone())
                .collect::<String>()
                .contains("@baseUrl")
        );
    });
}

#[gpui::test]
async fn request_mode_edits_the_original_buffer_and_keeps_identity(cx: &mut gpui::TestAppContext) {
    crate::tests::init(cx);
    let buffer = cx.new(|cx| language::Buffer::local(DOCUMENT, cx).with_language(language(), cx));
    let (source, cx) = cx
        .add_window_view(|window, cx| editor::Editor::for_buffer(buffer.clone(), None, window, cx));
    cx.run_until_parked();
    let workbench = source.read_with(cx, |source, _| {
        source
            .addon::<crate::EmbeddedResponse>()
            .unwrap()
            .workbench
            .clone()
            .unwrap()
    });
    let (first, second) = workbench.read_with(cx, |workbench, _| {
        (workbench.requests[0].id, workbench.requests[1].id)
    });
    workbench.update_in(cx, |workbench, window, cx| {
        workbench.toggle_mode(window, cx);
        workbench.select(Some(first), window, cx);
    });
    cx.run_until_parked();
    let request_editor = workbench.read_with(cx, |workbench, _| workbench.editor.clone());
    request_editor.read_with(cx, |editor, cx| {
        assert!(editor.text(cx).contains("# @name user__2_list"));
        assert!(!editor.text(cx).contains("POST"));
    });
    request_editor.update_in(cx, |editor, window, cx| {
        let point = language::Point::new(1, 0);
        editor.change_selections(
            editor::SelectionEffects::default(),
            window,
            cx,
            |selection| selection.select_ranges([point..language::Point::new(1, 3)]),
        );
        editor.insert("HEAD", window, cx);
    });
    cx.run_until_parked();
    assert!(buffer.read_with(cx, |buffer, _| {
        buffer.text().contains("HEAD {{baseUrl}}/users")
    }));
    workbench.read_with(cx, |workbench, _| {
        assert_eq!(workbench.requests[0].id, first);
        assert_eq!(workbench.requests[1].id, second);
        assert_eq!(workbench.requests[0].method, "HEAD");
    });
    workbench.update_in(cx, |workbench, window, cx| {
        workbench.toggle_mode(window, cx)
    });
    cx.run_until_parked();
    assert!(source.read_with(cx, |source, cx| {
        source.text(cx).contains("HEAD {{baseUrl}}/users")
    }));
    cx.update(|window, cx| assert!(source.focus_handle(cx).is_focused(window)));
    source.update_in(cx, |source, window, cx| {
        source.undo(&editor::actions::Undo, window, cx)
    });
    cx.run_until_parked();
    assert!(buffer.read_with(cx, |buffer, _| {
        buffer.text().contains("GET {{baseUrl}}/users")
    }));
    source.update_in(cx, |source, window, cx| {
        source.redo(&editor::actions::Redo, window, cx)
    });
    cx.run_until_parked();
    assert!(buffer.read_with(cx, |buffer, _| {
        buffer.text().contains("HEAD {{baseUrl}}/users")
    }));
}

#[gpui::test]
async fn histories_are_per_request_and_environment_and_do_not_steal_selection(
    cx: &mut gpui::TestAppContext,
) {
    crate::tests::init(cx);
    let buffer = cx.new(|cx| language::Buffer::local(DOCUMENT, cx).with_language(language(), cx));
    let (source, cx) = cx
        .add_window_view(|window, cx| editor::Editor::for_buffer(buffer.clone(), None, window, cx));
    cx.run_until_parked();
    let workbench = source.read_with(cx, |source, _| {
        source
            .addon::<crate::EmbeddedResponse>()
            .unwrap()
            .workbench
            .clone()
            .unwrap()
    });
    workbench.update_in(cx, |workbench, window, cx| {
        workbench.request_mode = true;
        let first = workbench.requests[0].id;
        let second = workbench.requests[1].id;
        workbench.select(Some(first), window, cx);
        workbench.select_environment(super::Environment::Named("dev".into()), cx);
        let mut task = task::SpawnInTerminal::default();
        task.env.insert("ZED_ROW".into(), "5".into());
        let dev = workbench.prepare_run(&mut task, window, cx);
        workbench.select(Some(second), window, cx);
        assert_ne!(workbench.response, dev);
        dev.update(cx, |view, _| view.status = "Finished dev".into());
        workbench.finished(cx);
        assert_eq!(workbench.selected, Some(second));
        workbench.select(Some(first), window, cx);
        assert_eq!(workbench.response, dev);
        workbench.select_environment(super::Environment::Named("prod".into()), cx);
        assert_eq!(workbench.response, workbench.empty);
        let prod = workbench.prepare_run(&mut task, window, cx);
        workbench.finished(cx);
        assert_ne!(dev, prod);
        assert_eq!(task.env.get("ZED_HTTPYAC_ENV").unwrap(), "prod");
        workbench.select_environment(super::Environment::Named("dev".into()), cx);
        assert_eq!(workbench.response, dev);
        assert_eq!(workbench.response.read(cx).status, "Finished dev");
    });
    buffer.update(cx, |buffer, cx| {
        buffer.edit(
            [(
                DOCUMENT.find("/users").unwrap()..DOCUMENT.find("/users").unwrap() + 6,
                "/people",
            )],
            None,
            cx,
        );
    });
    cx.run_until_parked();
    workbench.read_with(cx, |workbench, cx| {
        assert!(workbench.response.read(cx).stale)
    });
}

#[gpui::test]
async fn releasing_source_releases_active_run_and_excerpt(cx: &mut gpui::TestAppContext) {
    crate::tests::init(cx);
    let buffer = cx.new(|cx| language::Buffer::local(DOCUMENT, cx).with_language(language(), cx));
    let (source, cx) =
        cx.add_window_view(|window, cx| editor::Editor::for_buffer(buffer, None, window, cx));
    cx.run_until_parked();
    let workbench = source.read_with(cx, |source, _| {
        source
            .addon::<crate::EmbeddedResponse>()
            .unwrap()
            .workbench
            .clone()
            .unwrap()
    });
    let (view, excerpt, mut cancellation) = workbench.update_in(cx, |workbench, window, cx| {
        let mut task = task::SpawnInTerminal::default();
        task.env.insert("ZED_ROW".into(), "5".into());
        let view = workbench.prepare_run(&mut task, window, cx);
        let (sender, receiver) = futures::channel::oneshot::channel();
        view.update(cx, |view, _| view.cancel = Some(sender));
        (view.downgrade(), workbench.editor.downgrade(), receiver)
    });
    source.update(cx, |source, cx| {
        source.unregister_addon::<crate::EmbeddedResponse>();
        cx.notify();
    });
    drop(workbench);
    cx.run_until_parked();
    assert!(view.upgrade().is_none());
    assert!(excerpt.upgrade().is_none());
    assert!(cancellation.try_recv().is_err());
}

#[gpui::test]
async fn environments_are_discovered_without_executing_configuration(
    cx: &mut gpui::TestAppContext,
) {
    crate::tests::init(cx);
    let fs = fs::FakeFs::new(cx.executor());
    let root = if cfg!(windows) {
        "C:\\project"
    } else {
        "/project"
    };
    fs.insert_tree(
        root,
        serde_json::json!({
            "http-client.env.json": "{\"dev\":{\"host\":\"localhost\"}}",
            "http-client.private.env.json": "{\"prod\":{\"token\":\"private\"}}",
            "api": {"requests.http": DOCUMENT},
            ".httpyac.cjs": "throw new Error('must not execute during discovery')"
        }),
    )
    .await;
    let project = project::Project::test(fs, [std::path::Path::new(root)], cx).await;
    let worktree_id = project.read_with(cx, |project, cx| {
        project.worktrees(cx).next().unwrap().read(cx).id()
    });
    let path = project::ProjectPath {
        worktree_id,
        path: util::rel_path::RelPath::from_unix_str("api/requests.http")
            .unwrap()
            .into(),
    };
    let buffer = project
        .update(cx, |project, cx| project.open_buffer(path, cx))
        .await
        .unwrap();
    buffer.update(cx, |buffer, cx| buffer.set_language(Some(language()), cx));
    let (source, cx) = cx.add_window_view(|window, cx| {
        editor::Editor::for_buffer(buffer, Some(project), window, cx)
    });
    cx.run_until_parked();
    let workbench = source.read_with(cx, |source, _| {
        source
            .addon::<crate::EmbeddedResponse>()
            .unwrap()
            .workbench
            .clone()
            .unwrap()
    });
    workbench.read_with(cx, |workbench, cx| {
        assert_eq!(workbench.environment_names, ["dev", "prod"]);
        assert_eq!(workbench.environment_files.len(), 2);
        assert!(workbench.message.is_empty(), "{}", workbench.message);
        assert!(workbench.preflight(cx).is_ok());
    });
    let environment_buffer =
        workbench.read_with(cx, |workbench, _| workbench.environment_buffers[0].clone());
    environment_buffer.update(cx, |buffer, cx| buffer.edit([(0..0, " ")], None, cx));
    cx.run_until_parked();
    assert!(workbench.read_with(cx, |workbench, cx| workbench.preflight(cx).is_err()));
}

#[gpui::test]
async fn send_all_results_are_addressed_by_source_not_name(cx: &mut gpui::TestAppContext) {
    crate::tests::init(cx);
    let buffer = cx.new(|cx| language::Buffer::local(DOCUMENT, cx).with_language(language(), cx));
    let (source, cx) =
        cx.add_window_view(|window, cx| editor::Editor::for_buffer(buffer, None, window, cx));
    cx.run_until_parked();
    let workbench = source.read_with(cx, |source, _| {
        source
            .addon::<crate::EmbeddedResponse>()
            .unwrap()
            .workbench
            .clone()
            .unwrap()
    });
    workbench.update_in(cx, |workbench, window, cx| {
        workbench.request_mode = true;
        let mut task = task::SpawnInTerminal::default();
        task.args.push("--all".into());
        let view = workbench.prepare_run(&mut task, window, cx);
        for (row, main) in [(5, true), (11, false), (11, true)] {
            let crate::Event::Response(mut response) = crate::tests::response() else {
                unreachable!()
            };
            response.source_line = Some(row);
            response.source_document = main;
            view.update(cx, |view, cx| {
                view.responses.push_back(response);
                cx.notify();
            });
        }
        workbench.finished(cx);
        workbench.select(Some(workbench.requests[0].id), window, cx);
        assert_eq!(workbench.response, view);
        assert_eq!(view.read(cx).selected, 0);
        workbench.select(Some(workbench.requests[1].id), window, cx);
        assert_eq!(workbench.response, view);
        assert_eq!(view.read(cx).selected, 2);
    });
}

#[gpui::test]
async fn request_search_is_local_and_hiding_response_preserves_layout(
    cx: &mut gpui::TestAppContext,
) {
    crate::tests::init(cx);
    let buffer = cx.new(|cx| language::Buffer::local(DOCUMENT, cx).with_language(language(), cx));
    let (source, cx) =
        cx.add_window_view(|window, cx| editor::Editor::for_buffer(buffer, None, window, cx));
    cx.run_until_parked();
    let workbench = source.read_with(cx, |source, _| {
        source
            .addon::<crate::EmbeddedResponse>()
            .unwrap()
            .workbench
            .clone()
            .unwrap()
    });
    workbench.update_in(cx, |workbench, window, cx| {
        workbench.toggle_mode(window, cx);
        workbench.select(Some(workbench.requests[0].id), window, cx);
        workbench.sidebar_ratio = 0.3;
        workbench.editor_ratio = 0.6;
    });
    cx.run_until_parked();
    cx.dispatch_action(search::buffer_search::Deploy::find());
    cx.run_until_parked();
    let search = workbench.read_with(cx, |workbench, _| workbench.search.clone());
    assert!(!search.read_with(cx, |search, _| search.is_dismissed()));
    search
        .update_in(cx, |search, window, cx| {
            search.search("POST", None, true, window, cx)
        })
        .await
        .unwrap();
    assert!(!search.update_in(cx, |search, window, cx| search.match_exists(window, cx)));
    cx.update(|window, cx| crate::toggle_response(source.clone(), window, cx));
    workbench.read_with(cx, |workbench, _| {
        assert!(!workbench.response_visible);
        assert_eq!(workbench.sidebar_ratio, 0.3);
        assert_eq!(workbench.editor_ratio, 0.6);
    });
}

#[gpui::test]
async fn queued_environment_is_frozen_and_history_is_bounded(cx: &mut gpui::TestAppContext) {
    crate::tests::init(cx);
    let buffer = cx.new(|cx| language::Buffer::local(DOCUMENT, cx).with_language(language(), cx));
    let (source, cx) =
        cx.add_window_view(|window, cx| editor::Editor::for_buffer(buffer, None, window, cx));
    cx.run_until_parked();
    let workbench = source.read_with(cx, |source, _| {
        source
            .addon::<crate::EmbeddedResponse>()
            .unwrap()
            .workbench
            .clone()
            .unwrap()
    });
    let evicted = workbench.update_in(cx, |workbench, window, cx| {
        workbench.select_environment(super::Environment::Named("prod".into()), cx);
        let mut task = task::SpawnInTerminal::default();
        task.env.insert("ZED_ROW".into(), "5".into());
        task.env.insert("ZED_HTTPYAC_ENV".into(), "default".into());
        task.env
            .insert("ZED_CUSTOM_HTTPYAC_ENV_OVERRIDE".into(), "\"dev\"".into());
        let view = workbench.prepare_run(&mut task, window, cx);
        assert_eq!(task.env.get("ZED_HTTPYAC_ENV").unwrap(), "dev");
        assert_eq!(view.read(cx).environment, "[\"dev\"]");
        assert_ne!(
            workbench.response, view,
            "a queued dev run must not appear under prod"
        );
        workbench.finished(cx);
        let weak = view.downgrade();
        drop(view);
        for index in 0..super::MAX_RUNS + 2 {
            task.env.insert(
                "ZED_CUSTOM_HTTPYAC_ENV_OVERRIDE".into(),
                serde_json::json!(format!("env{index}")).to_string(),
            );
            workbench.prepare_run(&mut task, window, cx);
            workbench.finished(cx);
        }
        assert!(workbench.runs.len() <= super::MAX_RUNS);
        weak
    });
    cx.run_until_parked();
    assert!(evicted.upgrade().is_none());
}

#[test]
fn environment_identity_preserves_overlay_order() {
    assert_eq!(
        super::environment_key("dev, local"),
        super::environment_key(r#"["dev","local"]"#)
    );
    assert_ne!(
        super::environment_key("dev,local"),
        super::environment_key("local,dev")
    );
    assert_eq!(super::environment_key(""), "[]");
}
