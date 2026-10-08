use std::time::Duration;

use async_trait::async_trait;
use fastab_proto::local::{
    self, BundleMetadataCommand, BundleMetadataResponse, CommandResponse, DebugModeCommand, DevtoolsCommand,
    DumpStateCommand, DumpStateResponse, InputMethodAction, InputMethodCommand, LogLevelCommand, LogLevelResponse,
    LoginCommand, LogoutCommand, OpenUiElementCommand, PromptAccessibilityCommand, QuitCommand, RestartCommand,
    RestartSettingsListenerCommand, UiElement, UpdateCommand, command, command_response, devtools_command,
    dump_state_command,
};
use fastab_util::directories;

use crate::{BufferedUnixStream, Error, RecvError, SendRecvMessage};

type Result<T, E = crate::Error> = std::result::Result<T, E>;

pub async fn restart_settings_listener() -> Result<()> {
    let command = command::Command::RestartSettingsListener(RestartSettingsListenerCommand {});
    send_command_to_socket(command).await
}

pub async fn open_ui_element(element: UiElement, route: Option<String>) -> Result<()> {
    let command = command::Command::OpenUiElement(OpenUiElementCommand {
        element: element.into(),
        route,
    });
    send_command_to_socket(command).await
}

pub async fn toggle_debug_mode() -> Result<Option<local::CommandResponse>> {
    let command = command::Command::DebugMode(DebugModeCommand {
        set_debug_mode: None,
        toggle_debug_mode: Some(true),
    });
    send_recv_command_to_socket(command).await
}

pub async fn set_debug_mode(debug_mode: bool) -> Result<Option<local::CommandResponse>> {
    let command = command::Command::DebugMode(DebugModeCommand {
        set_debug_mode: Some(debug_mode),
        toggle_debug_mode: None,
    });
    send_recv_command_to_socket(command).await
}

pub async fn set_log_level(level: String) -> Result<Option<String>> {
    let command = command::Command::LogLevel(LogLevelCommand { level });
    let resp: Option<local::CommandResponse> = send_recv_command_to_socket(command).await?;

    match resp {
        Some(CommandResponse {
            response: Some(command_response::Response::LogLevel(LogLevelResponse { old_level })),
            ..
        }) => Ok(old_level),
        _ => Err(RecvError::InvalidMessageType.into()),
    }
}

pub async fn dump_state_command(component: dump_state_command::Type) -> Result<DumpStateResponse> {
    let command = command::Command::DumpState(DumpStateCommand {
        r#type: component.into(),
    });
    let resp: Option<local::CommandResponse> = send_recv_command_to_socket(command).await?;
    decode_dump_state_response(resp)
}

fn decode_dump_state_response(resp: Option<CommandResponse>) -> Result<DumpStateResponse> {
    match resp {
        Some(CommandResponse {
            response: Some(command_response::Response::DumpState(resp)),
            ..
        }) => Ok(resp),
        Some(CommandResponse {
            response: Some(command_response::Response::Error(error)),
            ..
        }) => Err(Error::Command {
            exit_code: error.exit_code,
            message: error.message.unwrap_or_else(|| "No error message returned".to_owned()),
        }),
        _ => Err(RecvError::InvalidMessageType.into()),
    }
}

pub async fn bundle_metadata_command() -> Result<BundleMetadataResponse> {
    let command = command::Command::BundleMetadata(BundleMetadataCommand {});
    let resp: Option<local::CommandResponse> = send_recv_command_to_socket(command).await?;

    match resp {
        Some(CommandResponse {
            response: Some(command_response::Response::BundleMetadata(resp)),
            ..
        }) => Ok(resp),
        _ => Err(RecvError::InvalidMessageType.into()),
    }
}

pub async fn input_method_command(action: InputMethodAction) -> Result<()> {
    let command = command::Command::InputMethod(InputMethodCommand {
        actions: Some(action.into()),
    });
    send_command_to_socket(command).await
}

pub async fn prompt_accessibility_command() -> Result<()> {
    let command = command::Command::PromptAccessibility(PromptAccessibilityCommand {});
    send_command_to_socket(command).await
}

pub async fn update_command(force: bool) -> Result<Option<CommandResponse>> {
    let command = command::Command::Update(UpdateCommand { force });
    send_recv_command_to_socket_with_timeout(command, std::time::Duration::from_secs(120)).await
}

pub async fn restart_command() -> Result<()> {
    let command = command::Command::Restart(RestartCommand {});
    send_command_to_socket(command).await
}

pub async fn quit_command() -> Result<()> {
    let command = command::Command::Quit(QuitCommand {});
    send_command_to_socket(command).await
}

pub async fn login_command() -> Result<()> {
    let command = command::Command::Login(LoginCommand {});
    send_command_to_socket(command).await
}

pub async fn logout_command() -> Result<()> {
    let command = command::Command::Logout(LogoutCommand {});
    send_command_to_socket(command).await
}

pub async fn devtools_command(window: devtools_command::Window) -> Result<()> {
    let command = command::Command::Devtools(DevtoolsCommand { window: window.into() });
    send_command_to_socket(command).await
}

