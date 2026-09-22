use gpui::{AppContext as _, Focusable as _};

fn response() -> super::Event {
    serde_json::from_value(serde_json::json!({
        "type": "response", "name": "GetUser", "status": 200, "statusMessage": "OK",
        "headers": "Content-Type: application/json\nSet-Cookie: a=1\nSet-Cookie: b=2",
        "request": "GET https://example.test/user", "body": "{\"HTTP_RESULT_UNIQUE\":42}",
        "rawBody": "e30=", "bodyBytes": 2, "truncated": false, "textTruncated": false,
        "contentType": "application/json", "timings": {"total": 12}
    }))
    .unwrap()
}

#[test]
fn pretty_json_preserves_numbers_keys_and_string_escapes() {
    let raw =
        r#"{"id":900719925474099312345678,"id":1e999,"text":"a\\\"{}[]", "empty": [ {}, [] ]}"#;
    let formatted = super::pretty_json(raw).unwrap();
    assert!(formatted.contains("900719925474099312345678"));
    assert!(formatted.contains("1e999"));
    assert_eq!(formatted.matches("\"id\"").count(), 2);
    assert!(formatted.contains(r#""a\\\"{}[]""#));
    assert!(serde_json::from_str::<Box<serde_json::value::RawValue>>(&formatted).is_ok());
    assert_eq!(super::pretty_json("[1,2]"), Some("[\n  1,\n  2\n]".into()));
    assert!(super::pretty_json("not JSON").is_none());
}

fn init(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        cx.set_global(db::AppDatabase::test_new());
        workspace::AppState::test(cx);
        editor::init(cx);
        super::init(cx);
    });
}

#[gpui::test]
async fn response_is_editor_local_and_searches_only_response(cx: &mut gpui::TestAppContext) {
    init(cx);
    let (source, cx) = cx.add_window_view(|window, cx| editor::Editor::multi_line(window, cx));
    source.update_in(cx, |source, window, cx| {
        source.set_text("GET https://example.test/user\n", window, cx);
        source.focus_handle(cx).focus(window, cx);
    });
    cx.run_until_parked();
    let full_width = source.read_with(cx, |source, _| source.last_bounds().unwrap().size.width);
    let response = source.update_in(cx, super::ensure_response);
    cx.run_until_parked();
    assert!(
        source.read_with(cx, |source, _| source.last_bounds().unwrap().size.width)
            < full_width * 0.6
    );
    response.update_in(cx, |view, window, cx| {
        view.event(self::response(), window, cx)
    });
    cx.run_until_parked();
    source.read_with(cx, |source, cx| {
        assert_eq!(source.text(cx), "GET https://example.test/user\n");
    });
    cx.update(|window, cx| assert!(source.focus_handle(cx).is_focused(window)));
    let other = source.update_in(cx, |source, window, cx| {
        cx.new(|cx| source.clone(window, cx))
    });
    assert!(other.read_with(cx, |other, _| {
        other.addon::<super::EmbeddedResponse>().is_none()
    }));
    let search = response.read_with(cx, |view, _| view.search.clone());
    let body = response.read_with(cx, |view, _| view.body.clone());
    cx.update(|window, cx| body.focus_handle(cx).focus(window, cx));
    cx.dispatch_action(search::buffer_search::Deploy::find());
    cx.run_until_parked();
    assert!(!search.read_with(cx, |search, _| search.is_dismissed()));
    cx.update(|window, cx| {
        assert!(
            search.focus_handle(cx).is_focused(window),
            "response search input must have keyboard focus"
        )
    });
    cx.dispatch_action(search::buffer_search::Deploy::replace());
    cx.run_until_parked();
    cx.update(|window, cx| {
        assert!(
            search.focus_handle(cx).is_focused(window),
            "read-only search must not focus a hidden replace input"
        )
    });
    search
        .update_in(cx, |search, window, cx| {
            search.search("HTTP_RESULT_UNIQUE", None, true, window, cx)
        })
        .await
        .unwrap();
    assert!(search.update_in(cx, |search, window, cx| search.match_exists(window, cx)));
    search
        .update_in(cx, |search, window, cx| {
            search.search("example.test", None, true, window, cx)
        })
        .await
        .unwrap();
    assert!(!search.update_in(cx, |search, window, cx| search.match_exists(window, cx)));
}

