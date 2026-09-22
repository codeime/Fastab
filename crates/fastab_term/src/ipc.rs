//! Utiities for IPC with Tauri App

mod outbox;

pub(crate) use outbox::{ContextAdmission, ContextProgress, Generation, GenerationSender, RemoteSender, RequestOrigin};

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::Result;
use fastab_ipc::{BufferedReader, RecvMessage, SendMessage};
use fastab_proto::FigProtobufEncodable;
use fastab_proto::figterm::{FigtermRequestMessage, FigtermResponseMessage};
use fastab_proto::remote::hostbound::Handshake;
use fastab_proto::remote::{Clientbound, Hostbound, clientbound, hostbound};
use fastab_util::{PTY_BINARY_NAME, directories, gen_hex_string};
use flume::{Receiver, Sender, unbounded};
use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::oneshot;
use tokio::time::{MissedTickBehavior, interval, timeout};
use tracing::{debug, error, info, trace};

// These use the existing connect/retry timescale. The handshake and write
// deadlines are engineering bounds, not measured remote-network optima.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) struct RemoteIncoming {
    pub(crate) generation: Generation,
    pub(crate) message: Clientbound,
}

struct ForwardedConnection {
    reader: MessageSource,
    writer: MessageSink,
    child: Option<Child>,
}

