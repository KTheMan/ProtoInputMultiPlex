use super::authority::*;
use mux_application_sdk::*;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};

pub(super) const QUEUED: u8 = 0;
pub(super) const APPLYING: u8 = 1;
pub(super) const COMPLETED: u8 = 2;
pub(super) const CANCELLED: u8 = 3;
pub(super) struct Admission {
    pub state: AtomicU8,
    uncertain: AtomicBool,
}
impl Admission {
    pub fn new() -> Self {
        Self {
            state: AtomicU8::new(QUEUED),
            uncertain: AtomicBool::new(false),
        }
    }
    pub fn cancel_queued(&self) -> bool {
        self.state
            .compare_exchange(QUEUED, CANCELLED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    pub fn uncertain(&self, shared: &Shared) {
        if !self.uncertain.swap(true, Ordering::AcqRel) {
            shared.uncertain.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub(super) enum UiRequest {
    Connect {
        lease: Arc<Lease>,
        output: mpsc::Sender<ServerMessage>,
        ready: oneshot::Sender<Result<EventReady, Failure>>,
    },
    Command {
        lease: Arc<Lease>,
        message: ClientMessage,
        deadline: Instant,
        admission: Arc<Admission>,
    },
}

/// Send half only; it never contains the !Send Host or a native window adapter.
pub(crate) struct NetworkEndpoint {
    pub(super) shared: Arc<Shared>,
    pub(super) requests: mpsc::Sender<UiRequest>,
}

struct Client {
    lease: Arc<Lease>,
    output: mpsc::Sender<ServerMessage>,
    subscription: Option<SubscriptionId>,
    last_gap: Option<u64>,
    last_request: u64,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub(crate) struct TickReport {
    pub commands: usize,
    pub frames: usize,
    pub cleanups: usize,
    pub cleanup_failures: usize,
    pub pending: usize,
}

/// Owns Host on its creating UI thread. tick/drain perform no network or file IO.
/// Call at least every 20 ms while live, including during background Service stop.
/// Dropping this owner fences and ends Host before releasing its native adapter.
pub(crate) struct HostBridge<A: WindowBackend = PlatformWindows> {
    host: Host<A>,
    shared: Arc<Shared>,
    requests: mpsc::Receiver<UiRequest>,
    clients: BTreeMap<u64, Client>,
}

impl<A: WindowBackend> HostBridge<A> {
    pub fn new(host: Host<A>) -> (Self, NetworkEndpoint) {
        let shared = Arc::new(Shared::new());
        let (sender, receiver) = mpsc::channel(UI_CAPACITY);
        (
            Self {
                host,
                shared: Arc::clone(&shared),
                requests: receiver,
                clients: BTreeMap::new(),
            },
            NetworkEndpoint {
                shared,
                requests: sender,
            },
        )
    }
    pub fn host(&self) -> &Host<A> {
        &self.host
    }

    /// Explicit local setup only. Grant changes to a receiver with a permit first
    /// retire that receiver. Reissue through a newly registered receiver identity;
    /// stable local-owner UserIds remain unchanged. No queued old command survives.
    pub fn host_command(&mut self, request: HostRequest) -> Result<HostResponse, Failure> {
        if request.version != PROTOCOL_VERSION {
            return self.host.host_dispatch(request);
        }
        match &request.command {
            HostCommand::ApproveEvents { recipient, .. }
            | HostCommand::ApproveControls { recipient, .. }
                if self.has_permit(*recipient) =>
            {
                self.revoke_recipient(*recipient);
                return Err(failure(
                    FailureCode::Ended,
                    "receiver retired; register a fresh receiver before approval",
                ));
            }
            HostCommand::Reset { .. } => {
                self.fence_all();
                let result = self.host.host_dispatch(request);
                if result.is_ok() {
                    self.mark_all_clean();
                }
                return result;
            }
            HostCommand::End => {
                self.stop();
                return Ok(HostResponse::Acknowledged);
            }
            _ => {}
        }
        self.host.host_dispatch(request)
    }

    pub fn dispatch_local(
        &mut self,
        recipient: RecipientId,
        request: ClientRequest,
    ) -> Result<ClientResponse, Failure> {
        if matches!(&request.command, ClientCommand::Disconnect) && self.has_permit(recipient) {
            self.revoke_recipient(recipient);
            return Ok(ClientResponse::Disconnected);
        }
        self.host.dispatch(recipient, request)
    }

    pub fn approve_client(
        &mut self,
        recipient: RecipientId,
        exact_origin: &str,
        ttl: Duration,
    ) -> anyhow::Result<ClientPermit> {
        self.approve_client_at(recipient, exact_origin, ttl, Instant::now())
    }
    pub(super) fn approve_client_at(
        &mut self,
        recipient: RecipientId,
        origin: &str,
        ttl: Duration,
        now: Instant,
    ) -> anyhow::Result<ClientPermit> {
        anyhow::ensure!(
            !self.shared.stop.load(Ordering::Acquire),
            "event bridge is stopping"
        );
        validate_origins(&[origin.to_owned()])?;
        anyhow::ensure!(
            ttl >= Duration::from_millis(1) && ttl <= MAX_TTL,
            "event credential lifetime must be 1 ms through 5 minutes"
        );
        anyhow::ensure!(
            !self.has_permit(recipient),
            "receiver already has a permit; retire it before replacement"
        );
        let ready = self.host.ready_for(recipient)?;
        let mut leases = self
            .shared
            .leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        anyhow::ensure!(leases.len() < MAX_CLIENTS, "event client limit reached");
        let id = self
            .shared
            .next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| anyhow::anyhow!("event permit IDs exhausted"))?;
        let deadline = now
            .checked_add(ttl)
            .ok_or_else(|| anyhow::anyhow!("invalid event credential lifetime"))?;
        let lease = Arc::new(Lease::new(id, origin.to_owned(), ready, deadline));
        leases.insert(id, Arc::clone(&lease));
        Ok(ClientPermit {
            shared: Arc::clone(&self.shared),
            lease,
        })
    }

    fn has_permit(&self, recipient: RecipientId) -> bool {
        self.shared.leases().iter().any(|lease| {
            lease.recipient == recipient && !lease.cleanup_done.load(Ordering::Acquire)
        })
    }
    fn fence_all(&self) {
        for lease in self.shared.leases() {
            lease.revoke();
        }
    }
    fn mark_all_clean(&mut self) {
        for lease in self.shared.leases() {
            // Reset/End retires core state but intentionally retains ended tokens
            // until their owner polls/unsubscribes. Retire only our stream token.
            if self.retire_subscription(lease.id) {
                self.clients.remove(&lease.id);
                lease.cleanup_done.store(true, Ordering::Release);
            }
        }
        self.shared.changed.notify_waiters();
    }
    fn retire_subscription(&mut self, client_id: u64) -> bool {
        let Some(client) = self.clients.get_mut(&client_id) else {
            return true;
        };
        let Some(subscription) = client.subscription else {
            return true;
        };
        let result = self.host.dispatch(
            client.lease.recipient,
            ClientRequest {
                version: PROTOCOL_VERSION,
                command: ClientCommand::Unsubscribe { subscription },
            },
        );
        let settled = match result {
            Ok(_) => true,
            Err(error) => matches!(error.code, FailureCode::Missing),
        };
        if settled {
            client.subscription = None;
        }
        settled
    }

    pub fn revoke_recipient(&mut self, recipient: RecipientId) {
        for lease in self
            .shared
            .leases()
            .into_iter()
            .filter(|lease| lease.recipient == recipient)
        {
            lease.revoke();
            self.cleanup(&lease);
        }
    }
    fn cleanup(&mut self, lease: &Arc<Lease>) -> bool {
        if !self.retire_subscription(lease.id) {
            return false;
        }
        self.clients.remove(&lease.id);
        if lease.cleanup_done.load(Ordering::Acquire) {
            return true;
        }
        let result = self.host.dispatch(
            lease.recipient,
            ClientRequest {
                version: PROTOCOL_VERSION,
                command: ClientCommand::Disconnect,
            },
        );
        let settled = match result {
            Ok(_) => true,
            Err(error) => matches!(error.code, FailureCode::Missing | FailureCode::Ended),
        };
        if settled {
            lease.cleanup_done.store(true, Ordering::Release);
            self.shared.changed.notify_waiters();
        }
        settled
    }

    pub fn tick(&mut self) -> TickReport {
        self.tick_at(Instant::now())
    }
    pub fn drain(&mut self) -> TickReport {
        self.tick()
    }
    pub(super) fn tick_at(&mut self, now: Instant) -> TickReport {
        let mut report = TickReport::default();
        for lease in self.shared.leases() {
            let approved = lease.live(&self.shared, now)
                && self
                    .host
                    .ready_for(lease.recipient)
                    .is_ok_and(|ready| lease.matches(&ready));
            if !approved {
                lease.revoke();
                if self.cleanup(&lease) {
                    report.cleanups += 1;
                } else {
                    report.cleanup_failures += 1;
                }
            }
        }
        // A bounded turn keeps network load from monopolizing the host event loop.
        for _ in 0..16 {
            let Ok(request) = self.requests.try_recv() else {
                break;
            };
            match request {
                UiRequest::Connect {
                    lease,
                    output,
                    ready,
                } => {
                    let state = self.host.ready_for(lease.recipient);
                    let result = match state {
                        Ok(mut state)
                            if lease.live(&self.shared, now)
                                && lease.matches(&state)
                                && !self.clients.contains_key(&lease.id) =>
                        {
                            stamp_capabilities(&mut state.capabilities);
                            self.clients.insert(
                                lease.id,
                                Client {
                                    lease: Arc::clone(&lease),
                                    output,
                                    subscription: None,
                                    last_gap: None,
                                    last_request: 0,
                                },
                            );
                            Ok(state)
                        }
                        _ => Err(failure(
                            FailureCode::Denied,
                            "event receiver no longer approved",
                        )),
                    };
                    if ready.send(result).is_err() {
                        lease.revoke();
                    }
                }
                UiRequest::Command {
                    lease,
                    message,
                    deadline,
                    admission,
                } => {
                    report.commands += 1;
                    self.apply(lease, message, deadline, admission, now);
                }
            }
        }
        for client in self.clients.values_mut() {
            if !client.lease.live(&self.shared, now) {
                continue;
            }
            let Some(subscription) = client.subscription else {
                continue;
            };
            let result = self.host.dispatch(
                client.lease.recipient,
                ClientRequest {
                    version: PROTOCOL_VERSION,
                    command: ClientCommand::Poll {
                        subscription,
                        max_events: MAX_POLL_EVENTS,
                    },
                },
            );
            let Ok(ClientResponse::Polled { poll }) = result else {
                client.lease.revoke();
                continue;
            };
            let publish = match &poll {
                Poll::Events { events } => !events.is_empty(),
                Poll::Gap { lost_through, .. } => {
                    client.last_gap.replace(*lost_through) != Some(*lost_through)
                }
                Poll::Ended { .. } => {
                    client.subscription = None;
                    true
                }
            };
            if publish && client.lease.live(&self.shared, Instant::now()) {
                if client
                    .output
                    .try_send(ServerMessage::EventStream {
                        subscription,
                        stream: poll,
                    })
                    .is_ok()
                {
                    report.frames += 1;
                } else {
                    client.lease.revoke();
                }
            }
        }
        report.pending = self.requests.len();
        report
    }

    fn apply(
        &mut self,
        lease: Arc<Lease>,
        message: ClientMessage,
        deadline: Instant,
        admission: Arc<Admission>,
        now: Instant,
    ) {
        if now >= deadline
            || !lease.live(&self.shared, now)
            || !self
                .host
                .ready_for(lease.recipient)
                .is_ok_and(|ready| lease.matches(&ready))
        {
            admission.cancel_queued();
            lease.revoke();
            return;
        }
        let Some(client) = self.clients.get_mut(&lease.id) else {
            admission.cancel_queued();
            lease.revoke();
            return;
        };
        if admission
            .state
            .compare_exchange(QUEUED, APPLYING, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let validation = if message.request_id == 0 || message.request_id <= client.last_request {
            Err(failure(
                FailureCode::Invalid,
                "request IDs must strictly increase",
            ))
        } else {
            client.last_request = message.request_id;
            match &message.request.command {
                ClientCommand::Poll { .. } => Err(failure(
                    FailureCode::Unsupported,
                    "event subscriptions are server-pushed",
                )),
                ClientCommand::Subscribe { .. } if client.subscription.is_some() => {
                    Err(failure(FailureCode::Limit, "one stream per receiver"))
                }
                ClientCommand::Resubscribe { subscription, .. }
                | ClientCommand::Unsubscribe { subscription }
                | ClientCommand::Stats { subscription }
                    if client.subscription != Some(*subscription) =>
                {
                    Err(failure(
                        FailureCode::Denied,
                        "stream not owned by this connection",
                    ))
                }
                _ => Ok(()),
            }
        };
        // Recheck the atomic fence immediately before the actual host call. A call
        // already applying when stop wins is uncertain and cleaned, never retried.
        let mut result = validation.and_then(|()| {
            if Instant::now() >= deadline || !lease.live(&self.shared, Instant::now()) {
                return Err(failure(
                    FailureCode::Denied,
                    "event receiver revoked or request expired before apply",
                ));
            }
            self.host.dispatch(lease.recipient, message.request.clone())
        });
        if let Ok(response) = &mut result {
            stamp_response(response);
            match response {
                ClientResponse::Subscribed { subscription, .. } => {
                    client.subscription = Some(*subscription);
                    client.last_gap = None;
                }
                ClientResponse::Acknowledged
                    if matches!(&message.request.command, ClientCommand::Unsubscribe { .. }) =>
                {
                    client.subscription = None
                }
                ClientResponse::Disconnected => {
                    lease.revoke();
                }
                _ => {}
            }
        }
        admission.state.store(COMPLETED, Ordering::Release);
        // One FIFO carries reply and stream frames; snapshot replies always precede
        // any events polled for that new subscription in this or later ticks.
        if !lease.live(&self.shared, Instant::now())
            || client
                .output
                .try_send(ServerMessage::Response {
                    request_id: message.request_id,
                    reply: result.into(),
                })
                .is_err()
        {
            admission.uncertain(&self.shared);
            lease.revoke();
        }
    }

    /// UI-thread Stop/Quit boundary. No join, network operation or descriptor IO.
    pub fn stop(&mut self) {
        self.shared.request_stop();
        self.requests.close();
        self.host.end();
        self.mark_all_clean();
        while let Ok(request) = self.requests.try_recv() {
            if let UiRequest::Command { admission, .. } = request {
                admission.cancel_queued();
            }
        }
    }
}
impl<A: WindowBackend> Drop for HostBridge<A> {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(super) fn failure(code: FailureCode, message: &str) -> Failure {
    Failure {
        code,
        message: message.into(),
    }
}
fn stamp_capabilities(capabilities: &mut [CapabilityStatus]) {
    if let Some(status) = capabilities
        .iter_mut()
        .find(|status| status.capability == Capability::BrowserTransport)
    {
        status.available = true;
        status.detail =
            "Event-only loopback WebSocket transport for cooperative virtual users".into();
    }
}
fn stamp_response(response: &mut ClientResponse) {
    match response {
        ClientResponse::Snapshot { snapshot } | ClientResponse::Subscribed { snapshot, .. } => {
            stamp_capabilities(&mut snapshot.capabilities)
        }
        _ => {}
    }
}