#[gpui::test]
async fn hiding_keeps_results_and_unregistering_releases_view(cx: &mut gpui::TestAppContext) {
    init(cx);
    let (source, cx) = cx.add_window_view(|window, cx| editor::Editor::multi_line(window, cx));
    let view = source.update_in(cx, super::ensure_response);
    view.update_in(cx, |view, window, cx| view.event(response(), window, cx));
    source.update(cx, |source, cx| {
        let addon = source.addon_mut::<super::EmbeddedResponse>().unwrap();
        addon.ratio = 0.7;
        addon.visible = false;
        cx.notify();
    });
    cx.run_until_parked();
    let reopened = source.update_in(cx, super::ensure_response);
    assert_eq!(reopened, view);
    assert_eq!(view.read_with(cx, |view, _| view.responses.len()), 1);
    assert_eq!(
        source.read_with(cx, |source, _| source
            .addon::<super::EmbeddedResponse>()
            .unwrap()
            .ratio),
        0.7
    );
    view.update_in(cx, |view, window, cx| {
        view.section = super::Section::Headers;
        view.refresh(window, cx);
        assert!(view.text().contains("Set-Cookie: b=2"));
        assert!(
            view.controls.is_none(),
            "view changes must never start execution"
        );
    });
    let weak = view.downgrade();
    let (cancel, mut canceled) = futures::channel::oneshot::channel();
    view.update(cx, |view, _| view.cancel = Some(cancel));
    source.update(cx, |source, cx| {
        source.unregister_addon::<super::EmbeddedResponse>();
        cx.notify();
    });
    drop(reopened);
    drop(view);
    cx.run_until_parked();
    assert!(weak.upgrade().is_none());
    assert!(
        canceled.try_recv().is_err(),
        "releasing the response must cancel pending IO"
    );
}

#[gpui::test]
async fn results_and_logs_are_bounded_and_prompts_are_cleared(cx: &mut gpui::TestAppContext) {
    init(cx);
    let (source, cx) = cx.add_window_view(|window, cx| editor::Editor::multi_line(window, cx));
    let view = source.update_in(cx, super::ensure_response);
    view.update_in(cx, |view, window, cx| {
        for _ in 0..super::MAX_RESULTS + 3 {
            view.event(response(), window, cx);
        }
        assert_eq!(view.responses.len(), super::MAX_RESULTS);
        view.event(
            super::Event::Output {
                stream: "stderr".into(),
                text: "中文".repeat(super::MAX_LOG),
            },
            window,
            cx,
        );
        assert!(view.logs.len() <= super::MAX_LOG);
        view.event(
            serde_json::from_value(serde_json::json!({
                "type":"prompt", "id":"input", "kind":"password", "message":"Password"
            }))
            .unwrap(),
            window,
            cx,
        );
        view.prompt_input
            .update(cx, |input, cx| input.set_text("secret", window, cx));
        view.finish("Cancelled".into(), window, cx);
        assert!(view.prompt.is_none());
        assert!(view.prompt_input.read(cx).text(cx).is_empty());
    });
}

