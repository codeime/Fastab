use super::*;
use fastab_remote_ipc::figterm::{EditBuffer, FigtermSession};

fn row(name: &str, kind: &str) -> SuggestionItem {
    SuggestionItem {
        name: name.into(),
        kind: kind.into(),
        ..Default::default()
    }
}

fn insertions(commands: &mut fastab_remote_ipc::outbox::Outbox) -> Vec<(Option<String>, Option<i64>, Option<bool>)> {
    let mut inserted = Vec::new();
    while let Some(frame) = commands.try_recv() {
        let (_, message) = fastab_proto::FigMessage::parse(&mut frame.bytes()).unwrap();
        let message = message.decode::<fastab_proto::remote::Clientbound>().unwrap();
        if let Some(fastab_proto::remote::clientbound::Packet::Request(request)) = message.packet {
            if let Some(fastab_proto::remote::clientbound::request::Request::InsertText(insert)) = request.request {
                inserted.push((
                    insert.insertion,
                    insert.deletion.map(|value| value as i64),
                    insert.immediate,
                ));
            }
        }
    }
    inserted
}

#[gpui::test]
fn show_without_a_caret_keeps_idle_retirement_and_resumes_once(cx: &mut gpui::TestAppContext) {
    // The real worker reads process-wide settings on its own thread. Isolate
    // this override so the regression never loads the user's shell history.
    const CHILD: &str = "FASTAB_SHOW_CARET_REGRESSION_CHILD";
    const TEST: &str = "overlay::tab_tests::show_without_a_caret_keeps_idle_retirement_and_resumes_once";
    if std::env::var(CHILD).as_deref() != Ok(TEST) {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, TEST)
            .output()
            .expect("isolated caret regression process");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("test result: ok. 1 passed;"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    use fastab_settings::JsonStore as _;
    *fastab_settings::OldSettings::data_lock().write() = Some(
        [("autocomplete.history.disableLoading".into(), serde_json::json!(true))]
            .into_iter()
            .collect(),
    );
    let _settings = fastab_settings::settings::install_override(fastab_settings::Settings::from_slice(&[
        ("beta.history.mode", serde_json::json!("off")),
        ("autocomplete.onlyShowOnTab", serde_json::json!(true)),
    ]));
    let specs = tempfile::tempdir().unwrap();
    std::fs::write(
        specs.path().join("fastab-show-fixture.json"),
        r#"{"names":["fastab-show-fixture"],"subcommands":[{"names":["choice-a"]},{"names":["choice-b"]}]}"#,
    )
    .unwrap();
    let engine = EngineClient::spawn(specs.path().to_path_buf()).unwrap();
    let (proxy, events) = crate::event_loop::channel();
    let figterm = Arc::new(FigtermState::new());
    let platform = Arc::new(PlatformState::new(proxy.clone()));
    let session = Uuid::new_v4();
    let buffer = "fastab-show-fixture ch";
    let mut controller = cx.update(|cx| {
        let mut controller =
            OverlayController::start(cx, engine.clone(), proxy, figterm.clone(), platform.clone()).unwrap();
        controller.enabled = true;
        controller.complete_buffer(
            buffer.into(),
            String::new(),
            buffer.len() as u32,
            session,
            figterm.clone(),
            cx,
        );
        controller.state.update(cx, |state, _| {
            state.set_suggestions(
                vec![row("choice-a", "subcommand"), row("choice-b", "subcommand")],
                "ch".into(),
            );
            state.current_arg_description = String::with_capacity(4096);
        });
        controller.hide_until_shown(cx);
        controller.handle_action("showAutocomplete", session, &figterm, cx);
        controller
    });
    cx.run_until_parked();
    for elapsed in [6, 3] {
        cx.executor().advance_clock(Duration::from_secs(elapsed));
        cx.run_until_parked();
        cx.update(|cx| {
            // A delayed Tab and repeated same-window caret misses must keep
            // both the original retirement deadline and the pending Show.
            controller.clear_caret_position(cx);
            controller.handle_action("showAutocompleteFromTab", session, &figterm, cx);
            let state = controller.state.read(cx);
            assert!(!state.visible && !state.suppress_until_shown);
            assert_eq!(state.items.len(), 2);
            assert!(state.current_arg_description.capacity() >= 4096);
            assert!(controller.handle.lock().unwrap().is_none());
            assert!(matches!(
                controller.completion_display,
                CompletionDisplayState::WaitingForCaret
            ));
        });
        cx.run_until_parked();
    }
    let waiting = futures::executor::block_on(engine.diagnostics()).unwrap();
    assert_eq!(waiting.requests.submitted, 0);
    assert!(waiting.engine.is_none(), "waiting for a caret must not load specs");
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    cx.update(|cx| {
        let state = controller.state.read(cx);
        assert_eq!(
            state.current_arg_description.capacity(),
            0,
            "idle retirement still ran at ten seconds"
        );
        assert_eq!(state.items.len(), 2, "Tab recovery keeps the rows");
        assert!(!state.visible);
        assert!(controller.handle.lock().unwrap().is_none());
        let position = WindowPosition::Absolute(Position::Logical(LogicalPosition::new(100.0, 100.0)));
        controller.apply_position(position, &platform, cx);
        controller.apply_position(position, &platform, cx);
        assert!(!controller.state.read(cx).suppress_until_shown);
    });
    // TestAppContext cannot open a native NSWindow. Verify recovery through
    // the actual worker and host event before the visible result is applied.
    assert_eq!(
        futures::executor::block_on(engine.diagnostics())
            .unwrap()
            .requests
            .completed,
        1
    );
    cx.run_until_parked();
    let result = events
        .try_iter()
        .find_map(|event| match event {
            Event::GpuiOverlayComplete { result, session_id, .. } if session_id == session => Some(result.unwrap()),
            _ => None,
        })
        .expect("a valid caret must resume the requested completion");
    assert!(result.suggestions.iter().any(|item| item.name == "choice-a"));
    assert!(result.suggestions.iter().any(|item| item.name == "choice-b"));
}

#[gpui::test]
fn default_tab_preserves_action_guards_and_normal_completion(cx: &mut gpui::TestAppContext) {
    let _settings = fastab_settings::settings::install_override(fastab_settings::Settings::new_fake());
    let specs = tempfile::tempdir().unwrap();
    let engine = EngineClient::spawn(specs.path().to_path_buf()).unwrap();
    let (proxy, _events) = crate::event_loop::channel();
    let figterm = Arc::new(FigtermState::new());
    let platform = Arc::new(PlatformState::new(proxy.clone()));
    let session = Uuid::new_v4();
    let (sender, mut commands) = fastab_remote_ipc::outbox::channel();
    // Capture the real controller's output without connecting to a user's PTY.
    figterm.insert(FigtermSession {
        id: session,
        secret: String::new(),
        sender,
        dead_since: None,
        edit_buffer: EditBuffer::default(),
        last_receive: tokio::time::Instant::now(),
        context: None,
        flattened_env: Arc::new(Vec::new()),
        terminal_cursor_coordinates: None,
        current_session_metrics: None,
        intercept: InterceptMode::Unlocked,
        intercept_global: InterceptMode::Unlocked,
    });
    let mut controller = cx.update(|cx| {
        let controller = OverlayController::start(cx, engine, proxy, figterm.clone(), platform).unwrap();
        controller.set_session(session);
        // No last_input: these acceptance checks must not write recency data.
        controller
    });
    let tab = DEFAULT_OVERLAY_BINDINGS
        .iter()
        .find(|(_, bindings)| bindings.contains(&"tab"))
        .unwrap()
        .0;
    let enter = DEFAULT_OVERLAY_BINDINGS
        .iter()
        .find(|(_, bindings)| bindings.contains(&"enter"))
        .unwrap()
        .0;
    let show = |controller: &mut OverlayController, rows, search: &str, selected, cx: &mut App| {
        controller.state.update(cx, |state, _| {
            state.suppress_until_shown = false;
            state.set_suggestions(rows, search.into());
            state.selected = selected;
        });
    };
    let newline = vec![(Some("\n".into()), Some(0), Some(false))];
    for kind in ["auto-execute", "special"] {
        let action = SuggestionItem {
            insert_value: Some("\n".into()),
            ..row("status", kind)
        };
        // The exact-command result contains both the execution row and the
        // ordinary row. Also cover an action selected away from index zero.
        for selected in [0, 1] {
            let mut rows = vec![action.clone(), row("status", "subcommand")];
            rows.swap(0, selected);
            cx.update(|cx| {
                show(&mut controller, rows, "status", selected, cx);
                controller.handle_action(tab, session, &figterm, cx);
                assert!(
                    insertions(&mut commands).is_empty(),
                    "Tab accepted {kind} at {selected}"
                );
                let state = controller.state.read(cx);
                assert!(state.visible);
                assert_eq!(state.items.len(), 2);
                assert_eq!(state.selected, selected);
                controller.handle_action(enter, session, &figterm, cx);
                assert_eq!(insertions(&mut commands), newline, "Enter must still accept {kind}");
            });
        }
        cx.update(|cx| {
            // Preserve the existing sole-action Full acceptance contract.
            show(&mut controller, vec![action], "status", 0, cx);
            controller.handle_action(tab, session, &figterm, cx);
            assert_eq!(insertions(&mut commands), newline, "sole {kind} must be accepted once");
        });
    }
    for (search, expected) in [("ch", "e"), ("che", "ckout")] {
        cx.update(|cx| {
            // An unrelated action row must not disable an ordinary selection.
            show(
                &mut controller,
                vec![
                    row("run", "auto-execute"),
                    row("checkout", "subcommand"),
                    row("cherry", "subcommand"),
                ],
                search,
                1,
                cx,
            );
            controller.handle_action(tab, session, &figterm, cx);
            assert_eq!(
                insertions(&mut commands),
                vec![(Some(expected.into()), Some(0), Some(false))]
            );
        });
    }
    cx.update(|cx| {
        show(&mut controller, Vec::new(), "", 0, cx);
        controller.handle_action(tab, session, &figterm, cx);
        assert!(insertions(&mut commands).is_empty());
        show(&mut controller, vec![row("status", "subcommand")], "status", 0, cx);
        controller.handle_action("execute", session, &figterm, cx);
        assert_eq!(insertions(&mut commands), newline, "explicit execution stays available");
    });
}

#[gpui::test]
fn rejected_insert_clears_prediction_and_does_not_advance_intercept_state(cx: &mut gpui::TestAppContext) {
    let _settings = fastab_settings::settings::install_override(fastab_settings::Settings::new_fake());
    let specs = tempfile::tempdir().unwrap();
    let engine = EngineClient::spawn(specs.path().to_path_buf()).unwrap();
    let (proxy, _events) = crate::event_loop::channel();
    let figterm = Arc::new(FigtermState::new());
    let platform = Arc::new(PlatformState::new(proxy.clone()));
    let session = Uuid::new_v4();
    let (sender, receiver) = fastab_remote_ipc::outbox::channel();
    figterm.insert(FigtermSession {
        id: session,
        secret: String::new(),
        sender,
        dead_since: None,
        edit_buffer: EditBuffer::default(),
        last_receive: tokio::time::Instant::now(),
        context: None,
        flattened_env: Arc::new(Vec::new()),
        terminal_cursor_coordinates: None,
        current_session_metrics: None,
        intercept: InterceptMode::Unlocked,
        intercept_global: InterceptMode::Unlocked,
    });
    drop(receiver);
    cx.update(|cx| {
        let controller = OverlayController::start(cx, engine, proxy, figterm.clone(), platform).unwrap();
        controller.set_session(session);
        *controller.self_insertion.lock().unwrap() = Some("old prediction".into());
        assert!(!controller.insert_text("x", 0, false, &figterm, cx));
        assert!(controller.self_insertion.lock().unwrap().is_none());
        set_intercept_flags(&figterm, session, true, true);
        let session = figterm.get(&session).unwrap();
        assert_eq!(session.intercept, InterceptMode::Unlocked);
        assert_eq!(session.intercept_global, InterceptMode::Unlocked);
        assert!(session.sender.diagnostics().closed);
    });
}
