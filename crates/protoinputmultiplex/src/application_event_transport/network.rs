use super::{
    authority::*,
    bridge::{APPLYING, Admission, COMPLETED, NetworkEndpoint, UiRequest},
};
use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use mux_application_sdk::*;
use serde::Serialize;
use std::{
    collections::{BTreeMap, VecDeque},
    io,
    net::{Ipv4Addr, TcpListener as StdListener},
    path::Path,
    pin::Pin,
    sync::{Arc, atomic::Ordering},
    task::{Context as TaskContext, Poll as TaskPoll},
    thread::{self, JoinHandle},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    task::JoinSet,
    time::{sleep, timeout},
};
use tokio_tungstenite::{
    WebSocketStream, accept_hdr_async_with_config,
    tungstenite::{
        Message,
        handshake::server::{Request, Response},
        protocol::{Role, WebSocketConfig},
    },
};

const UPGRADE_LIMIT: usize = 16 * 1024;
const HELLO_LIMIT: usize = 4096;
const MAX_REQUESTS: u64 = 65_536;
const ENDPOINT_PATH: &str = "/application-events/v1";
type Socket = WebSocketStream<UpgradeStream>;

#[derive(Serialize)]
struct Descriptor<'a> {
    protoinputmultiplex_application_events: u32,
    endpoint: String,
    exact_origin: &'a str,
    credential: &'a str,
    expires_unix_ms: u64,
}