#[gpui::test]
async fn native_task_stays_with_invoking_tab_and_reports_preflight_failure(
    cx: &mut gpui::TestAppContext,
) {
    init(cx);
    let fs = fs::FakeFs::new(cx.executor());
    let root = if cfg!(target_os = "windows") {
        "C:\\project"
    } else {
        "/project"
    };
    fs.insert_tree(root, serde_json::json!({})).await;
    let project = project::Project::test(fs, [std::path::Path::new(root)], cx).await;
    let (workspace, cx) =
        cx.add_window_view(|window, cx| workspace::Workspace::test_new(project, window, cx));
    let (source, other) = workspace.update_in(cx, |workspace, window, cx| {
        let source = cx.new(|cx| editor::Editor::multi_line(window, cx));
        let other = cx.new(|cx| editor::Editor::multi_line(window, cx));
        workspace.add_item_to_active_pane(Box::new(source.clone()), None, true, window, cx);
        workspace.add_item_to_active_pane(Box::new(other.clone()), None, true, window, cx);
        (source, other)
    });
    let mut template = task::TaskTemplate {
        label: "HTTP test".into(),
        command: "node".into(),
        args: vec![
            "-e".into(),
            "require(process.env.ZED_CUSTOM_HTTPYAC_EXECUTOR)".into(),
            "--".into(),
            "send".into(),
        ],
        ..Default::default()
    };
    template.env.insert("ZED_HTTPYAC_UI".into(), "1".into());
    template.env.insert(
        "ZED_CUSTOM_SOURCE_EDITOR".into(),
        source.entity_id().as_u64().to_string(),
    );
    let resolved = template
        .resolve_task("http-test", &task::TaskContext::default())
        .unwrap();
    let (sender, receiver) = futures::channel::oneshot::channel();
    source.update_in(cx, |_, window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.schedule_resolved_task_with_completion(
                project::TaskSourceKind::UserInput,
                resolved,
                true,
                move |result, _| {
                    sender.send(result).unwrap();
                },
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();
    assert_eq!(
        receiver.await.unwrap(),
        workspace::tasks::ScheduledTaskResult::Failure
    );
    source.read_with(cx, |source, cx| {
        let view = &source.addon::<super::EmbeddedResponse>().unwrap().view;
        let view = view.read(cx);
        assert!(view.status.contains("Missing HTTP executor"));
        assert!(view.logs.contains("Missing HTTP executor"));
        assert!(view.controls.is_none());
    });
    assert!(other.read_with(cx, |other, _| {
        other.addon::<super::EmbeddedResponse>().is_none()
    }));
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.panes().len(), 1);
        assert_eq!(workspace.active_pane().read(cx).items_len(), 2);
        assert_eq!(
            workspace.active_item_as::<editor::Editor>(cx),
            Some(other.clone())
        );
    });
}

#[gpui::test]
async fn streaming_defaults_to_log_once_without_overriding_explicit_sections(
    cx: &mut gpui::TestAppContext,
) {
    init(cx);
    let (source, cx) = cx.add_window_view(|window, cx| editor::Editor::multi_line(window, cx));
    let view = source.update_in(cx, super::ensure_response);
    view.update_in(cx, |view, window, cx| {
        for streaming in [false, true] {
            view.section = super::Section::Body;
            view.select_initial_section = true;
            view.event(
                serde_json::from_value(
                    serde_json::json!({"type": "started", "streaming": streaming}),
                )
                .unwrap(),
                window,
                cx,
            );
            assert!(
                view.section
                    == if streaming {
                        super::Section::Log
                    } else {
                        super::Section::Body
                    }
            );
            assert!(view.pretty);
            assert!(!view.select_initial_section);
        }
        view.event(
            super::Event::Output {
                stream: "stdout".into(),
                text: "message: first push\n".into(),
            },
            window,
            cx,
        );
        assert!(view.body.read(cx).text(cx).contains("first push"));
        // A user switching to Raw after startup must not be switched back by later events.
        view.section = super::Section::Body;
        view.pretty = false;
        view.refresh(window, cx);
        view.event(
            super::Event::Output {
                stream: "stdout".into(),
                text: "message: another push\n".into(),
            },
            window,
            cx,
        );
        view.event(response(), window, cx);
        view.event(super::Event::Started { streaming: true }, window, cx);
        assert!(view.section == super::Section::Body);
        assert!(!view.pretty);
        for section in [super::Section::Headers, super::Section::Request] {
            view.section = section;
            view.select_initial_section = false;
            view.event(super::Event::Started { streaming: true }, window, cx);
            assert!(view.section == section);
        }
        // A choice made while preparing/queued also cancels automatic selection.
        view.section = super::Section::Tests;
        view.select_initial_section = false;
        view.event(super::Event::Started { streaming: true }, window, cx);
        assert!(view.section == super::Section::Tests);
        view.select_initial_section = true;
        view.finish("Cancelled".into(), window, cx);
        view.event(super::Event::Started { streaming: true }, window, cx);
        assert!(view.section == super::Section::Tests);
    });
}

