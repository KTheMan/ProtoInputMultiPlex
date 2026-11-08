//! One bounded worker owns the socket. Any interrupted/failed send drops the
//! socket immediately: no Close/terminal write can flush a dirty application sink.
use super::*;
use futures_util::{SinkExt, StreamExt};
use std::{
    collections::BTreeSet,
    future::Future,
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    time::{Instant, timeout, timeout_at},
};
use tokio_tungstenite::{
    WebSocketStream, client_async_with_config,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        http::{HeaderValue, Uri},
        protocol::WebSocketConfig,
    },
};
const PATH: &str = "/application-events/v1";
const HANDSHAKE: Duration = Duration::from_secs(5);
const REQUEST: Duration = Duration::from_secs(10);

pub(super) fn validate_config(config: &ConnectConfig) -> Result<SocketAddr, ClientError> {
    let port_text = config
        .endpoint
        .strip_prefix("ws://127.0.0.1:")
        .and_then(|s| s.strip_suffix(PATH))
        .ok_or(ClientError::Invalid)?;
    let port: u16 = port_text.parse().map_err(|_| ClientError::Invalid)?;
    if port == 0
        || port.to_string() != port_text
        || config.exact_origin.len() > 512
        || config.event_credential.len() != 64
        || !config
            .event_credential
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
    {
        return Err(ClientError::Invalid);
    }
    let origin: Uri = config
        .exact_origin
        .parse()
        .map_err(|_| ClientError::Invalid)?;
    let scheme = origin.scheme_str().ok_or(ClientError::Invalid)?;
    let authority = origin.authority().ok_or(ClientError::Invalid)?.as_str();
    if !matches!(scheme, "http" | "https")
        || authority.is_empty()
        || authority.contains(['@', '*', '%', '\\'])
        || config.exact_origin != format!("{scheme}://{authority}")
    {
        return Err(ClientError::Invalid);
    }
    HeaderValue::from_str(&config.exact_origin).map_err(|_| ClientError::Invalid)?;
    Ok(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
}

/// Cancellation wraps the WHOLE I/O future. Callers must return and drop socket
/// on every Err, including stop/timeout; never resume a possibly dirty sink.
async fn fenced<T>(
    shared: &Shared,
    limit: Duration,
    io: impl Future<Output = T>,
) -> Result<T, StopReason> {
    if shared.stop.load(Ordering::Acquire) {
        return Err(StopReason::LocalStop);
    }
    if limit.is_zero() {
        return Err(StopReason::DeadlineUncertain);
    }
    tokio::select! { biased;
        _ = shared.changed.notified() => Err(StopReason::LocalStop),
        result = timeout(limit, io) => result.map_err(|_| StopReason::DeadlineUncertain),
    }
}
pub(super) async fn send<S: AsyncRead + AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
    shared: &Shared,
    text: String,
) -> Result<(), StopReason> {
    fenced(shared, REQUEST, socket.send(Message::Text(text.into())))
        .await?
        .map_err(|_| StopReason::Transport)
}
fn parse(message: Message) -> Result<Option<(ServerMessage, usize)>, StopReason> {
    match message {
        Message::Text(text) if text.len() <= MAX_RESPONSE_BYTES => {
            let bytes = text.len();
            let message = serde_json::from_str(text.as_str()).map_err(|_| StopReason::Protocol)?;
            Ok(Some((message, bytes)))
        }
        Message::Ping(_) | Message::Pong(_) => Ok(None),
        Message::Close(_) => Err(StopReason::RemoteEnd),
        _ => Err(StopReason::Protocol),
    }
}
async fn receive<S: AsyncRead + AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
    shared: &Shared,
    until: Option<Instant>,
) -> Result<(ServerMessage, usize), StopReason> {
    loop {
        if shared.stop.load(Ordering::Acquire) {
            return Err(StopReason::LocalStop);
        }
        if until.is_some_and(|deadline| deadline <= Instant::now()) {
            return Err(StopReason::DeadlineUncertain);
        }
        let next = async {
            match until {
                Some(deadline) => timeout_at(deadline, socket.next())
                    .await
                    .map_err(|_| StopReason::DeadlineUncertain),
                None => Ok(socket.next().await),
            }
        };
        let frame = tokio::select! { biased;
            _ = shared.changed.notified() => return Err(StopReason::LocalStop),
            frame = next => frame?,
        }
        .ok_or(StopReason::RemoteEnd)?
        .map_err(|_| StopReason::Transport)?;
        if let Some(message) = parse(frame)? {
            return Ok(message);
        }
    }
}