/// Retain in a background lifecycle actor. start/issue/finish_stop do file/network
/// work and must never run on the UI thread; request_stop/snapshot are cheap.
pub(crate) struct Service {
    shared: Arc<Shared>,
    port: u16,
    origins: Vec<String>,
    worker: Option<JoinHandle<Result<()>>>,
}
impl Service {
    pub fn start(config: Config, endpoint: NetworkEndpoint) -> Result<Self> {
        validate_origins(&config.allowed_origins)?;
        ensure!(
            !endpoint.shared.stop.load(Ordering::Acquire),
            "event bridge stopped before startup"
        );
        ensure!(
            endpoint
                .shared
                .running
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            "event service already started"
        );
        let shared = Arc::clone(&endpoint.shared);
        let preparation = (|| {
            let listener = StdListener::bind((Ipv4Addr::LOCALHOST, 0))?;
            listener.set_nonblocking(true)?;
            let port = listener.local_addr()?.port();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            Ok::<_, anyhow::Error>((listener, port, runtime))
        })();
        let (listener, port, runtime) = match preparation {
            Ok(value) => value,
            Err(error) => {
                shared.request_stop();
                shared.running.store(false, Ordering::Release);
                return Err(error);
            }
        };
        let origins = config.allowed_origins.clone();
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("application-events-loopback".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    runtime.block_on(async move {
                        let listener = TcpListener::from_std(listener)?;
                        listen(listener, config, endpoint, port).await
                    })
                }));
                let result = match result {
                    Ok(result) => result,
                    Err(_) => Err(anyhow::anyhow!(
                        "event worker panicked; retirement is unconfirmed"
                    )),
                };
                if result.is_err() {
                    worker_shared.request_stop();
                }
                worker_shared.running.store(false, Ordering::Release);
                if result.is_ok() {
                    worker_shared.retired.store(true, Ordering::Release);
                }
                result
            });
        match worker {
            Ok(worker) => Ok(Self {
                shared,
                port,
                origins,
                worker: Some(worker),
            }),
            Err(error) => {
                shared.request_stop();
                shared.running.store(false, Ordering::Release);
                Err(error.into())
            }
        }
    }

    /// Explicit background issuance after host UI approval. Descriptor ACL/security
    /// uses the existing current-logon private descriptor implementation.
    pub fn issue_client(
        &mut self,
        permit: &ClientPermit,
        descriptor_path: &Path,
    ) -> Result<IssuedClient> {
        ensure!(
            Arc::ptr_eq(&self.shared, &permit.shared),
            "permit belongs to another event service"
        );
        let lease = &permit.lease;
        ensure!(
            self.shared.running.load(Ordering::Acquire) && lease.live(&self.shared, Instant::now()),
            "event permit is no longer live"
        );
        ensure!(
            self.origins.contains(&lease.origin),
            "event origin was not enabled for this service"
        );
        ensure!(!descriptor_path.exists(), "event descriptor already exists");
        {
            // Pin the registered slot against concurrent retirement before starting
            // descriptor IO; cleanup never removes a slot while issuance is active.
            let leases = self
                .shared
                .leases
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ensure!(
                leases
                    .get(&lease.id)
                    .is_some_and(|registered| Arc::ptr_eq(registered, lease))
                    && lease.live(&self.shared, Instant::now()),
                "event permit retired before issuance"
            );
            ensure!(
                lease
                    .issued
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok(),
                "event permit already issued"
            );
            lease.issuing.store(true, Ordering::Release);
        }
        struct IssueGuard<'a> {
            lease: &'a Lease,
            shared: &'a Shared,
            committed: bool,
        }
        impl Drop for IssueGuard<'_> {
            fn drop(&mut self) {
                if !self.committed {
                    self.lease.revoke();
                }
                self.lease.issuing.store(false, Ordering::Release);
                self.shared.changed.notify_waiters();
            }
        }
        let mut guard = IssueGuard {
            lease,
            shared: &self.shared,
            committed: false,
        };
        let result = (|| {
            let credential = crate::gui_bridge::random_token()?;
            ensure!(
                self.shared.leases().iter().all(|other| other
                    .credential
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_deref()
                    != Some(credential.as_str())),
                "event credential collision"
            );
            *lease
                .credential
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(credential);
            *lease
                .descriptor
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some(descriptor_path.to_path_buf());
            let private = self.descriptor(lease)?;
            crate::gui_bridge::write_private_descriptor(descriptor_path, private.as_bytes())?;
            ensure!(
                lease.live(&self.shared, Instant::now()),
                "event permit retired during issuance"
            );
            Ok(IssuedClient {
                descriptor_path: descriptor_path.to_path_buf(),
                expires_in: lease.deadline.saturating_duration_since(Instant::now()),
            })
        })();
        guard.committed = result.is_ok();
        if result.is_err() {
            lease.revoke();
        }
        result
    }

    fn descriptor(&self, lease: &Lease) -> Result<String> {
        let credential = lease
            .credential
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let credential = credential
            .as_deref()
            .context("event credential unavailable")?;
        let remaining = lease.deadline.saturating_duration_since(Instant::now());
        let expires = SystemTime::now()
            .checked_add(remaining)
            .context("event expiry overflow")?
            .duration_since(UNIX_EPOCH)?
            .as_millis();
        Ok(serde_json::to_string(&Descriptor {
            protoinputmultiplex_application_events: PROTOCOL_VERSION,
            endpoint: format!("ws://127.0.0.1:{}{ENDPOINT_PATH}", self.port),
            exact_origin: &lease.origin,
            credential,
            expires_unix_ms: u64::try_from(expires)?,
        })?)
    }

    /// Only an explicit local Copy action should call this. Never put its result in
    /// Snapshot/status/logs, a URL/query, browser storage, ordinary routing or
    /// stopped-Setup MCP, or automatic telemetry. The separate window-only MCP
    /// mode may read the private descriptor file after exact GUI approval.
    pub fn private_connection_for_copy(&self, permit: &ClientPermit) -> Option<String> {
        if !Arc::ptr_eq(&self.shared, &permit.shared)
            || !permit.lease.live(&self.shared, Instant::now())
        {
            return None;
        }
        self.descriptor(&permit.lease).ok()
    }
    pub fn request_stop(&self) {
        self.shared.request_stop();
    }
    pub fn snapshot(&self) -> ServiceSnapshot {
        let leases = self.shared.leases();
        let now = Instant::now();
        ServiceSnapshot {
            running: self.shared.running.load(Ordering::Acquire),
            stopping: self.shared.stop.load(Ordering::Acquire),
            retired: self.shared.retired.load(Ordering::Acquire),
            port: self.port,
            approved_clients: leases
                .iter()
                .filter(|lease| lease.live(&self.shared, now))
                .count(),
            connected_clients: leases
                .iter()
                .filter(|lease| {
                    lease.live(&self.shared, now)
                        && lease.connected.load(Ordering::Acquire)
                        && !lease.network_done.load(Ordering::Acquire)
                })
                .count(),
            cleanup_pending: leases
                .iter()
                .filter(|lease| {
                    !lease.live(&self.shared, now) && !lease.cleanup_done.load(Ordering::Acquire)
                })
                .count(),
            uncertain_admissions: self.shared.uncertain.load(Ordering::Acquire),
            queue_overflows: self.shared.overflows.load(Ordering::Acquire),
        }
    }
    /// Background-only join. UI must continue tick/drain or call bridge.stop first.
    /// No timeout claims that an applying command or its cleanup was cancelled.
    pub fn finish_stop(&mut self) -> Result<()> {
        self.request_stop();
        if let Some(worker) = self.worker.take() {
            match worker.join() {
                Ok(result) => result?,
                Err(_) => bail!("event worker failed; retirement is unconfirmed"),
            }
        }
        ensure!(
            self.shared.retired.load(Ordering::Acquire),
            "event retirement is unconfirmed"
        );
        Ok(())
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.request_stop();
    }
}