#[gpui::test]
async fn response_navigation_keeps_raw_pretty_and_run_status_separate(
    cx: &mut gpui::TestAppContext,
) {
    init(cx);
    let (source, cx) = cx.add_window_view(|window, cx| editor::Editor::multi_line(window, cx));
    let view = source.update_in(cx, super::ensure_response);
    view.update_in(cx, |view, window, cx| {
        view.event(response(), window, cx);
        view.finish("Finished".into(), window, cx);
        assert!(view.pretty);
        assert!(view.text().contains('\n'));
        assert_eq!(view.status_summary(), "Finished · 200 OK · 2 bytes · 12 ms");
        view.pretty = false;
        view.refresh(window, cx);
        assert_eq!(view.text(), "{\"HTTP_RESULT_UNIQUE\":42}");
        assert_eq!(view.body.read(cx).text(cx), view.text());
        view.event(response(), window, cx);
        view.selected = 0;
        view.pretty = true;
        view.refresh(window, cx);
        assert!(view.text().contains('\n'));
        assert_eq!(view.responses.len(), 2);
        assert!(
            view.controls.is_none(),
            "navigation must not start a request"
        );
        view.finish("Failed — see Tests / Log".into(), window, cx);
        assert_eq!(view.status_summary(), "Failed · 200 OK · 2 bytes · 12 ms");
        view.finish("HTTP execution failed: detailed failure".into(), window, cx);
        assert!(!view.status_summary().contains("detailed failure"));
        assert!(view.logs.contains("detailed failure"));
    });
}