#[async_trait]
pub trait LocalIpc: SendRecvMessage {
    async fn send_hook(&mut self, hook: local::Hook) -> Result<()>;
    async fn send_command(&mut self, command: local::command::Command, response: bool) -> Result<()>;
    async fn send_recv_command(
        &mut self,
        command: local::command::Command,
        timeout: Duration,
    ) -> Result<Option<local::CommandResponse>>;
}

#[async_trait]
impl<C> LocalIpc for C
where
    C: SendRecvMessage + Send,
{
    /// Send a hook to the desktop app
    async fn send_hook(&mut self, hook: local::Hook) -> Result<()> {
        let message = local::LocalMessage {
            r#type: Some(local::local_message::Type::Hook(hook)),
        };
        Ok(self.send_message(message).await?)
    }

    /// Send a command to the desktop app
    async fn send_command(&mut self, command: local::command::Command, response: bool) -> Result<()> {
        let message = local::LocalMessage {
            r#type: Some(local::local_message::Type::Command(local::Command {
                id: None,
                no_response: Some(!response),
                command: Some(command),
            })),
        };
        Ok(self.send_message(message).await?)
    }

    /// Send a command to and recv a response from the desktop app, with a configurable timeout on
    /// the response
    async fn send_recv_command(
        &mut self,
        command: local::command::Command,
        timeout: Duration,
    ) -> Result<Option<local::CommandResponse>> {
        self.send_command(command, true).await?;
        Ok(tokio::time::timeout(timeout, self.recv_message())
            .await
            .or(Err(Error::Timeout))??)
    }
}

/// Send a hook directly to the Fig socket
pub async fn send_hook_to_socket(hook: local::Hook) -> Result<()> {
    let path = directories::desktop_socket_path()?;
    let mut conn = BufferedUnixStream::connect_timeout(&path, Duration::from_secs(3)).await?;
    conn.send_hook(hook).await
}

pub async fn send_command_to_socket(command: local::command::Command) -> Result<()> {
    let path = directories::desktop_socket_path()?;
    let mut conn = BufferedUnixStream::connect_timeout(&path, Duration::from_secs(3)).await?;
    conn.send_command(command, false).await
}

pub async fn send_recv_command_to_socket(command: local::command::Command) -> Result<Option<local::CommandResponse>> {
    send_recv_command_to_socket_with_timeout(command, Duration::from_secs(2)).await
}

pub async fn send_recv_command_to_socket_with_timeout(
    command: local::command::Command,
    timeout: Duration,
) -> Result<Option<local::CommandResponse>> {
    let path = directories::desktop_socket_path()?;
    let mut conn = BufferedUnixStream::connect_timeout(&path, Duration::from_secs(3)).await?;
    conn.send_recv_command(command, timeout).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BufferedReader, RecvMessage, SendMessage};

    async fn dump_state_round_trip(response: command_response::Response) -> Result<DumpStateResponse> {
        let (client, server) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            let mut server = BufferedReader::new(server);
            let request = server.recv_message::<local::LocalMessage>().await.unwrap().unwrap();
            let Some(local::local_message::Type::Command(local::Command {
                command: Some(command::Command::DumpState(request)),
                no_response: Some(false),
                ..
            })) = request.r#type
            else {
                panic!("expected a dump-state request with a response");
            };
            assert_eq!(request.r#type, dump_state_command::Type::DumpStateEngine as i32);
            server
                .send_message(CommandResponse {
                    id: None,
                    response: Some(response),
                })
                .await
                .unwrap();
        });
        let response = BufferedReader::new(client)
            .send_recv_command(
                command::Command::DumpState(DumpStateCommand {
                    r#type: dump_state_command::Type::DumpStateEngine.into(),
                }),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
        server.await.unwrap();
        decode_dump_state_response(response)
    }

    #[tokio::test]
    async fn dump_state_preserves_server_errors_through_the_framed_protocol() {
        let error = dump_state_round_trip(command_response::Response::Error(local::ErrorResponse {
            exit_code: Some(17),
            message: Some("Engine resource diagnostics timed out".to_owned()),
        }))
        .await
        .unwrap_err();
        assert!(matches!(error, Error::Command { exit_code: Some(17), ref message }
            if message == "Engine resource diagnostics timed out"));
        assert_eq!(
            error.to_string(),
            "desktop command failed: Engine resource diagnostics timed out"
        );

        let error = dump_state_round_trip(command_response::Response::Error(local::ErrorResponse {
            exit_code: None,
            message: None,
        }))
        .await
        .unwrap_err();
        assert!(matches!(error, Error::Command { exit_code: None, ref message }
            if message == "No error message returned"));
    }

    #[tokio::test]
    async fn dump_state_accepts_its_success_and_rejects_other_response_types() {
        let json = "{\"engine\":null}".to_owned();
        let response = dump_state_round_trip(command_response::Response::DumpState(DumpStateResponse {
            json: json.clone(),
        }))
        .await
        .unwrap();
        assert_eq!(response.json, json);
        assert!(matches!(
            dump_state_round_trip(command_response::Response::Success(local::SuccessResponse {
                message: None
            }))
            .await,
            Err(Error::Recv(RecvError::InvalidMessageType))
        ));
    }
}