pub(super) async fn run(config: ConnectConfig, address: SocketAddr, shared: Arc<Shared>) {
    let result = session(config, address, &shared).await;
    // session's socket is already dropped here. This wake contains no terminal
    // WebSocket write, and local retirement makes no remote-cleanup assertion.
    shared.finish(result.err().unwrap_or(StopReason::RemoteEnd));
}
async fn session(
    mut config: ConnectConfig,
    address: SocketAddr,
    shared: &Shared,
) -> Result<(), StopReason> {
    let handshake = Instant::now() + HANDSHAKE;
    let stream = fenced(
        shared,
        handshake.saturating_duration_since(Instant::now()),
        TcpStream::connect(address),
    )
    .await?
    .map_err(|_| StopReason::Transport)?;
    let mut request = config
        .endpoint
        .as_str()
        .into_client_request()
        .map_err(|_| StopReason::Protocol)?;
    request.headers_mut().insert(
        "Origin",
        HeaderValue::from_str(&config.exact_origin).map_err(|_| StopReason::Protocol)?,
    );
    let socket_config = WebSocketConfig::default()
        .max_message_size(Some(MAX_RESPONSE_BYTES))
        .max_frame_size(Some(MAX_RESPONSE_BYTES));
    let (mut socket, _) = fenced(
        shared,
        handshake.saturating_duration_since(Instant::now()),
        client_async_with_config(request, stream, Some(socket_config)),
    )
    .await?
    .map_err(|_| StopReason::Transport)?;
    let hello = EventHello {
        version: PROTOCOL_VERSION,
        credential: std::mem::take(&mut config.event_credential),
    };
    let hello_text = serde_json::to_string(&hello).map_err(|_| StopReason::Protocol)?;
    fenced(
        shared,
        handshake.saturating_duration_since(Instant::now()),
        socket.send(Message::Text(hello_text.into())),
    )
    .await?
    .map_err(|_| StopReason::Transport)?;
    drop(hello);
    drop(config);
    drive(socket, shared, handshake).await
}
pub(super) async fn drive<S: AsyncRead + AsyncWrite + Unpin>(
    mut socket: WebSocketStream<S>,
    shared: &Shared,
    handshake: Instant,
) -> Result<(), StopReason> {
    let (message, bytes) = receive(&mut socket, shared, Some(handshake)).await?;
    if !matches!(&message, ServerMessage::Ready { ready } if ready.version == PROTOCOL_VERSION && ready.epoch != 0)
    {
        return Err(StopReason::Protocol);
    }
    if !shared.enqueue(message, bytes) {
        return Err(StopReason::QueueOverflow);
    }
    let mut expected_id = 1_u64;
    let mut urgent_inflight = BTreeSet::new();
    let mut subscription = None;
    loop {
        if shared.stop.load(Ordering::Acquire) {
            return Err(StopReason::LocalStop);
        }
        // No receive future that has begun a send is ever cancelled and reused.
        // Tungstenite next() only reads/flushes control frames after successful send.
        let next = tokio::select! { biased;
            _ = shared.changed.notified() => return Err(StopReason::LocalStop),
            request = next_request(shared) => request?,
            frame = socket.next() => {
                let frame = frame.ok_or(StopReason::RemoteEnd)?.map_err(|_| StopReason::Transport)?;
                if let Some((message, bytes)) = parse(frame)? {
                    if let ServerMessage::OwnerResponse { request_id, reply } = &message
                        && urgent_inflight.remove(request_id)
                    {
                        if !matches!(reply, Reply::Ok { version, response: OwnerResponse::Acknowledged } if *version == PROTOCOL_VERSION)
                            && !matches!(reply, Reply::Error { version, .. } if *version == PROTOCOL_VERSION)
                        {
                            return Err(StopReason::Protocol);
                        }
                        if !shared.enqueue(message, bytes) { return Err(StopReason::QueueOverflow); }
                        continue;
                    }
                    if !valid_push(&message, subscription) { return Err(StopReason::Protocol); }
                    let terminal = is_terminal(&message);
                    if !shared.enqueue(message, bytes) { return Err(StopReason::QueueOverflow); }
                    if terminal { return Err(StopReason::RemoteEnd); }
                }
                continue;
            }
        };
        let id = next.request_id();
        if id & (1_u64 << 63) == 0 {
            if id != expected_id || expected_id > 65_536 {
                return Err(StopReason::Protocol);
            }
            expected_id += 1;
        }
        let text = match &next {
            Outgoing::Client(value) => serde_json::to_string(value),
            Outgoing::Owner(value) => serde_json::to_string(value),
        }
        .map_err(|_| StopReason::Protocol)?;
        if text.len() > MAX_REQUEST_BYTES {
            return Err(StopReason::Protocol);
        }
        send(&mut socket, shared, text).await?;
        let until = Instant::now() + REQUEST;
        loop {
            let (message, bytes) = tokio::select! { biased;
                _ = shared.requests_changed.notified() => {
                    if let Some(frame) = take_urgent(shared)? {
                        let text = serde_json::to_string(&frame).map_err(|_| StopReason::Protocol)?;
                        if text.len() > MAX_REQUEST_BYTES { return Err(StopReason::Protocol); }
                        send(&mut socket, shared, text).await?;
                        urgent_inflight.insert(frame.request_id);
                    }
                    continue;
                }
                received = receive(&mut socket, shared, Some(until)) => received?,
            };
            match &message {
                ServerMessage::Response { request_id, reply }
                    if *request_id == id && matches!(&next, Outgoing::Client(_)) =>
                {
                    let Outgoing::Client(value) = &next else {
                        unreachable!()
                    };
                    match reply {
                        Reply::Ok { version, response }
                            if *version == PROTOCOL_VERSION
                                && response_matches(&value.request.command, response) =>
                        {
                            if let ClientResponse::Subscribed {
                                subscription: token,
                                ..
                            } = response
                            {
                                subscription = Some(*token);
                            }
                        }
                        Reply::Error { version, .. } if *version == PROTOCOL_VERSION => {}
                        _ => return Err(StopReason::Protocol),
                    }
                    if !shared.enqueue(message, bytes) {
                        return Err(StopReason::QueueOverflow);
                    }
                    break;
                }
                ServerMessage::OwnerResponse { request_id, reply }
                    if *request_id == id && matches!(&next, Outgoing::Owner(_)) =>
                {
                    let Outgoing::Owner(value) = &next else {
                        unreachable!()
                    };
                    if !owner_response_matches(&value.operation, reply) {
                        return Err(StopReason::Protocol);
                    }
                    if !shared.enqueue(message, bytes) {
                        return Err(StopReason::QueueOverflow);
                    }
                    break;
                }
                ServerMessage::OwnerResponse { request_id, reply }
                    if urgent_inflight.remove(request_id) =>
                {
                    if !matches!(reply, Reply::Ok { version, response: OwnerResponse::Acknowledged } if *version == PROTOCOL_VERSION)
                        && !matches!(reply, Reply::Error { version, .. } if *version == PROTOCOL_VERSION)
                    {
                        return Err(StopReason::Protocol);
                    }
                    if !shared.enqueue(message, bytes) {
                        return Err(StopReason::QueueOverflow);
                    }
                }
                message if valid_push(message, subscription) => {
                    let terminal = is_terminal(message);
                    if !shared.enqueue(message.clone(), bytes) {
                        return Err(StopReason::QueueOverflow);
                    }
                    if terminal {
                        return Err(StopReason::RemoteEnd);
                    }
                }
                _ => return Err(StopReason::Protocol),
            }
        }
    }
}
fn valid_push(message: &ServerMessage, subscription: Option<SubscriptionId>) -> bool {
    matches!(message, ServerMessage::EventStream { subscription: token, .. } if subscription == Some(*token))
        || matches!(message, ServerMessage::WindowApply { .. })
        || matches!(message, ServerMessage::Disconnected { .. })
}
fn owner_response_matches(operation: &OwnerOperation, reply: &Reply<OwnerResponse>) -> bool {
    match reply {
        Reply::Error { version, .. } => *version == PROTOCOL_VERSION,
        Reply::Ok { version, response } if *version == PROTOCOL_VERSION => matches!(
            (operation, response),
            (
                OwnerOperation::Register { .. },
                OwnerResponse::Registered { .. }
            ) | (OwnerOperation::Refresh { .. }, OwnerResponse::Window { .. })
                | (
                    OwnerOperation::Retire { .. } | OwnerOperation::ApplyResult { .. },
                    OwnerResponse::Acknowledged
                )
        ),
        _ => false,
    }
}
fn is_terminal(message: &ServerMessage) -> bool {
    matches!(
        message,
        ServerMessage::Disconnected { .. }
            | ServerMessage::EventStream {
                stream: Poll::Ended { .. },
                ..
            }
    )
}
fn take_urgent(shared: &Shared) -> Result<Option<OwnerFrame>, StopReason> {
    let mut q = shared
        .mailbox
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if shared.stop.load(Ordering::Acquire) {
        return Err(StopReason::LocalStop);
    }
    Ok(q.urgent.pop_front())
}
async fn next_request(shared: &Shared) -> Result<Outgoing, StopReason> {
    loop {
        // Register before checking the queue. Cancellation by the idle socket
        // select cannot pop a request unless this future is itself Ready.
        let changed = shared.requests_changed.notified();
        {
            let mut q = shared
                .mailbox
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if shared.stop.load(Ordering::Acquire) {
                return Err(StopReason::LocalStop);
            }
            if let Some(request) = q.urgent.pop_front() {
                return Ok(Outgoing::Owner(request));
            }
            if let Some(request) = q.outgoing.pop_front() {
                return Ok(request);
            }
        }
        changed.await;
    }
}