#[gpui::test]
async fn response_context_menu_preserves_selection_and_targets_response(
    cx: &mut gpui::TestAppContext,
) {
    init(cx);
    let (source, cx) = cx.add_window_view(|window, cx| editor::Editor::multi_line(window, cx));
    source.update_in(cx, |source, window, cx| {
        source.set_text("source stays unchanged", window, cx)
    });
    let view = source.update_in(cx, super::ensure_response);
    view.update_in(cx, |view, window, cx| view.event(response(), window, cx));
    let body = view.read_with(cx, |view, _| view.body.clone());
    body.update_in(cx, |body, window, cx| {
        let text = body.text(cx);
        let start = text.find("HTTP_RESULT_UNIQUE").unwrap();
        body.change_selections(
            editor::SelectionEffects::no_scroll(),
            window,
            cx,
            |selections| {
                selections.select_ranges([editor::MultiBufferOffset(start)
                    ..editor::MultiBufferOffset(start + "HTTP_RESULT_UNIQUE".len())]);
            },
        );
    });
    cx.run_until_parked();
    let point = body.read_with(cx, |body, _| body.last_bounds().unwrap().center());
    cx.simulate_mouse_down(point, gpui::MouseButton::Right, gpui::Modifiers::none());
    cx.simulate_mouse_up(point, gpui::MouseButton::Right, gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("MENU_ITEM-Cut").is_none());
    assert!(cx.debug_bounds("MENU_ITEM-Paste").is_none());
    let copy = cx.debug_bounds("MENU_ITEM-Copy").unwrap();
    cx.simulate_click(copy.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap()),
        "HTTP_RESULT_UNIQUE"
    );
    cx.update(|window, cx| body.focus_handle(cx).focus(window, cx));
    cx.dispatch_action(editor::actions::OpenContextMenu);
    cx.run_until_parked();
    let find = cx.debug_bounds("MENU_ITEM-Find Selection").unwrap();
    cx.simulate_click(find.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    view.read_with(cx, |view, cx| {
        assert_eq!(view.search.read(cx).query(cx), "HTTP_RESULT_UNIQUE");
        assert!(!view.search.read(cx).is_dismissed());
    });
    cx.update(|window, cx| body.focus_handle(cx).focus(window, cx));
    cx.dispatch_action(editor::actions::OpenContextMenu);
    cx.run_until_parked();
    let copy_all = cx.debug_bounds("MENU_ITEM-Copy All").unwrap();
    cx.simulate_click(copy_all.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap()),
        body.read_with(cx, |body, cx| body.text(cx))
    );
    cx.update(|window, cx| body.focus_handle(cx).focus(window, cx));
    cx.dispatch_action(editor::actions::OpenContextMenu);
    cx.run_until_parked();
    let select_all = cx.debug_bounds("MENU_ITEM-Select All").unwrap();
    cx.simulate_click(select_all.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    cx.dispatch_action(editor::actions::Copy);
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap()),
        body.read_with(cx, |body, cx| body.text(cx))
    );
    body.update_in(cx, |body, window, cx| {
        body.change_selections(
            editor::SelectionEffects::no_scroll(),
            window,
            cx,
            |selections| {
                selections
                    .select_ranges([editor::MultiBufferOffset(0)..editor::MultiBufferOffset(0)]);
            },
        );
    });
    cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("unchanged".into())));
    cx.dispatch_action(editor::actions::OpenContextMenu);
    cx.run_until_parked();
    let copy = cx.debug_bounds("MENU_ITEM-Copy").unwrap();
    cx.simulate_click(copy.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap()),
        "unchanged",
        "Copy without a selection must be disabled, not copy the current line",
    );
    assert_eq!(
        source.read_with(cx, |source, cx| source.text(cx)),
        "source stays unchanged"
    );
}

fn stream_event(section: super::Section, text: String) -> super::Event {
    if section == super::Section::Tests {
        super::Event::Test {
            status: "SUCCESS".into(),
            message: text,
        }
    } else {
        super::Event::Output {
            stream: "stdout".into(),
            text,
        }
    }
}