#[allow(dead_code)]
#[pin_project(project = MessageSourceProj)]
enum MessageSource {
    UnixStream(#[pin] tokio::io::ReadHalf<tokio::net::UnixStream>),
    ChildStdout(#[pin] ChildStdout),
}

impl AsyncRead for MessageSource {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        match self.project() {
            MessageSourceProj::UnixStream(stream) => stream.poll_read(cx, buf),
            MessageSourceProj::ChildStdout(stdout) => stdout.poll_read(cx, buf),
        }
    }
}

#[allow(dead_code)]
#[pin_project(project = MessageSinkProj)]
enum MessageSink {
    UnixStream(#[pin] tokio::io::WriteHalf<tokio::net::UnixStream>),
    ChildStdin(#[pin] ChildStdin),
}

impl AsyncWrite for MessageSink {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, io::Error>> {
        match self.project() {
            MessageSinkProj::UnixStream(stream) => stream.poll_write(cx, buf),
            MessageSinkProj::ChildStdin(stdin) => stdin.poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match self.project() {
            MessageSinkProj::UnixStream(stream) => stream.poll_flush(cx),
            MessageSinkProj::ChildStdin(stdin) => stdin.poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match self.project() {
            MessageSinkProj::UnixStream(stream) => stream.poll_shutdown(cx),
            MessageSinkProj::ChildStdin(stdin) => stdin.poll_shutdown(cx),
        }
    }
}

async fn get_forwarded_stream() -> Result<ForwardedConnection> {
    #[cfg(target_os = "linux")]
    if fastab_util::system_info::in_wsl() {
        use std::process::Stdio;

        use anyhow::Context as AnyhowContext;

        let mut child = tokio::process::Command::new("fig.exe")
            .args(["_", "stream-from-socket"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;

        let stdin = child.stdin.take().context("Failed to open stdin")?;
        let stdout = child.stdout.take().context("Failed to open stdout")?;

        return Ok(ForwardedConnection {
            reader: MessageSource::ChildStdout(stdout),
            writer: MessageSink::ChildStdin(stdin),
            child: Some(child),
        });
    }

    let socket = directories::remote_socket_path()?;
    let stream = fastab_ipc::socket_connect_timeout(&socket, CONNECTION_TIMEOUT).await?;
    let (reader, writer) = tokio::io::split(stream);
    Ok(ForwardedConnection {
        reader: MessageSource::UnixStream(reader),
        writer: MessageSink::UnixStream(writer),
        child: None,
    })
}

/// Spawns a local unix socket for communicating with figterm on a local machine
pub async fn spawn_figterm_ipc(
    session_id: impl std::fmt::Display,
) -> Result<Receiver<(FigtermRequestMessage, Sender<FigtermResponseMessage>)>> {
    trace!("Spawning incoming receiver");

    let (incoming_tx, incoming_rx) = unbounded();

    let socket_path = directories::figterm_socket_path(session_id)?;
    if let Some(parent) = socket_path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            error!(%err, "Failed to create {PTY_BINARY_NAME} socket directory");
        }

        #[cfg(unix)]
        {
            use std::fs::Permissions;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, Permissions::from_mode(0o700))?;
        }
    }

    tokio::fs::remove_file(&socket_path).await.ok();
    let socket_listener = tokio::net::UnixListener::bind(&socket_path)?;

    tokio::spawn(async move {
        loop {
            if let Ok((stream, _)) = socket_listener.accept().await {
                let incoming_tx = incoming_tx.clone();

                let (read_half, mut write_half) = tokio::io::split(stream);
                let (response_tx, response_rx) = unbounded::<FigtermResponseMessage>();

                tokio::spawn(async move {
                    let mut read_half = BufferedReader::new(read_half);
                    let mut rx_thread = tokio::spawn(async move {
                        loop {
                            match read_half.recv_message::<FigtermRequestMessage>().await {
                                Ok(Some(message)) => {
                                    // debug!("Received message: {message:?}");
                                    incoming_tx
                                        .clone()
                                        .send_async((message, response_tx.clone()))
                                        .await
                                        .unwrap();
                                },
                                Ok(None) => {
                                    debug!("Received EOF");
                                    break;
                                },
                                Err(err) => {
                                    error!("Error receiving message: {err}");
                                    break;
                                },
                            }
                        }
                    });

                    loop {
                        tokio::select! {
                            // Break once the rx_thread quits
                            _ = &mut rx_thread => break,
                            res = response_rx.recv_async() => {
                                match res {
                                    Ok(response) => {
                                        match response.encode_fastab_protobuf() {
                                            Ok(protobuf) => {
                                                if let Err(err) = write_half.write_all(&protobuf).await {
                                                    error!(%err, "Failed to send response");
                                                    break;
                                                }
                                            },
                                            Err(err) => error!(%err, "Failed to encode protobuf")
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                        }
                    }
                });
            }
        }
    });

    Ok(incoming_rx)
}

/// Connects to the desktop app and allows for a remote connection from remote hosts
pub(crate) async fn spawn_remote_ipc(
    session_id: String,
    parent_id: Option<String>,
) -> Result<(RemoteSender, Receiver<RemoteIncoming>, oneshot::Sender<()>)> {
    let (stop_ipc_tx, mut stop_ipc_rx) = oneshot::channel::<()>();
    let outgoing = RemoteSender::new();
    let supervisor = outgoing.clone();
    // This change bounds the outbox; it does not claim to bound incoming
    // protocol parsing or the local figterm listener.
    let (incoming_tx, incoming_rx) = unbounded::<RemoteIncoming>();

    tokio::spawn(async move {
        let mut interval = interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let secret = gen_hex_string();

        loop {
            tokio::select! {
                _ = &mut stop_ipc_rx => {
                    supervisor.stop();
                    break;
                }
                _ = interval.tick() => {}
            }
            let Some(generation) = (match supervisor.begin_attempt() {
                outbox::BeginAttempt::Started(generation) => Some(generation),
                outbox::BeginAttempt::Busy => {
                    // In-flight accounting has not cleared yet. Wait for the
                    // next reconnect tick instead of permanently stopping IPC.
                    continue;
                },
                outbox::BeginAttempt::Stopped => None,
            }) else {
                break;
            };
            let connection = tokio::select! {
                _ = &mut stop_ipc_rx => {
                    supervisor.stop();
                    break;
                }
                result = timeout(CONNECTION_TIMEOUT, get_forwarded_stream()) => match result {
                    Ok(Ok(connection)) => connection,
                    Ok(Err(err)) => {
                        supervisor.retire(generation, &format!("connect failed: {err}"));
                        continue;
                    }
                    Err(err) => {
                        supervisor.retire(generation, &format!("connect timed out: {err}"));
                        continue;
                    }
                }
            };
            let ForwardedConnection {
                reader,
                mut writer,
                mut child,
            } = connection;
            let mut reader = BufferedReader::new(reader);
            supervisor.set_handshaking(generation);

            // Both halves are scoped to this select. Finishing either half,
            // invalidating admission, or stopping drops the other half's
            // future (including an in-flight frame) before the next generation.
            let result: Result<()> = {
                let run_connection = async {
                    timeout(CONNECTION_TIMEOUT, async {
                        writer
                            .send_message(Hostbound {
                                packet: Some(hostbound::Packet::Handshake(Handshake {
                                    id: session_id.clone(),
                                    parent_id: parent_id.clone(),
                                    secret: secret.clone(),
                                })),
                            })
                            .await?;
                        loop {
                            let Some(message) = reader.recv_message::<Clientbound>().await? else {
                                anyhow::bail!("EOF awaiting handshake");
                            };
                            if let Some(clientbound::Packet::HandshakeResponse(response)) = message.packet {
                                if !response.success {
                                    anyhow::bail!("handshake rejected");
                                }
                                return Ok::<(), anyhow::Error>(());
                            }
                        }
                    })
                    .await??;
                    supervisor.set_ready(generation);
                    info!(?generation, "Remote handshake succeeded");

                    let receive = async {
                        while let Some(message) = reader.recv_message::<Clientbound>().await? {
                            incoming_tx
                                .send(RemoteIncoming { generation, message })
                                .map_err(|err| anyhow::anyhow!("remote incoming receiver closed: {err}"))?;
                        }
                        Err::<(), anyhow::Error>(anyhow::anyhow!("remote reader reached EOF"))
                    };
                    let send = async {
                        while let Some(mut frame) = supervisor.next_frame(generation).await {
                            frame.write_to(&mut writer).await?;
                        }
                        Ok::<(), anyhow::Error>(())
                    };
                    tokio::select! {
                        result = receive => result,
                        result = send => result,
                    }
                };
                tokio::select! {
                    _ = &mut stop_ipc_rx => {
                        supervisor.stop();
                        Ok(())
                    }
                    _ = supervisor.invalidated(generation) => Ok(()),
                    status = async {
                        match child.as_mut() {
                            Some(child) => child.wait().await,
                            None => std::future::pending().await,
                        }
                    } => Err(anyhow::anyhow!("forwarder exited: {status:?}")),
                    result = run_connection => result,
                }
            };
            let reason = match result {
                Ok(()) => "connection cancelled".to_owned(),
                Err(err) => format!("connection failed: {err}"),
            };
            supervisor.retire(generation, &reason);
            drop(reader);
            drop(writer);
            if let Some(mut child) = child {
                // A WSL forwarder is owned by this connection, not by a
                // detached wait task. Close its pipes, terminate and reap it.
                if let Err(err) = child.start_kill() {
                    debug!(%err, "Could not stop remote forwarder");
                }
                let mut stopping = supervisor.phase() == outbox::ConnectionPhase::Stopped;
                let reaped = {
                    let wait = timeout(CONNECTION_TIMEOUT, child.wait());
                    tokio::pin!(wait);
                    loop {
                        tokio::select! {
                            result = &mut wait => break match result {
                                Ok(Ok(_)) => true,
                                Ok(Err(err)) => {
                                    error!(%err, "Could not confirm remote forwarder exit");
                                    false
                                }
                                Err(err) => {
                                    error!(%err, "Remote forwarder cleanup timed out; stopping IPC without reconnecting");
                                    false
                                }
                            },
                            _ = &mut stop_ipc_rx, if !stopping => {
                                stopping = true;
                                supervisor.stop();
                            }
                        }
                    }
                };
                if !reaped {
                    // kill_on_drop remains a best-effort fallback. Do not
                    // accumulate forwarders whose exit we could not confirm.
                    supervisor.stop();
                }
            }
            if supervisor.phase() == outbox::ConnectionPhase::Stopped {
                break;
            }
        }
    });

    Ok((outgoing, incoming_rx, stop_ipc_tx))
}