async fn listen(
    listener: TcpListener,
    config: Config,
    endpoint: NetworkEndpoint,
    port: u16,
) -> Result<()> {
    let mut sockets = JoinSet::new();
    let mut failure = None;
    while !endpoint.shared.stop.load(Ordering::Acquire) {
        if endpoint.requests.is_closed() {
            endpoint.shared.request_stop();
            break;
        }
        if let Err(error) = retire_descriptors(&endpoint.shared) {
            failure = Some(error);
            break;
        }
        tokio::select! {
            _ = sleep(NETWORK_POLL) => {},
            _ = endpoint.shared.changed.notified() => {},
            Some(result) = sockets.join_next(), if !sockets.is_empty() => {
                if result.is_err() { failure = Some(anyhow::anyhow!("event connection task failed")); break; }
            },
            accepted = listener.accept() => match accepted {
                Ok((stream, address)) if address.ip() == std::net::IpAddr::V4(Ipv4Addr::LOCALHOST) && sockets.len() < MAX_CLIENTS => {
                    let shared = Arc::clone(&endpoint.shared); let requests = endpoint.requests.clone(); let origins = config.allowed_origins.clone();
                    sockets.spawn(async move { let _ = connection(stream, shared, requests, origins, port).await; });
                }
                Ok((stream, _)) => drop(stream),
                Err(error) => { failure = Some(error.into()); break; }
            }
        }
    }
    endpoint.shared.request_stop();
    drop(listener);
    while let Some(result) = sockets.join_next().await {
        if result.is_err() {
            failure = Some(anyhow::anyhow!("event connection cleanup failed"));
        }
    }
    // A closed socket is not proof of host cleanup. The UI owner remains the only
    // place that can release !Send native/virtual state and settle this fence.
    loop {
        retire_descriptors(&endpoint.shared)?;
        if endpoint.shared.leases().is_empty() {
            break;
        }
        sleep(NETWORK_POLL).await;
    }
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(())
}

pub(super) fn retire_descriptors(shared: &Shared) -> Result<()> {
    for lease in shared.leases() {
        if lease.live(shared, Instant::now()) {
            continue;
        }
        lease.revoke();
        if lease.issuing.load(Ordering::Acquire) {
            continue;
        }
        *lease
            .credential
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        let path = lease
            .descriptor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(path) = path {
            crate::gui_bridge::remove_descriptor_file(&path)?;
            *lease
                .descriptor
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }
    }
    shared
        .leases
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|_, lease| {
            !(lease.cleanup_done.load(Ordering::Acquire)
                && lease.network_done.load(Ordering::Acquire)
                && !lease.issuing.load(Ordering::Acquire)
                && lease
                    .descriptor
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_none()
                && lease
                    .credential
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_none())
        });
    Ok(())
}

