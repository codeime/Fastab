use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::term::ShellState;
use fastab_proto::remote::Hostbound;
use fastab_proto::remote_hooks::{hook_to_message, new_postexec_hook, new_preexec_hook, new_prompt_hook};
// use fastab_telemetry::sentry::configure_scope;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tracing::level_filters::LevelFilter;
use tracing::{debug, error};

use crate::history::{HistoryCommand, HistorySender};
use crate::ipc::{ContextAdmission, RemoteSender};
use crate::{INSERT_ON_NEW_CMD, MainLoopEvent, shell_context_epoch, shell_state_to_context};

// Parsed events never cross an async producer boundary. The main loop drains
// this collection after every byte, before parsing another OSC.
#[derive(Clone, Default)]
pub(crate) struct LocalEvents(Arc<LocalEventQueue>);

#[derive(Default)]
struct LocalEventQueue {
    events: Mutex<VecDeque<MainLoopEvent>>,
    pending: AtomicBool,
}

impl LocalEvents {
    fn push(&self, event: MainLoopEvent) {
        self.0.events.lock().unwrap().push_back(event);
        self.0.pending.store(true, Ordering::Release);
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.0.pending.load(Ordering::Acquire)
    }

    pub(crate) fn pop(&self) -> Option<MainLoopEvent> {
        let mut events = self.0.events.lock().unwrap();
        let event = events.pop_front();
        self.0.pending.store(!events.is_empty(), Ordering::Release);
        event
    }
}

pub(crate) struct EventHandler {
    socket_sender: RemoteSender,
    history_sender: HistorySender,
    local_events: LocalEvents,
    csi_u_enabled: bool,
}

impl EventHandler {
    pub(crate) fn new(
        socket_sender: RemoteSender,
        history_sender: HistorySender,
        local_events: LocalEvents,
        csi_u_enabled: bool,
    ) -> Self {
        Self {
            socket_sender,
            history_sender,
            local_events,
            csi_u_enabled,
        }
    }

    fn send_full_context_hook(&self, message: Hostbound) {
        if let Some(sender) = self.socket_sender.current() {
            let _ = sender.try_send_with_context(
                message,
                ContextAdmission {
                    full_context: true,
                    environment_epoch: Some(shell_context_epoch()),
                },
            );
        }
    }
}

impl EventListener for EventHandler {
    fn send_event(&self, event: Event<'_>, shell_state: &ShellState) {
        debug!(?event, ?shell_state, "Handling event");
        match event {
            Event::Prompt => {
                let context = shell_state_to_context(shell_state);
                let hook = new_prompt_hook(Some(context));
                let message = hook_to_message(hook);

                let insert_on_new_cmd = INSERT_ON_NEW_CMD.lock().unwrap().take();

                if let Some(cwd) = &shell_state.local_context.current_working_directory {
                    if cwd.exists() {
                        std::env::set_current_dir(cwd).ok();
                    }
                }

                if let Some(pending) = insert_on_new_cmd {
                    if pending.origin.is_current(self.socket_sender.phase().ready_generation()) {
                        self.local_events.push(MainLoopEvent::Insert {
                            insert: pending.text.into_bytes(),
                            unlock: false,
                            bracketed: pending.bracketed,
                            execute: pending.execute,
                            origin: pending.origin,
                        });
                    }
                }

                self.local_events.push(MainLoopEvent::SetImmediateMode(false));

                self.send_full_context_hook(message);

                if self.csi_u_enabled {
                    self.local_events.push(MainLoopEvent::SetCsiU);
                }
            },
            Event::PreExec => {
                let context = shell_state_to_context(shell_state);
                let hook = new_preexec_hook(Some(context));
                let message = hook_to_message(hook);

                self.local_events.push(MainLoopEvent::UnlockInterception);
                self.local_events.push(MainLoopEvent::SetImmediateMode(true));

                self.send_full_context_hook(message);

                if self.csi_u_enabled {
                    self.local_events.push(MainLoopEvent::UnsetCsiU);
                }
            },
            Event::CommandInfo(command_info) => {
                let context = shell_state_to_context(shell_state);
                let hook = new_postexec_hook(context, command_info.command.clone(), command_info.exit_code);
                let message = hook_to_message(hook);
                self.send_full_context_hook(message);

                if let Err(err) = self.history_sender.send(HistoryCommand::Insert(command_info.clone())) {
                    error!(%err, "Sender error");
                }
            },
            Event::ShellChanged => {
                // let shell = &shell_state.local_context.shell;
                // configure_scope(|scope| {
                //     if let Some(shell) = shell {
                //         scope.set_tag("shell", shell);
                //     }
                // });
            },
        }
    }

    fn log_level_event(&self, level: Option<String>) {
        if let Err(err) = fastab_log::set_log_level(level.unwrap_or_else(|| LevelFilter::INFO.to_string())) {
            error!(%err, "Failed to set log level");
        }
    }
}
