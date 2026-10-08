use std::{net::IpAddr, path::PathBuf, time::Duration};

use acd_protocol::{ClientInfo, ClientMessage, ErrorCode, Machine, PROTOCOL};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{broadcast, mpsc, oneshot, watch},
    time::{Instant, timeout_at},
};
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};
use uuid::Uuid;

use crate::{
    backoff::Backoff,
    commands::{Command, Commands, NotSent, Outcome},
    endpoint::{self, Endpoint},
    local,
    session::{Applied, EpochChange, Session},
    tls,
};

#[derive(Clone, Debug)]
pub enum Credential {
    Token(String),
    LocalFile(PathBuf),
}

#[derive(Clone, Debug)]
pub struct MachineConfig {
    pub key: String,
    pub instance: u64,
    pub endpoint: Endpoint,
    pub credential: Credential,
    pub client: ClientInfo,
    pub timing: crate::Timing,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MachineStatus {
    Connecting,
    Syncing,
    Online,
    Offline { since_ms: f64, error: String },
    AuthFailed { message: String },
    IdentityChanged { message: String },
    Incompatible { message: String },
    LocalUnavailable { message: String },
}

#[derive(Clone, Debug)]
pub struct MachineState {
    pub instance: u64,
    pub status: MachineStatus,
    pub session: Session,
    pub conn_gen: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MachineEvent {
    Opened(acd_protocol::Request),
    Resolved {
        request_id: String,
        agent_id: String,
        by: acd_protocol::ResolvedBy,
        option_id: Option<String>,
    },
    Welcomed {
        generation: u64,
        change: EpochChange,
    },
    Ready {
        generation: u64,
    },
    ConnectionEnded {
        generation: u64,
    },
}

enum Control {
    DropConnection,
    RetryNow,
    Submit(Command, Option<u64>, oneshot::Sender<Outcome>),
}
#[derive(Clone)]
pub struct MachineHandle {
    state: watch::Receiver<MachineState>,
    events: broadcast::Sender<(u64, MachineEvent)>,
    ctl: mpsc::Sender<Control>,
    cancel: watch::Sender<bool>,
}

impl MachineHandle {
    pub fn state(&self) -> watch::Receiver<MachineState> {
        self.state.clone()
    }
    pub fn events(&self) -> broadcast::Receiver<(u64, MachineEvent)> {
        self.events.subscribe()
    }
    pub fn drop_connection(&self) {
        let _ = self.ctl.try_send(Control::DropConnection);
    }
    pub fn retry_now(&self) {
        let _ = self.ctl.try_send(Control::RetryNow);
    }
    pub fn cancel(&self) {
        self.cancel.send_replace(true);
    }
    pub async fn submit(&self, cmd: Command, expect_generation: Option<u64>) -> Outcome {
        let (tx, rx) = oneshot::channel();
        if self
            .ctl
            .send(Control::Submit(cmd, expect_generation, tx))
            .await
            .is_err()
        {
            return Outcome::NotSent(NotSent::Offline);
        }
        rx.await.unwrap_or(Outcome::Uncertain)
    }
}

pub fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
        * 1000.0
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
type Ws = WebSocketStream<Box<dyn Io>>;
enum End {
    Offline(String),
    Resync(&'static str),
    TooSlow,
    Unauthorized,
    PinMismatch,
    Incompatible(String),
    Local(String),
    Stopped,
}

async fn open(cfg: &MachineConfig, deadline: Instant) -> Result<(Ws, String), End> {
    timeout_at(deadline, async {
        let token = match &cfg.credential {
            Credential::Token(t) => t.clone(),
            Credential::LocalFile(dir) => {
                local::read_local_token(dir).map_err(|e| End::Local(e.to_string()))?
            }
        };
        match &cfg.endpoint {
            Endpoint::Local { config_dir } => {
                let stream = tokio::net::UnixStream::connect(local::socket_path(config_dir))
                    .await
                    .map_err(|e| End::Offline(e.to_string()))?;
                let (ws, _) = tokio_tungstenite::client_async(
                    "ws://localhost/",
                    Box::new(stream) as Box<dyn Io>,
                )
                .await
                .map_err(|e| End::Offline(e.to_string()))?;
                Ok((ws, token))
            }
            Endpoint::Tcp { host, port, pin } => {
                let addrs = endpoint::vet(host, *port)
                    .await
                    .map_err(|e| End::Offline(e.to_string()))?;
                let mut last = "no vetted address connected".to_string();
                for addr in addrs {
                    let tcp = match tokio::net::TcpStream::connect(addr).await {
                        Ok(s) => s,
                        Err(e) => {
                            last = e.to_string();
                            continue;
                        }
                    };
                    let server_name = rustls::pki_types::ServerName::try_from(host.clone())
                        .or_else(|_| {
                            host.parse::<IpAddr>()
                                .map(|ip| match ip {
                                    IpAddr::V4(v4) => {
                                        rustls::pki_types::ServerName::IpAddress(v4.into())
                                    }
                                    IpAddr::V6(v6) => {
                                        rustls::pki_types::ServerName::IpAddress(v6.into())
                                    }
                                })
                                .map_err(|_| ())
                        })
                        .unwrap_or_else(|_| "acd.invalid".try_into().expect("fixed server name"));
                    let tls_handshake = tokio_rustls::TlsConnector::from(tls::client_config(pin))
                        .connect(server_name, tcp);
                    let stream = match timeout_at(deadline, tls_handshake).await {
                        Err(_) => return Err(End::Offline("connection timed out".into())),
                        Ok(Ok(s)) => s,
                        Ok(Err(e)) => {
                            let io = std::io::Error::other(e.to_string());
                            if e.to_string().contains(tls::PIN_MISMATCH)
                                || tls::is_pin_mismatch(&io)
                            {
                                return Err(End::PinMismatch);
                            }
                            last = e.to_string();
                            continue;
                        }
                    };
                    let authority_host = match host.parse::<std::net::Ipv6Addr>() {
                        Ok(ip) => format!("[{ip}]"),
                        Err(_) => host.trim_matches(['[', ']']).to_string(),
                    };
                    let url = format!("wss://{authority_host}:{port}/");
                    match tokio_tungstenite::client_async(url, Box::new(stream) as Box<dyn Io>)
                        .await
                    {
                        Ok((ws, _)) => return Ok((ws, token)),
                        Err(e) => {
                            last = e.to_string();
                        }
                    }
                }
                Err(End::Offline(last))
            }
        }
    })
    .await
    .unwrap_or_else(|_| Err(End::Offline("connection timed out".into())))
}

fn credential_hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn close_code(msg: &Message) -> Option<u16> {
    if let Message::Close(Some(frame)) = msg {
        Some(frame.code.into())
    } else {
        None
    }
}
async fn send_json(ws: &mut Ws, value: &impl Serialize) -> Result<(), End> {
    let text = serde_json::to_string(value).map_err(|e| End::Offline(e.to_string()))?;
    ws.send(Message::Text(text.into()))
        .await
        .map_err(|e| End::Offline(e.to_string()))
}
async fn next_server(ws: &mut Ws, deadline: Instant) -> Result<acd_protocol::ServerMessage, End> {
    loop {
        let msg = timeout_at(deadline, ws.next())
            .await
            .map_err(|_| End::Offline("handshake timed out".into()))?
            .ok_or_else(|| End::Offline("closed".into()))?
            .map_err(|e| End::Offline(e.to_string()))?;
        if let Some(code) = close_code(&msg) {
            return Err(end_for_close(code));
        }
        match msg {
            Message::Text(text) => match acd_protocol::decode_server(&text) {
                Ok(Some(m)) => return Ok(m),
                Ok(None) => {}
                Err(_) => return Err(End::Resync("undecodable frame")),
            },
            Message::Ping(v) => {
                ws.send(Message::Pong(v))
                    .await
                    .map_err(|e| End::Offline(e.to_string()))?;
            }
            Message::Close(_) => return Err(End::Offline("closed".into())),
            _ => {}
        }
    }
}
fn end_for_close(code: u16) -> End {
    match code {
        4400 => End::Incompatible("unsupported protocol".into()),
        4401 => End::Unauthorized,
        4408 => End::TooSlow,
        _ => End::Offline("closed".into()),
    }
}

async fn handshake(
    ws: &mut Ws,
    token: &str,
    client: &ClientInfo,
    resume: Option<acd_protocol::Cursor>,
    deadline: Instant,
) -> Result<(Machine, Vec<String>, String, u64), End> {
    timeout_at(deadline, async {
        send_json(
            ws,
            &ClientMessage::Hello {
                protocol: PROTOCOL.into(),
                token: token.into(),
                client: client.clone(),
                resume,
            },
        )
        .await?;
        loop {
            match next_server(ws, deadline).await? {
                acd_protocol::ServerMessage::Welcome {
                    machine,
                    capabilities,
                    epoch,
                    seq,
                } => return Ok((machine, capabilities, epoch, seq)),
                acd_protocol::ServerMessage::Error {
                    code: ErrorCode::UnsupportedProtocol,
                    message,
                    ..
                } => return Err(End::Incompatible(message)),
                acd_protocol::ServerMessage::Error {
                    code: ErrorCode::Unauthorized,
                    ..
                } => return Err(End::Unauthorized),
                acd_protocol::ServerMessage::Error { message, .. } => {
                    return Err(End::Offline(message));
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap_or_else(|_| Err(End::Offline("handshake timed out".into())))
}

#[allow(clippy::too_many_arguments)]
async fn serve(
    mut ws: Ws,
    session: &mut Session,
    generation: u64,
    events: &broadcast::Sender<(u64, MachineEvent)>,
    instance: u64,
    state: &watch::Sender<MachineState>,
    timing: &crate::Timing,
    ctl: &mut mpsc::Receiver<Control>,
    backoff: &mut Backoff,
    commands: &mut Commands,
    cred: [u8; 32],
    cancel: &mut watch::Receiver<bool>,
) -> End {
    let mut last_frame = tokio::time::Instant::now();
    let mut ping_deadline: Option<tokio::time::Instant> = None;
    let mut became_ready = session.ready();
    loop {
        let ping_at = ping_deadline.unwrap_or(last_frame + timing.idle_ping);
        let due_at = commands
            .next_due()
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(86400));
        tokio::select! {
            biased;
            _ = cancel.changed() => return End::Stopped,
            control = ctl.recv() => match control {
                Some(Control::DropConnection) => return End::Offline("dropped by request".into()),
                Some(Control::Submit(command, expect_gen, reply)) => {
                    let online = session.ready() && state.borrow().status == MachineStatus::Online;
            if !online || expect_gen.is_some_and(|g| g != generation) { let _ = reply.send(Outcome::NotSent(NotSent::Offline)); }
                    else if let Some(msg) = commands.begin(command, reply, &session.capabilities, cred, Instant::now())
                        && let Err(e) = send_json(&mut ws, &msg).await { return e; }
                },
                Some(Control::RetryNow) => {}, None => return End::Stopped,
            },
            _ = tokio::time::sleep_until(ping_at) => {
                if ping_deadline.is_some() { return End::Offline("no reply to ping".into()); }
                if let Err(e) = send_json(&mut ws, &ClientMessage::Ping { id: Uuid::new_v4().to_string() }).await { return e; }
                ping_deadline = Some(tokio::time::Instant::now() + timing.ping_timeout);
            }
            _ = tokio::time::sleep_until(due_at), if commands.next_due().is_some() => {
                for msg in commands.due(Instant::now(), &session.capabilities, timing) {
                    if let Err(e) = send_json(&mut ws, &msg).await { return e; }
                }
            }
            incoming = ws.next() => {
                let Some(incoming) = incoming else { return End::Offline("closed".into()); };
                let msg = match incoming { Ok(m) => m, Err(e) => return End::Offline(e.to_string()) };
                if let Some(code) = close_code(&msg) { return end_for_close(code); }
                last_frame = tokio::time::Instant::now(); ping_deadline = None;
                let text = match msg { Message::Text(t) => t.to_string(), Message::Ping(v) => { if let Err(e) = ws.send(Message::Pong(v)).await { return End::Offline(e.to_string()); } continue; }, Message::Pong(_) => continue, Message::Close(_) => return End::Offline("closed".into()), _ => continue };
                let decoded = match acd_protocol::decode_server(&text) { Ok(Some(m)) => m, Ok(None) => continue, Err(_) => return End::Resync("undecodable frame") };
                match decoded {
                    acd_protocol::ServerMessage::Result(result) => commands.on_result(&result, Instant::now(), timing),
                    acd_protocol::ServerMessage::Error { message, .. } => tracing::warn!("daemon error: {message}"),
                    acd_protocol::ServerMessage::Pong { .. } => {},
                    other => {
                        let applied = session.apply(&other);
                        match applied {
                            Applied::Resync(why) => return End::Resync(why),
                            Applied::Changed => { let status = if session.ready() { MachineStatus::Online } else { MachineStatus::Syncing }; state.send_replace(MachineState { instance, status, session: session.clone(), conn_gen: generation }); },
                            Applied::Opened(req) => { state.send_replace(MachineState { instance, status: if session.ready() { MachineStatus::Online } else { MachineStatus::Syncing }, session: session.clone(), conn_gen: generation }); let _ = events.send((instance, MachineEvent::Opened(req))); },
                            Applied::Resolved { request_id, agent_id, by, option_id } => { let _ = events.send((instance, MachineEvent::Resolved { request_id, agent_id, by, option_id })); },
                            Applied::Ignored => {}
                        }
                        if session.ready() && !became_ready {
                            became_ready = true;
                            state.send_replace(MachineState { instance, status: MachineStatus::Online, session: session.clone(), conn_gen: generation });
                            let _ = events.send((instance, MachineEvent::Ready { generation })); backoff.reset();
                        }
                    }
                }
            }
        }
    }
}

fn retryable_status(s: &MachineStatus) -> bool {
    !matches!(
        s,
        MachineStatus::AuthFailed { .. }
            | MachineStatus::IdentityChanged { .. }
            | MachineStatus::Incompatible { .. }
            | MachineStatus::LocalUnavailable { .. }
    )
}
fn publish(
    state: &watch::Sender<MachineState>,
    instance: u64,
    status: MachineStatus,
    session: &Session,
    generation: u64,
) {
    state.send_replace(MachineState {
        instance,
        status,
        session: session.clone(),
        conn_gen: generation,
    });
}

async fn run(
    cfg: MachineConfig,
    mut ctl: mpsc::Receiver<Control>,
    mut cancel: watch::Receiver<bool>,
    state: watch::Sender<MachineState>,
    events: broadcast::Sender<(u64, MachineEvent)>,
) {
    let mut session = Session::default();
    let mut commands = Commands::default();
    let mut previous_cred: Option<[u8; 32]> = None;
    let mut backoff = Backoff::new(cfg.timing.backoff_base, cfg.timing.backoff_max);
    let mut offline_since = None;
    let mut generation = 0;
    loop {
        if *cancel.borrow() {
            return;
        }
        if retryable_status(&state.borrow().status) {
            publish(
                &state,
                cfg.instance,
                MachineStatus::Connecting,
                &session,
                generation,
            );
        }
        let deadline = Instant::now() + cfg.timing.connect_total;
        let connecting_cred = match &cfg.credential {
            Credential::Token(token) => Some(credential_hash(token)),
            Credential::LocalFile(dir) => local::read_local_token(dir)
                .ok()
                .map(|token| credential_hash(&token)),
        };
        if let Some(cred) = connecting_cred
            && previous_cred.is_some_and(|previous| previous != cred)
        {
            commands.revoke_other_creds(cred);
        }
        let opened = tokio::select! { _ = cancel.changed() => return, result = open(&cfg, deadline) => result };
        let end = match opened {
            Err(e) => e,
            Ok((mut ws, token)) => {
                let cred = credential_hash(&token);
                if previous_cred.is_some_and(|previous| previous != cred) {
                    commands.revoke_other_creds(cred);
                }
                previous_cred = Some(cred);
                let handshake_result = tokio::select! {
                    _ = cancel.changed() => return,
                    result = handshake(&mut ws, &token, &cfg.client, session.resume(), deadline) => result,
                };
                match handshake_result {
                    Err(e) => e,
                    Ok((machine, caps, epoch, wseq)) => {
                        if *cancel.borrow() {
                            return;
                        }
                        generation += 1;
                        let change = session.welcome(machine, caps, &epoch, wseq);
                        let resend = commands.on_welcome(
                            change,
                            &session.capabilities,
                            Instant::now(),
                            &cfg.timing,
                        );
                        let mut send_error = None;
                        for msg in resend {
                            if let Err(error) = send_json(&mut ws, &msg).await {
                                send_error = Some(error);
                                break;
                            }
                        }
                        let _ = events
                            .send((cfg.instance, MachineEvent::Welcomed { generation, change }));
                        offline_since = None;
                        if session.ready() {
                            publish(
                                &state,
                                cfg.instance,
                                MachineStatus::Online,
                                &session,
                                generation,
                            );
                            let _ = events.send((cfg.instance, MachineEvent::Ready { generation }));
                            backoff.reset();
                        } else {
                            publish(
                                &state,
                                cfg.instance,
                                MachineStatus::Syncing,
                                &session,
                                generation,
                            );
                        }
                        let end = if let Some(error) = send_error {
                            error
                        } else {
                            serve(
                                ws,
                                &mut session,
                                generation,
                                &events,
                                cfg.instance,
                                &state,
                                &cfg.timing,
                                &mut ctl,
                                &mut backoff,
                                &mut commands,
                                cred,
                                &mut cancel,
                            )
                            .await
                        };
                        let _ = events
                            .send((cfg.instance, MachineEvent::ConnectionEnded { generation }));
                        end
                    }
                }
            }
        };
        let delay = match end {
            End::Stopped => return,
            End::Offline(error) => {
                let since = *offline_since.get_or_insert_with(now_ms);
                publish(
                    &state,
                    cfg.instance,
                    MachineStatus::Offline {
                        since_ms: since,
                        error,
                    },
                    &session,
                    generation,
                );
                backoff.next_delay()
            }
            End::Resync(reason) => {
                tracing::warn!(reason, "resyncing machine session");
                session.force_resync();
                publish(
                    &state,
                    cfg.instance,
                    MachineStatus::Connecting,
                    &session,
                    generation,
                );
                backoff.next_delay()
            }
            End::TooSlow => {
                publish(
                    &state,
                    cfg.instance,
                    MachineStatus::Connecting,
                    &session,
                    generation,
                );
                Duration::ZERO
            }
            End::Unauthorized => {
                commands.revoke_all();
                let message = if matches!(cfg.credential, Credential::LocalFile(_)) {
                    "acd rejected client.token; re-reading it"
                } else {
                    "The token was rejected. Re-pair this machine."
                };
                publish(
                    &state,
                    cfg.instance,
                    MachineStatus::AuthFailed {
                        message: message.into(),
                    },
                    &session,
                    generation,
                );
                if matches!(cfg.credential, Credential::Token(_)) {
                    if !wait_retry(&mut ctl, &mut cancel).await {
                        return;
                    }
                    continue;
                }
                backoff.next_delay()
            }
            End::PinMismatch => {
                publish(
                    &state,
                    cfg.instance,
                    MachineStatus::IdentityChanged {
                        message: "Server identity changed. Re-pair this machine.".into(),
                    },
                    &session,
                    generation,
                );
                cfg.timing.identity_retry
            }
            End::Incompatible(message) => {
                publish(
                    &state,
                    cfg.instance,
                    MachineStatus::Incompatible { message },
                    &session,
                    generation,
                );
                if !wait_retry(&mut ctl, &mut cancel).await {
                    return;
                }
                continue;
            }
            End::Local(message) => {
                publish(
                    &state,
                    cfg.instance,
                    MachineStatus::LocalUnavailable { message },
                    &session,
                    generation,
                );
                backoff.next_delay()
            }
        };
        match wait_delay(&mut ctl, delay, &mut cancel).await {
            Wait::Retry => continue,
            Wait::Elapsed => {}
            Wait::Stopped => return,
        }
    }
}

async fn wait_retry(ctl: &mut mpsc::Receiver<Control>, cancel: &mut watch::Receiver<bool>) -> bool {
    loop {
        tokio::select! { _ = cancel.changed() => return false, control = ctl.recv() => match control {
            Some(Control::RetryNow) => return true,
            Some(Control::Submit(command, expect_gen, reply)) => {
                let _ = (command.capability(), expect_gen);
                let _ = reply.send(Outcome::NotSent(NotSent::Offline));
            }
            Some(Control::DropConnection) => {}
            None => return false,
        }}
    }
}
enum Wait {
    Elapsed,
    Retry,
    Stopped,
}
async fn wait_delay(
    ctl: &mut mpsc::Receiver<Control>,
    delay: Duration,
    cancel: &mut watch::Receiver<bool>,
) -> Wait {
    let deadline = tokio::time::Instant::now() + delay;
    loop {
        tokio::select! {
            _ = cancel.changed() => return Wait::Stopped,
            _ = tokio::time::sleep_until(deadline) => return Wait::Elapsed,
            control = ctl.recv() => match control {
                Some(Control::RetryNow) => return Wait::Retry,
                Some(Control::Submit(command, expect_gen, reply)) => { let _ = (command.capability(), expect_gen); let _ = reply.send(Outcome::NotSent(NotSent::Offline)); },
                Some(Control::DropConnection) => {},
                None => return Wait::Stopped,
            }
        }
    }
}

pub fn spawn_machine(cfg: MachineConfig) -> MachineHandle {
    let initial = MachineState {
        instance: cfg.instance,
        status: MachineStatus::Connecting,
        session: Session::default(),
        conn_gen: 0,
    };
    let (state_tx, state) = watch::channel(initial);
    let (events, _) = broadcast::channel(256);
    let (ctl, ctl_rx) = mpsc::channel(64);
    let (cancel, cancel_rx) = watch::channel(false);
    let task_events = events.clone();
    let task_state = state_tx.clone();
    tokio::spawn(async move {
        run(
            cfg,
            ctl_rx,
            cancel_rx,
            task_state.clone(),
            task_events.clone(),
        )
        .await;
        drop(task_state);
        drop(task_events);
    });
    MachineHandle {
        state,
        events,
        ctl,
        cancel,
    }
}