pub(super) fn origin_allowed(request: &Request, origins: &[String], port: u16) -> Option<String> {
    let mut values = request.headers().get_all("origin").iter();
    let (Some(origin), None) = (values.next(), values.next()) else {
        return None;
    };
    let origin = origin.to_str().ok()?;
    if !origins.iter().any(|allowed| allowed == origin)
        || request.uri().path() != ENDPOINT_PATH
        || request.uri().query().is_some()
        || request.uri().scheme().is_some()
        || request.uri().authority().is_some()
        || request.headers().contains_key("authorization")
    {
        return None;
    }
    let mut hosts = request.headers().get_all("host").iter();
    let (Some(host), None) = (hosts.next(), hosts.next()) else {
        return None;
    };
    if host.to_str().ok()? != format!("127.0.0.1:{port}") {
        return None;
    }
    Some(origin.to_owned())
}

pub(super) fn authenticate_hello(
    shared: &Arc<Shared>,
    origin: &str,
    hello: &EventHello,
    now: Instant,
) -> Result<Arc<Lease>> {
    ensure!(
        hello.version == PROTOCOL_VERSION
            && hello.credential.len() == 64
            && hello
                .credential
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit()),
        "event authentication denied"
    );
    for lease in shared.leases() {
        if lease.origin != origin
            || !lease.live(shared, now)
            || !lease.issued.load(Ordering::Acquire)
        {
            continue;
        }
        let matches = lease
            .credential
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_deref()
            .is_some_and(|expected| token_equal(&hello.credential, expected));
        if matches
            && lease
                .connected
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            lease.network_done.store(false, Ordering::Release);
            return Ok(lease);
        }
    }
    bail!("event authentication denied")
}

pub(super) struct ConnectionGuard {
    pub(super) lease: Arc<Lease>,
}
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.lease.revoke();
        self.lease.network_done.store(true, Ordering::Release);
    }
}
struct Established {
    socket: Socket,
    lease: Arc<Lease>,
    events: mpsc::Receiver<ServerMessage>,
    _guard: ConnectionGuard,
}

async fn establish(
    stream: TcpStream,
    shared: Arc<Shared>,
    requests: mpsc::Sender<UiRequest>,
    origins: Vec<String>,
    port: u16,
) -> Result<Established> {
    let mut approved_origin = None;
    let stream = UpgradeStream::prefetch(stream).await?;
    #[allow(clippy::result_large_err)]
    let callback = |request: &Request, response: Response| {
        if !shared.stop.load(Ordering::Acquire)
            && let Some(origin) = origin_allowed(request, &origins, port)
        {
            approved_origin = Some(origin);
            Ok(response)
        } else {
            Err(tokio_tungstenite::tungstenite::http::Response::builder()
                .status(403)
                .body(Some("Event origin denied".into()))
                .expect("constant response"))
        }
    };
    let mut socket = accept_hdr_async_with_config(
        stream,
        callback,
        Some(
            WebSocketConfig::default()
                .max_message_size(Some(HELLO_LIMIT))
                .max_frame_size(Some(HELLO_LIMIT))
                .write_buffer_size(0)
                .max_write_buffer_size(MAX_RESPONSE_BYTES + 1024),
        ),
    )
    .await?;
    socket.get_mut().upgrade_end = None;
    socket.get_mut().read_chunk = 1;
    let hello = socket.next().await.context("event hello required")??;
    let Message::Text(hello) = hello else {
        bail!("event hello must be text");
    };
    let hello: EventHello =
        serde_json::from_slice(hello.as_bytes()).context("invalid event hello")?;
    let lease = authenticate_hello(
        &shared,
        approved_origin.as_deref().context("event origin missing")?,
        &hello,
        Instant::now(),
    )?;
    let guard = ConnectionGuard {
        lease: Arc::clone(&lease),
    };
    let (output, events) = mpsc::channel(OUT_CAPACITY);
    let (ready, receive) = oneshot::channel();
    ensure!(lease.live(&shared, Instant::now()), "event permit expired");
    requests
        .try_send(UiRequest::Connect {
            lease: Arc::clone(&lease),
            output,
            ready,
        })
        .map_err(|_| anyhow::anyhow!("event host busy"))?;
    let ready = receive
        .await
        .context("event host closed")?
        .map_err(|_| anyhow::anyhow!("event host denied"))?;
    let mut stream = socket.into_inner();
    stream.read_chunk = usize::MAX;
    let mut socket = WebSocketStream::from_raw_socket(
        stream,
        Role::Server,
        Some(
            WebSocketConfig::default()
                .max_message_size(Some(MAX_REQUEST_BYTES))
                .max_frame_size(Some(MAX_REQUEST_BYTES))
                .write_buffer_size(0)
                .max_write_buffer_size(MAX_RESPONSE_BYTES + 1024),
        ),
    )
    .await;
    send(&mut socket, &shared, &lease, ServerMessage::Ready { ready }).await?;
    Ok(Established {
        socket,
        lease,
        events,
        _guard: guard,
    })
}