#[gpui::test]
async fn streams_follow_only_at_bottom_and_preserve_reading_and_selection(
    cx: &mut gpui::TestAppContext,
) {
    init(cx);
    for section in [super::Section::Log, super::Section::Tests] {
        let (source, cx) = cx.add_window_view(|window, cx| editor::Editor::multi_line(window, cx));
        let view = source.update_in(cx, super::ensure_response);
        view.update_in(cx, |view, window, cx| {
            view.section = section;
            view.refresh(window, cx);
        });
        cx.run_until_parked();
        let body = view.read_with(cx, |view, _| view.body.clone());
        view.update_in(cx, |view, window, cx| {
            view.event(
                stream_event(
                    section,
                    (0..200)
                        .map(|index| format!("row-{index:04}-中文\r\n"))
                        .collect(),
                ),
                window,
                cx,
            );
        });
        cx.run_until_parked();
        for index in 0..5 {
            view.update_in(cx, |view, window, cx| {
                for part in ["first", "second"] {
                    view.event(
                        stream_event(section, format!("latest-{index}-{part}\n")),
                        window,
                        cx,
                    );
                }
            });
            cx.run_until_parked();
            body.update_in(cx, |body, _, cx| {
                let position = body.scroll_position(cx);
                let last_content_row = f64::from(body.max_point(cx).row().0) - 1.;
                let visible = body.visible_line_count().unwrap().floor();
                assert!((position.y - (last_content_row + 1. - visible)).abs() < 0.01);
                assert!(
                    position.y < last_content_row,
                    "the trailing empty line must not become the top row"
                );
            });
        }
        body.update_in(cx, |body, window, cx| {
            let start = body.text(cx).find("row-0050").unwrap();
            body.change_selections(
                editor::SelectionEffects::no_scroll(),
                window,
                cx,
                |selections| {
                    selections.select_ranges([
                        editor::MultiBufferOffset(start)..editor::MultiBufferOffset(start + 8)
                    ]);
                },
            );
            body.set_scroll_position(gpui::point(0., 40.25), window, cx);
        });
        cx.run_until_parked();
        let (position, selection) = body.update_in(cx, |body, _, cx| {
            let display = body.display_snapshot(cx);
            (
                body.scroll_position(cx),
                body.selections
                    .newest::<editor::MultiBufferOffset>(&display)
                    .range(),
            )
        });
        for index in 0..10 {
            view.update_in(cx, |view, window, cx| {
                view.event(
                    stream_event(section, format!("more-{index}\r\n")),
                    window,
                    cx,
                );
                // A completed response must not rewrite the unchanged stream either.
                view.event(response(), window, cx);
            });
            cx.run_until_parked();
            body.update_in(cx, |body, _, cx| {
                let display = body.display_snapshot(cx);
                assert_eq!(body.scroll_position(cx), position);
                assert_eq!(
                    body.selections
                        .newest::<editor::MultiBufferOffset>(&display)
                        .range(),
                    selection
                );
            });
        }
        view.read_with(cx, |view, cx| {
            assert_eq!(view.text(), body.read(cx).text(cx))
        });
        // Reading without a selection must also remain stable.
        body.update_in(cx, |body, window, cx| {
            body.change_selections(
                editor::SelectionEffects::no_scroll(),
                window,
                cx,
                |selections| {
                    selections.select_ranges([
                        editor::MultiBufferOffset(0)..editor::MultiBufferOffset(0)
                    ]);
                },
            );
        });
        view.update_in(cx, |view, window, cx| {
            view.event(stream_event(section, "still reading\n".into()), window, cx)
        });
        cx.run_until_parked();
        body.update_in(cx, |body, window, cx| {
            assert_eq!(body.scroll_position(cx), position);
            let bottom =
                f64::from(body.max_point(cx).row().0) - body.visible_line_count().unwrap().floor();
            body.set_scroll_position(gpui::point(0., bottom), window, cx);
        });
        view.update_in(cx, |view, window, cx| {
            view.event(stream_event(section, "follow again\n".into()), window, cx)
        });
        cx.run_until_parked();
        body.update_in(cx, |body, _, cx| {
            let bottom =
                f64::from(body.max_point(cx).row().0) - body.visible_line_count().unwrap().floor();
            assert!((body.scroll_position(cx).y - bottom).abs() < 0.01);
        });
        let position = body.update_in(cx, |body, window, cx| {
            let start = body.text(cx).find("follow again").unwrap();
            body.change_selections(
                editor::SelectionEffects::no_scroll(),
                window,
                cx,
                |selections| {
                    selections.select_ranges([
                        editor::MultiBufferOffset(start)..editor::MultiBufferOffset(start + 12)
                    ]);
                },
            );
            body.scroll_position(cx)
        });
        view.update_in(cx, |view, window, cx| {
            view.event(
                stream_event(section, "pause while selecting\n".into()),
                window,
                cx,
            );
        });
        cx.run_until_parked();
        body.update_in(cx, |body, _, cx| {
            assert_eq!(body.scroll_position(cx), position)
        });
    }
}

