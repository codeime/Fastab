use super::*;
use fastab_remote_ipc::figterm::{EditBuffer, FigtermSession};

fn row(name: &str, kind: &str) -> SuggestionItem {
    SuggestionItem {
        name: name.into(),
        kind: kind.into(),
        ..Default::default()
    }
}

fn insertions(commands: &flume::Receiver<FigtermCommand>) -> Vec<(Option<String>, Option<i64>, Option<bool>)> {
    commands
        .try_iter()
        .filter_map(|command| match command {
            FigtermCommand::InsertText {
                insertion,
                deletion,
                immediate,
                ..
            } => Some((insertion, deletion, immediate)),
            _ => None,
        })
        .collect()
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
    let (sender, commands) = flume::unbounded();
    let (on_close_tx, _) = tokio::sync::broadcast::channel(1);
    // Capture the real controller's output without connecting to a user's PTY.
    figterm.insert(FigtermSession {
        id: session,
        secret: String::new(),
        sender,
        writer: None,
        dead_since: None,
        edit_buffer: EditBuffer::default(),
        last_receive: tokio::time::Instant::now(),
        context: None,
        flattened_env: Arc::new(Vec::new()),
        terminal_cursor_coordinates: None,
        current_session_metrics: None,
        response_map: HashMap::new(),
        nonce_counter: Arc::new(AtomicU64::new(0)),
        on_close_tx,
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
                assert!(insertions(&commands).is_empty(), "Tab accepted {kind} at {selected}");
                let state = controller.state.read(cx);
                assert!(state.visible);
                assert_eq!(state.items.len(), 2);
                assert_eq!(state.selected, selected);
                controller.handle_action(enter, session, &figterm, cx);
                assert_eq!(insertions(&commands), newline, "Enter must still accept {kind}");
            });
        }
        cx.update(|cx| {
            // Preserve the existing sole-action Full acceptance contract.
            show(&mut controller, vec![action], "status", 0, cx);
            controller.handle_action(tab, session, &figterm, cx);
            assert_eq!(insertions(&commands), newline, "sole {kind} must be accepted once");
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
                insertions(&commands),
                vec![(Some(expected.into()), Some(0), Some(false))]
            );
        });
    }
    cx.update(|cx| {
        show(&mut controller, Vec::new(), "", 0, cx);
        controller.handle_action(tab, session, &figterm, cx);
        assert!(insertions(&commands).is_empty());
        show(&mut controller, vec![row("status", "subcommand")], "status", 0, cx);
        controller.handle_action("execute", session, &figterm, cx);
        assert_eq!(insertions(&commands), newline, "explicit execution stays available");
    });
}