struct Pending {
    id: u64,
    deadline: Instant,
    admission: Arc<Admission>,
}
fn retire_pending(pending: &VecDeque<Pending>, shared: &Shared) {
    for pending in pending {
        if !pending.admission.cancel_queued()
            && matches!(
                pending.admission.state.load(Ordering::Acquire),
                APPLYING | COMPLETED
            )
        {
            pending.admission.uncertain(shared);
        }
    }
}

async fn connection(
    stream: TcpStream,
    shared: Arc<Shared>,
    requests: mpsc::Sender<UiRequest>,
    origins: Vec<String>,
    port: u16,
) -> Result<()> {
    let Established {
        socket,
        lease,
        events,
        _guard,
    } = timeout(
        AUTH_TIMEOUT,
        establish(stream, Arc::clone(&shared), requests.clone(), origins, port),
    )
    .await??;
    let result = drive(
        socket,
        Arc::clone(&shared),
        requests,
        Arc::clone(&lease),
        events,
    )
    .await;
    drop(_guard);
    result
}

/// The actual post-auth connection driver, shared by TCP production and framed
/// duplex connection-path tests. There is no retry or concurrent Host application.
pub(super) async fn drive<S: AsyncRead + AsyncWrite + Unpin>(
    mut socket: WebSocketStream<S>,
    shared: Arc<Shared>,
    requests: mpsc::Sender<UiRequest>,
    lease: Arc<Lease>,
    mut events: mpsc::Receiver<ServerMessage>,
) -> Result<()> {
    let mut pending: VecDeque<Pending> = VecDeque::new();
    let mut urgent: BTreeMap<u64, Pending> = BTreeMap::new();
    let mut last_id = 0;
    let mut count = 0_u64;
    let result = loop {
        if !lease.live(&shared, Instant::now()) {
            break Ok(());
        }
        if pending
            .front()
            .is_some_and(|pending| Instant::now() >= pending.deadline)
            || urgent.values().any(|pending| Instant::now() >= pending.deadline)
        {
            break Err(anyhow::anyhow!(
                "event request deadline reached; do not retry an uncertain request"
            ));
        }
        tokio::select! {
            biased;
            _ = lease.changed.notified() => {},
            _ = shared.changed.notified() => {},
            _ = sleep(NETWORK_POLL) => {},
            frame = events.recv() => {
                let Some(frame) = frame else { break Ok(()); };
                let response_id = match &frame {
                    ServerMessage::Response { request_id, .. } | ServerMessage::OwnerResponse { request_id, .. } => Some(*request_id),
                    _ => None,
                };
                if let Some(request_id) = response_id {
                    let matched = if request_id & (1_u64 << 63) != 0 {
                        urgent.contains_key(&request_id)
                    } else {
                        pending.front().map(|pending| pending.id) == Some(request_id)
                    };
                    if !matched { break Err(anyhow::anyhow!("event reply ownership mismatch")); }
                }
                if let Err(error) = send(&mut socket, &shared, &lease, frame).await { break Err(error); }
                if let Some(request_id) = response_id {
                    if request_id & (1_u64 << 63) != 0 { urgent.remove(&request_id); } else { pending.pop_front(); }
                }
            }
            incoming = socket.next() => {
                let Some(incoming) = incoming else { break Ok(()); };
                let frame = match incoming { Ok(frame) => frame, Err(error) => break Err(error.into()) };
                match frame {
                    Message::Text(text) => {
                        if pending.len() >= MAX_PENDING || urgent.len() >= MAX_PENDING {
                            shared.overflows.fetch_add(1, Ordering::Relaxed);
                            break Err(anyhow::anyhow!("event pending FIFO overflow; receiver fenced and accepted requests remain uncertain"));
                        }
                        if text.len() > MAX_REQUEST_BYTES || count >= MAX_REQUESTS { break Err(anyhow::anyhow!("event request admission denied")); }
                        let client_message: Option<ClientMessage> = serde_json::from_slice(text.as_bytes()).ok();
                        let owner_frame: Option<OwnerFrame> = if client_message.is_none() { serde_json::from_slice(text.as_bytes()).ok() } else { None };
                        let (request_id, owner_urgent) = if let Some(message) = &client_message {
                            (message.request_id, false)
                        } else if let Some(frame) = &owner_frame {
                            (frame.request_id, matches!(&frame.operation, OwnerOperation::ApplyResult { .. }))
                        } else { break Err(anyhow::anyhow!("invalid event client frame")); };
                        if request_id == 0 || (!owner_urgent && (request_id & (1_u64 << 63) != 0 || request_id <= last_id))
                            || (owner_urgent && (request_id & (1_u64 << 63) == 0 || urgent.contains_key(&request_id)))
                        { break Err(anyhow::anyhow!("event request IDs must be unique within their channel")); }
                        if !owner_urgent { last_id = request_id; }
                        count += 1;
                        let admission = Arc::new(Admission::new()); let deadline = Instant::now() + REQUEST_TIMEOUT;
                        if !lease.live(&shared, Instant::now()) { break Err(anyhow::anyhow!("event permit retired before admission")); }
                        let request = if let Some(message) = client_message {
                            UiRequest::Command { lease: Arc::clone(&lease), message, deadline, admission: Arc::clone(&admission) }
                        } else if let Some(frame) = owner_frame {
                            UiRequest::Owner { lease: Arc::clone(&lease), frame, deadline, admission: Arc::clone(&admission) }
                        } else { unreachable!() };
                        match requests.try_send(request) {
                            Ok(()) => {
                                let value = Pending { id: request_id, deadline, admission };
                                if owner_urgent { urgent.insert(request_id, value); } else { pending.push_back(value); }
                            }
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                shared.overflows.fetch_add(1, Ordering::Relaxed);
                                break Err(anyhow::anyhow!("event UI request FIFO overflow; current request not admitted and receiver fenced"));
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => break Ok(()),
                        }
                    }
                    Message::Close(_) => break Ok(()),
                    Message::Ping(bytes) => {
                        match timeout(REQUEST_TIMEOUT, socket.send(Message::Pong(bytes))).await {
                            Ok(Ok(())) => {}, Ok(Err(error)) => break Err(error.into()), Err(error) => break Err(error.into()),
                        }
                    }
                    Message::Pong(_) => {},
                    _ => break Err(anyhow::anyhow!("event frames must be text")),
                }
            }
        }
    };
    lease.revoke();
    retire_pending(&pending, &shared);
    for pending in urgent.values() {
        if !pending.admission.cancel_queued()
            && matches!(pending.admission.state.load(Ordering::Acquire), APPLYING | COMPLETED)
        {
            pending.admission.uncertain(&shared);
        }
    }
    // A cancelled/failed SinkExt::send may leave application bytes buffered in
    // tungstenite. Never write Close, terminal, Pong or any other frame here: the
    // socket must be dropped without another flush on every exit path.
    result
}