#[gpui::test]
async fn stream_trimming_preserves_retained_text_and_clamps_evicted_positions(
    cx: &mut gpui::TestAppContext,
) {
    init(cx);
    for section in [super::Section::Log, super::Section::Tests] {
        let (source, cx) = cx.add_window_view(|window, cx| editor::Editor::multi_line(window, cx));
        let view = source.update_in(cx, super::ensure_response);
        view.update_in(cx, |view, window, cx| {
            view.section = section;
            view.refresh(window, cx);
        });
        cx.run_until_parked();
        view.update_in(cx, |view, window, cx| {
            view.event(
                stream_event(
                    section,
                    (0..6000)
                        .map(|index| format!("{index:05}-{}\r\n", "中文".repeat(8)))
                        .collect(),
                ),
                window,
                cx,
            );
        });
        cx.run_until_parked();
        let body = view.read_with(cx, |view, _| view.body.clone());
        body.update_in(cx, |body, window, cx| {
            let start = body.text(cx).find("05000-").unwrap();
            body.change_selections(
                editor::SelectionEffects::no_scroll(),
                window,
                cx,
                |selections| {
                    selections.select_ranges([
                        editor::MultiBufferOffset(start)..editor::MultiBufferOffset(start + 12)
                    ]);
                },
            );
            body.set_scroll_position(gpui::point(0., 1000.25), window, cx);
        });
        cx.run_until_parked();
        let top_line = body.update_in(cx, |body, _, cx| {
            let row = body.scroll_position(cx).y.floor() as usize;
            body.display_text(cx).lines().nth(row).unwrap().to_owned()
        });
        view.update_in(cx, |view, window, cx| {
            view.event(response(), window, cx);
            view.responses.back_mut().unwrap().raw_body = "!".into();
            view.save(window, cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |view, cx| {
            assert_eq!(view.text(), body.read(cx).text(cx))
        });
        for _ in 0..3 {
            view.update_in(cx, |view, window, cx| {
                view.event(stream_event(section, "新消息\r\n".repeat(100)), window, cx);
            });
            cx.run_until_parked();
            body.update_in(cx, |body, _, cx| {
                let position = body.scroll_position(cx);
                assert_eq!(
                    body.display_text(cx)
                        .lines()
                        .nth(position.y.floor() as usize)
                        .unwrap(),
                    top_line
                );
                assert!((position.y.fract() - 0.25).abs() < 0.01);
                let display = body.display_snapshot(cx);
                let selection = body
                    .selections
                    .newest::<editor::MultiBufferOffset>(&display);
                assert_eq!(
                    &body.text(cx)[selection.start.0..selection.end.0],
                    "05000-中文"
                );
            });
            view.read_with(cx, |view, cx| {
                assert!(view.text().len() <= super::MAX_LOG);
                assert_eq!(view.text(), body.read(cx).text(cx));
            });
        }
        // One large event can evict the entire old view, including its selection.
        view.update_in(cx, |view, window, cx| {
            view.event(
                stream_event(section, "替换\n".repeat(super::MAX_LOG)),
                window,
                cx,
            );
        });
        cx.run_until_parked();
        body.update_in(cx, |body, _, cx| {
            assert_eq!(body.scroll_position(cx).y, 0.);
            let display = body.display_snapshot(cx);
            let selection = body
                .selections
                .newest::<editor::MultiBufferOffset>(&display);
            assert_eq!(selection.start.0, 0);
            assert_eq!(selection.end.0, 0);
        });
        view.read_with(cx, |view, cx| {
            assert_eq!(view.text(), body.read(cx).text(cx))
        });
    }
}

#[gpui::test]
async fn frames_are_bounded_and_preserve_unicode() {
    let bytes = "{\"type\":\"output\",\"stream\":\"stdout\",\"text\":\"你好\"}\n{\"type\":\"done\",\"code\":0}\n".as_bytes();
    let mut reader = futures::io::BufReader::with_capacity(3, bytes);
    let frame = super::read_frame(&mut reader).await.unwrap().unwrap();
    assert!(matches!(
        serde_json::from_slice::<super::Event>(&frame).unwrap(),
        super::Event::Output { .. }
    ));
    assert!(super::read_frame(&mut reader).await.unwrap().is_some());
    assert!(super::read_frame(&mut reader).await.unwrap().is_none());
    let data = vec![b'x'; super::MAX_FRAME + 1];
    let mut reader = futures::io::BufReader::new(data.as_slice());
    assert!(super::read_frame(&mut reader).await.is_err());
}