pub(super) async fn send<S: AsyncRead + AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
    shared: &Shared,
    lease: &Lease,
    frame: ServerMessage,
) -> Result<()> {
    ensure!(
        lease.live(shared, Instant::now()),
        "event receiver revoked before push"
    );
    let text = encode(&frame)?;
    ensure!(
        lease.live(shared, Instant::now()),
        "event receiver revoked before push"
    );
    let transmit = timeout(REQUEST_TIMEOUT, socket.send(Message::Text(text.into())));
    tokio::pin!(transmit);
    loop {
        tokio::select! {
            biased;
            _ = lease.changed.notified() => {
                ensure!(lease.live(shared, Instant::now()), "event receiver fenced during push");
            },
            _ = shared.changed.notified() => {
                ensure!(lease.live(shared, Instant::now()), "event service fenced during push");
            },
            _ = sleep(NETWORK_POLL) => {
                ensure!(lease.live(shared, Instant::now()), "event receiver expired during push");
            },
            result = &mut transmit => { result??; break; }
        }
    }
    ensure!(
        lease.live(shared, Instant::now()),
        "event receiver retired during push"
    );
    Ok(())
}

pub(super) fn encode(value: &impl Serialize) -> Result<String> {
    struct Bounded(Vec<u8>);
    impl io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > MAX_RESPONSE_BYTES.saturating_sub(self.0.len()) {
                return Err(io::Error::other("event frame exceeds bound"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut output = Bounded(Vec::new());
    serde_json::to_writer(&mut output, value)?;
    Ok(String::from_utf8(output.0)?)
}

struct UpgradeStream {
    stream: TcpStream,
    prefetched: Vec<u8>,
    consumed: usize,
    upgrade_end: Option<usize>,
    read_chunk: usize,
}

impl UpgradeStream {
    async fn prefetch(mut stream: TcpStream) -> Result<Self> {
        let mut prefetched = Vec::with_capacity(UPGRADE_LIMIT + 1);
        let mut chunk = [0u8; 4096];
        loop {
            let available = (UPGRADE_LIMIT + 1 - prefetched.len()).min(chunk.len());
            ensure!(available != 0, "SDK upgrade exceeds 16 KiB");
            let count = stream.read(&mut chunk[..available]).await?;
            ensure!(count != 0, "SDK upgrade connection closed");
            prefetched.extend_from_slice(&chunk[..count]);
            if let Some(offset) = prefetched.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let end = offset + 4;
                ensure!(end <= UPGRADE_LIMIT, "SDK upgrade exceeds 16 KiB");
                return Ok(Self {
                    stream,
                    prefetched,
                    consumed: 0,
                    upgrade_end: Some(end),
                    read_chunk: usize::MAX,
                });
            }
        }
    }
}

impl AsyncRead for UpgradeStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        output: &mut ReadBuf<'_>,
    ) -> TaskPoll<io::Result<()>> {
        if output.remaining() == 0 {
            return TaskPoll::Ready(Ok(()));
        }
        let buffered_end = self.upgrade_end.unwrap_or(self.prefetched.len());
        if self.consumed < buffered_end {
            let count = (buffered_end - self.consumed)
                .min(output.remaining())
                .min(self.read_chunk);
            output.put_slice(&self.prefetched[self.consumed..self.consumed + count]);
            self.consumed += count;
            return TaskPoll::Ready(Ok(()));
        }
        if self.upgrade_end.is_some() {
            return TaskPoll::Ready(Err(io::Error::other(
                "SDK HTTP upgrade header was not accepted",
            )));
        }
        let available = output.remaining().min(self.read_chunk);
        let mut limited = ReadBuf::new(output.initialize_unfilled_to(available));
        match Pin::new(&mut self.stream).poll_read(cx, &mut limited) {
            TaskPoll::Ready(Ok(())) => {
                let count = limited.filled().len();
                output.advance(count);
                TaskPoll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

impl AsyncWrite for UpgradeStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        bytes: &[u8],
    ) -> TaskPoll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> TaskPoll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> TaskPoll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}
