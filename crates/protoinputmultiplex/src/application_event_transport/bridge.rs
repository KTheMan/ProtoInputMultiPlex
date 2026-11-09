use super::authority::*;
use mux_application_sdk::*;
use std::{
    collections::{BTreeMap, VecDeque},
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
    Owner {
        lease: Arc<Lease>,
        frame: OwnerFrame,
        deadline: Instant,
        admission: Arc<Admission>,
    },
}

struct RemoteBinding {
    lease_id: u64,
    owner: RecipientId,
    epoch: u64,
    token: u64,
    generation: u64,
    identity_metadata_consent: bool,
    last_sample: Instant,
}

struct PendingControl {
    operation_id: u64,
    request_id: u64,
    consumer_lease: u64,
    owner_lease: u64,
    window: WindowId,
    control: WindowControl,
    deadline: Instant,
    admission: Arc<Admission>,
}

#[derive(Debug, Clone)]
pub(crate) struct HostIntent {
    pub ticket: u64,
    pub action: HostedAction,
    pub reference: HostRevision,
}

struct PendingHost {
    request_id: u64,
    lease_id: u64,
    action: HostedAction,
    deadline: Instant,
    admission: Arc<Admission>,
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

#[derive(Debug, Default, Clone, serde::Serialize)]
pub(crate) struct WindowStatus {
    pub registered: usize,
    pub pending: usize,
    pub stale: usize,
    pub uncertain: usize,
    pub oldest_sample_age_ms: Option<u64>,
}

/// Owns Host on its creating UI thread. tick/drain perform no network or file IO.
/// Call at least every 20 ms while live, including during background Service stop.
/// Dropping this owner fences and ends Host before releasing its native adapter.
pub(crate) struct HostBridge<A: WindowBackend = PlatformWindows> {
    host: Host<A>,
    shared: Arc<Shared>,
    requests: mpsc::Receiver<UiRequest>,
    clients: BTreeMap<u64, Client>,
    remote: BTreeMap<WindowId, RemoteBinding>,
    retired_remote: BTreeMap<(u64, u64, u64), WindowId>,
    pending_controls: BTreeMap<u64, PendingControl>,
    host_intents: VecDeque<HostIntent>,
    pending_host: BTreeMap<u64, PendingHost>,
    next_operation: u64,
    uncertain_controls: usize,
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
                remote: BTreeMap::new(),
                retired_remote: BTreeMap::new(),
                pending_controls: BTreeMap::new(),
                host_intents: VecDeque::new(),
                pending_host: BTreeMap::new(),
                next_operation: 1,
                uncertain_controls: 0,
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

    pub fn screens(&self) -> &ScreenState { self.host.screens() }
    pub fn create_desktop_observed(&mut self, owner: RecipientId, name: &str, baseline: DesktopBaselineV1) -> Result<UserId, Error> {
        self.host.create_desktop_observed(owner, name, baseline)
    }
    pub fn publish_desktop_observation(&mut self, owner: RecipientId, user: UserId, record: DesktopRecordV1) -> Result<(), Error> {
        self.host.publish_desktop_observation(owner, user, record)
    }
    pub fn retire_desktop_observed(&mut self, owner: RecipientId, user: UserId) -> Result<(), Error> {
        self.host.retire_desktop_observed(owner, user)
    }
    pub fn is_desktop_observed(&self, user: UserId) -> bool { self.host.is_desktop_observed(user) }
    pub fn approve_desktop_observation(&mut self, recipient: RecipientId, consent: Option<DesktopConsentV1>) -> Result<(), Failure> {
        if self.has_permit(recipient) {
            self.revoke_recipient(recipient);
            return Err(failure(FailureCode::Ended, "receiver retired; register a fresh receiver before desktop approval"));
        }
        self.host.approve_desktop_observation(recipient, consent).map_err(Failure::from)
    }
    pub fn publish_hosted_state(&mut self, state: HostedState) -> Result<(), Error> {
        self.host.publish_hosted_state(state)
    }

    /// GUI-thread only. A queued intent expires without executing when its lease ends.
    pub fn take_host_intent(&mut self) -> Option<HostIntent> {
        while let Some(intent) = self.host_intents.pop_front() {
            let Some(pending) = self.pending_host.get(&intent.ticket) else {
                continue;
            };
            if Instant::now() >= pending.deadline
                || !self
                    .clients
                    .get(&pending.lease_id)
                    .is_some_and(|client| client.lease.live(&self.shared, Instant::now()))
            {
                continue;
            }
            return Some(intent);
        }
        None
    }

    pub fn host_action_pending(&self, ticket: u64) -> bool {
        self.pending_host.get(&ticket).is_some_and(|pending| {
            Instant::now() < pending.deadline
                && self
                    .clients
                    .get(&pending.lease_id)
                    .is_some_and(|client| client.lease.live(&self.shared, Instant::now()))
        })
    }

    pub fn host_action_recipient(&self, ticket: u64) -> Option<RecipientId> {
        if !self.host_action_pending(ticket) {
            return None;
        }
        let pending = self.pending_host.get(&ticket)?;
        let client = self.clients.get(&pending.lease_id)?;
        self.host
            .ready_for(client.lease.recipient)
            .ok()
            .filter(|ready| client.lease.matches(ready))
            .map(|ready| ready.recipient)
    }

    /// The GUI calls this after worker or later-frame viewport observation.
    /// A late receipt can update observed state but cannot complete an expired action.
    pub fn complete_host_action(&mut self, ticket: u64, result: Result<HostedState, Failure>) {
        let Some(pending) = self.pending_host.remove(&ticket) else {
            return;
        };
        let now = Instant::now();
        let live = self.clients.get(&pending.lease_id).is_some_and(|client| {
            client.lease.live(&self.shared, now)
                && self
                    .host
                    .ready_for(client.lease.recipient)
                    .is_ok_and(|ready| client.lease.matches(&ready))
        });
        if !live {
            pending.admission.uncertain(&self.shared);
            return;
        }
        let result = if now >= pending.deadline {
            Err(failure(
                FailureCode::Uncertain,
                "host action outcome unconfirmed; inspect fresh host state",
            ))
        } else {
            result
        };
        let response = result.map(|state| ClientResponse::HostedAction {
            action: pending.action,
            state,
        });
        pending.admission.state.store(COMPLETED, Ordering::Release);
        let Some(client) = self.clients.get(&pending.lease_id) else {
            pending.admission.uncertain(&self.shared);
            return;
        };
        if client
            .output
            .try_send(ServerMessage::Response {
                request_id: pending.request_id,
                reply: response.into(),
            })
            .is_err()
        {
            pending.admission.uncertain(&self.shared);
            client.lease.revoke();
        }
    }

    pub fn window_inventory(&self) -> Vec<(WindowInfo, RecipientId)> {
        let now = Instant::now();
        self.host
            .window_inventory()
            .into_iter()
            .filter(|(window, _)| {
                self.remote.get(&window.id).is_some_and(|binding| {
                    self.shared
                        .leases()
                        .iter()
                        .any(|lease| lease.id == binding.lease_id && lease.live(&self.shared, now))
                })
            })
            .collect()
    }

    pub fn window_status(&self) -> WindowStatus {
        let now = Instant::now();
        WindowStatus {
            registered: self.remote.len(),
            pending: self.pending_controls.len(),
            stale: self
                .window_inventory()
                .into_iter()
                .filter(|(window, _)| window.observation.stale)
                .count(),
            uncertain: self.uncertain_controls,
            oldest_sample_age_ms: self
                .remote
                .values()
                .map(|binding| {
                    u64::try_from(now.duration_since(binding.last_sample).as_millis())
                        .unwrap_or(u64::MAX)
                })
                .max(),
        }
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
            HostCommand::DelegateWindow { recipient, .. } if self.has_permit(*recipient) => {
                self.revoke_recipient(*recipient);
                return Err(failure(
                    FailureCode::Ended,
                    "receiver retired; register a fresh receiver before delegation",
                ));
            }
            HostCommand::DelegateWindow { window, access, .. }
                if access.observe || access.control =>
            {
                let Some(binding) = self.remote.get(window) else {
                    return Err(failure(FailureCode::Missing, "native window retired"));
                };
                if !self.shared.leases().iter().any(|lease| {
                    lease.id == binding.lease_id
                        && lease.live(&self.shared, Instant::now())
                        && lease.epoch == binding.epoch
                }) {
                    return Err(failure(FailureCode::Ended, "native window publisher ended"));
                }
            }
            HostCommand::Reset { .. } => {
                self.fence_all();
                self.expire_controls(Instant::now());
                self.expire_host_actions(Instant::now());
                self.remote.clear();
                self.retired_remote.clear();
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
        let mut ready = ready;
        ready.desktop = self.host.desktop_consent(recipient)?;
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
        self.retire_remote_for_lease(lease.id);
        self.expire_controls(Instant::now());
        self.expire_host_actions(Instant::now());
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
        self.expire_controls(now);
        self.expire_host_actions(now);
        for (window, binding) in &self.remote {
            if now.duration_since(binding.last_sample) >= Duration::from_secs(3) {
                let _ = self.host.mark_remote_window_stale(*window);
            }
        }
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
                    let version = (lease.desktop_version.load(Ordering::Acquire) == 1).then_some(1);
                    let state = if lease.live(&self.shared, now) && !self.clients.contains_key(&lease.id) {
                        self.host.negotiate_desktop_observation(lease.recipient, version)
                            .and_then(|()| self.host.ready_for(lease.recipient))
                    } else { Err(Error::Denied) };
                    if state.is_ok() { lease.desktop_ready.store(true, Ordering::Release); }
                    let result = match state {
                        Ok(mut state)
                            if lease.live(&self.shared, now)
                                && lease.matches(&state)
                                && !self.clients.contains_key(&lease.id) =>
                        {
                            stamp_capabilities(&mut state.capabilities, state.events.host_state);
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
                UiRequest::Owner {
                    lease,
                    frame,
                    deadline,
                    admission,
                } => {
                    report.commands += 1;
                    self.apply_owner(lease, frame, deadline, admission, now);
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
        report.pending =
            self.requests.len() + self.pending_controls.len() + self.pending_host.len();
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
        let output = client.output.clone();
        // Recheck the atomic fence immediately before the actual host call. A call
        // already applying when stop wins is uncertain and cleaned, never retried.
        let mut result = validation.and_then(|()| {
            if Instant::now() >= deadline || !lease.live(&self.shared, Instant::now()) {
                return Err(failure(
                    FailureCode::Denied,
                    "event receiver revoked or request expired before apply",
                ));
            }
            if let ClientCommand::RemoteWindowControl { reference, control } =
                &message.request.command
            {
                if reference.epoch != lease.epoch {
                    return Err(failure(
                        FailureCode::StaleRevision,
                        "window session changed",
                    ));
                }
                return self.begin_remote_control(
                    &lease,
                    message.request_id,
                    reference.window,
                    control.clone(),
                    deadline,
                    Arc::clone(&admission),
                );
            }
            match &message.request.command {
                ClientCommand::HostedSetMode { reference, mode } => {
                    return self.begin_host_action(
                        &lease,
                        message.request_id,
                        *reference,
                        HostedAction::SetMode(*mode),
                        deadline,
                        Arc::clone(&admission),
                    );
                }
                ClientCommand::HostedViewport { reference, action } => {
                    return self.begin_host_action(
                        &lease,
                        message.request_id,
                        *reference,
                        HostedAction::Viewport(*action),
                        deadline,
                        Arc::clone(&admission),
                    );
                }
                _ => {}
            }
            self.host.dispatch(lease.recipient, message.request.clone())
        });
        if matches!(&result, Err(error) if error.code == FailureCode::Uncertain && matches!(error.message.as_str(), "pending owner receipt" | "pending host receipt"))
        {
            return;
        }
        if let Ok(response) = &mut result {
            stamp_response(response, lease.events.host_state);
            if let Some(client) = self.clients.get_mut(&lease.id) {
                match response {
                    ClientResponse::Subscribed { subscription, .. } => {
                        client.subscription = Some(*subscription);
                        client.last_gap = None;
                    }
                    ClientResponse::Acknowledged
                        if matches!(
                            &message.request.command,
                            ClientCommand::Unsubscribe { .. }
                        ) =>
                    {
                        client.subscription = None
                    }
                    ClientResponse::Disconnected => {
                        lease.revoke();
                    }
                    _ => {}
                }
            }
        }
        admission.state.store(COMPLETED, Ordering::Release);
        // One FIFO carries reply and stream frames; snapshot replies always precede
        // any events polled for that new subscription in this or later ticks.
        if !lease.live(&self.shared, Instant::now())
            || output
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

    fn begin_host_action(
        &mut self,
        lease: &Arc<Lease>,
        request_id: u64,
        reference: HostRevision,
        action: HostedAction,
        deadline: Instant,
        admission: Arc<Admission>,
    ) -> Result<ClientResponse, Failure> {
        if reference.epoch != lease.epoch {
            return Err(failure(FailureCode::StaleRevision, "host session changed"));
        }
        let ready = self.host.ready_for(lease.recipient)?;
        if !ready.events.host_state
            || !match action {
                HostedAction::SetMode(_) => ready.controls.host_mode_control,
                HostedAction::Viewport(_) => ready.controls.host_viewport_control,
            }
        {
            return Err(failure(FailureCode::Denied, "host action is not approved"));
        }
        let state = self.host.hosted_state(lease.recipient, reference.epoch)?;
        if state.revision != reference.revision {
            return Err(failure(FailureCode::StaleRevision, "host state changed"));
        }
        if self.pending_host.len() >= 8
            || self
                .pending_host
                .values()
                .any(|pending| pending.lease_id == lease.id)
        {
            return Err(failure(FailureCode::Limit, "host action already pending"));
        }
        let ticket = self.next_operation;
        self.next_operation = self
            .next_operation
            .checked_add(1)
            .ok_or_else(|| failure(FailureCode::Limit, "host operation IDs exhausted"))?;
        self.pending_host.insert(
            ticket,
            PendingHost {
                request_id,
                lease_id: lease.id,
                action,
                deadline: deadline
                    .checked_sub(Duration::from_millis(250))
                    .unwrap_or(deadline),
                admission,
            },
        );
        self.host_intents.push_back(HostIntent {
            ticket,
            action,
            reference,
        });
        Err(failure(FailureCode::Uncertain, "pending host receipt"))
    }

    fn expire_host_actions(&mut self, now: Instant) {
        let expired: Vec<_> = self
            .pending_host
            .iter()
            .filter_map(|(ticket, pending)| {
                (now >= pending.deadline
                    || !self
                        .clients
                        .get(&pending.lease_id)
                        .is_some_and(|client| client.lease.live(&self.shared, now)))
                .then_some(*ticket)
            })
            .collect();
        for ticket in expired {
            self.complete_host_action(
                ticket,
                Err(failure(
                    FailureCode::Uncertain,
                    "host action outcome unconfirmed; inspect fresh host state",
                )),
            );
        }
        self.host_intents
            .retain(|intent| self.pending_host.contains_key(&intent.ticket));
    }

    fn begin_remote_control(
        &mut self,
        lease: &Arc<Lease>,
        request_id: u64,
        window: WindowId,
        control: WindowControl,
        deadline: Instant,
        admission: Arc<Admission>,
    ) -> Result<ClientResponse, Failure> {
        let owner = self
            .host
            .authorize_remote_window_control(lease.recipient, window, &control)
            .map_err(Failure::from)?;
        let binding = self
            .remote
            .get(&window)
            .ok_or_else(|| failure(FailureCode::Missing, "window retired"))?;
        if binding.owner != owner || binding.epoch != lease.epoch {
            return Err(failure(FailureCode::Denied, "window owner changed"));
        }
        let owner_lease = self
            .shared
            .leases()
            .into_iter()
            .find(|value| value.id == binding.lease_id)
            .ok_or_else(|| failure(FailureCode::Ended, "window publisher ended"))?;
        if !owner_lease.live(&self.shared, Instant::now())
            || !self.clients.contains_key(&owner_lease.id)
        {
            return Err(failure(FailureCode::Ended, "window publisher disconnected"));
        }
        if self.pending_controls.len() >= 16
            || self
                .pending_controls
                .values()
                .any(|value| value.window == window)
        {
            return Err(failure(FailureCode::Limit, "window action already pending"));
        }
        let operation_id = self.next_operation;
        self.next_operation = self
            .next_operation
            .checked_add(1)
            .ok_or_else(|| failure(FailureCode::Limit, "window operation IDs exhausted"))?;
        let frame = ServerMessage::WindowApply {
            operation_id,
            window,
            token: binding.token,
            generation: binding.generation,
            control: control.clone(),
        };
        let output = &self
            .clients
            .get(&owner_lease.id)
            .ok_or_else(|| failure(FailureCode::Ended, "window publisher disconnected"))?
            .output;
        output
            .try_send(frame)
            .map_err(|_| failure(FailureCode::Limit, "window publisher output full"))?;
        let _ = self.host.mark_remote_window_stale(window);
        self.pending_controls.insert(
            operation_id,
            PendingControl {
                operation_id,
                request_id,
                consumer_lease: lease.id,
                owner_lease: owner_lease.id,
                window,
                control,
                deadline: deadline
                    .checked_sub(Duration::from_millis(250))
                    .unwrap_or(deadline),
                admission,
            },
        );
        Err(failure(FailureCode::Uncertain, "pending owner receipt"))
    }

    fn apply_owner(
        &mut self,
        lease: Arc<Lease>,
        frame: OwnerFrame,
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
        let urgent = matches!(&frame.operation, OwnerOperation::ApplyResult { .. });
        let valid_id = if urgent {
            frame.request_id & (1_u64 << 63) != 0
        } else {
            frame.request_id != 0
                && frame.request_id & (1_u64 << 63) == 0
                && frame.request_id > client.last_request
        };
        if valid_id && !urgent {
            client.last_request = frame.request_id;
        }
        let output = client.output.clone();
        let result = if !valid_id {
            Err(failure(
                FailureCode::Invalid,
                "request IDs must strictly increase",
            ))
        } else if !lease.live(&self.shared, Instant::now()) || Instant::now() >= deadline {
            Err(failure(FailureCode::Denied, "publisher request expired"))
        } else {
            self.owner_operation(&lease, frame.operation)
        };
        admission.state.store(COMPLETED, Ordering::Release);
        if !lease.live(&self.shared, Instant::now())
            || output
                .try_send(ServerMessage::OwnerResponse {
                    request_id: frame.request_id,
                    reply: result.into(),
                })
                .is_err()
        {
            admission.uncertain(&self.shared);
            lease.revoke();
        }
    }

    fn owner_operation(
        &mut self,
        lease: &Arc<Lease>,
        operation: OwnerOperation,
    ) -> Result<OwnerResponse, Failure> {
        let now = Instant::now();
        if !lease.live(&self.shared, now)
            || !self.clients.contains_key(&lease.id)
            || !self
                .host
                .ready_for(lease.recipient)
                .is_ok_and(|ready| lease.matches(&ready))
        {
            return Err(failure(
                FailureCode::Denied,
                "window publisher lease is no longer current",
            ));
        }
        match operation {
            OwnerOperation::Register {
                token,
                generation,
                mut observed,
                identity_metadata_consent,
            } => {
                if !lease.events.owned_windows || !lease.controls.window_publication {
                    return Err(failure(
                        FailureCode::Denied,
                        "window publication is unavailable",
                    ));
                }
                if token == 0
                    || generation == 0
                    || self
                        .remote
                        .values()
                        .filter(|value| value.lease_id == lease.id)
                        .count()
                        >= 16
                    || self
                        .remote
                        .values()
                        .any(|value| value.lease_id == lease.id && value.token == token)
                    || self
                        .retired_remote
                        .keys()
                        .any(|(lease_id, retired_token, _)| {
                            *lease_id == lease.id && *retired_token == token
                        })
                {
                    return Err(failure(
                        FailureCode::Limit,
                        "window publication limit or duplicate token",
                    ));
                }
                let consent = lease.controls.window_identity_metadata && identity_metadata_consent;
                sanitize_observation(&mut observed, consent);
                let window = self
                    .host
                    .register_remote_window(lease.recipient, observed)
                    .map_err(Failure::from)?;
                self.remote.insert(
                    window,
                    RemoteBinding {
                        lease_id: lease.id,
                        owner: lease.recipient,
                        epoch: lease.epoch,
                        token,
                        generation,
                        identity_metadata_consent: consent,
                        last_sample: Instant::now(),
                    },
                );
                Ok(OwnerResponse::Registered { window })
            }
            OwnerOperation::Refresh {
                window,
                token,
                generation,
                mut observed,
                identity_metadata_consent,
            } => {
                let binding = self
                    .remote
                    .get_mut(&window)
                    .ok_or_else(|| failure(FailureCode::Missing, "window retired"))?;
                if binding.lease_id != lease.id
                    || binding.owner != lease.recipient
                    || binding.epoch != lease.epoch
                    || binding.token != token
                    || binding.generation != generation
                {
                    return Err(failure(FailureCode::Denied, "stale window binding"));
                }
                let consent = lease.controls.window_identity_metadata && identity_metadata_consent;
                sanitize_observation(&mut observed, consent);
                let info = self
                    .host
                    .refresh_remote_window(lease.recipient, window, observed)
                    .map_err(Failure::from)?;
                binding.identity_metadata_consent = consent;
                binding.last_sample = Instant::now();
                Ok(OwnerResponse::Window { window: info })
            }
            OwnerOperation::Retire {
                window,
                token,
                generation,
            } => {
                let Some(binding) = self.remote.get(&window) else {
                    if self.retired_remote.get(&(lease.id, token, generation)) == Some(&window) {
                        return Ok(OwnerResponse::Acknowledged);
                    }
                    return Err(failure(FailureCode::Missing, "window retired"));
                };
                if binding.lease_id != lease.id
                    || binding.owner != lease.recipient
                    || binding.epoch != lease.epoch
                    || binding.token != token
                    || binding.generation != generation
                {
                    return Err(failure(FailureCode::Denied, "stale window binding"));
                }
                self.host
                    .retire_remote_window(lease.recipient, window)
                    .map_err(Failure::from)?;
                self.remote.remove(&window);
                self.retired_remote
                    .insert((lease.id, token, generation), window);
                if self.retired_remote.len() > 128
                    && let Some(oldest) = self.retired_remote.keys().next().copied()
                {
                    self.retired_remote.remove(&oldest);
                }
                self.expire_controls(Instant::now());
                Ok(OwnerResponse::Acknowledged)
            }
            OwnerOperation::ApplyResult {
                operation_id,
                window,
                token,
                generation,
                result,
            } => {
                let binding = self
                    .remote
                    .get(&window)
                    .ok_or_else(|| failure(FailureCode::Missing, "window retired"))?;
                let pending = self
                    .pending_controls
                    .get(&operation_id)
                    .ok_or_else(|| failure(FailureCode::Missing, "window action expired"))?;
                if binding.lease_id != lease.id
                    || binding.owner != lease.recipient
                    || binding.epoch != lease.epoch
                    || binding.token != token
                    || binding.generation != generation
                    || pending.window != window
                    || pending.owner_lease != lease.id
                    || pending.operation_id != operation_id
                {
                    return Err(failure(FailureCode::Denied, "stale window action receipt"));
                }
                let pending = self
                    .pending_controls
                    .remove(&operation_id)
                    .ok_or_else(|| failure(FailureCode::Missing, "window action expired"))?;
                let response = match result {
                    Reply::Ok {
                        version,
                        mut response,
                    } if version == PROTOCOL_VERSION => {
                        sanitize_observation(&mut response, binding.identity_metadata_consent);
                        match self
                            .host
                            .refresh_remote_window(lease.recipient, window, response)
                        {
                            Ok(info) => Ok(ClientResponse::WindowControlled {
                                acknowledgement: WindowAcknowledgement {
                                    window,
                                    action: pending.control.action(),
                                    observed: info,
                                },
                            }),
                            Err(_) => Err(failure(
                                FailureCode::Uncertain,
                                "window action outcome unconfirmed; refresh the owner observation",
                            )),
                        }
                    }
                    _ => Err(failure(
                        FailureCode::Uncertain,
                        "window action outcome unconfirmed; refresh the owner observation",
                    )),
                };
                self.complete_control(pending, response);
                Ok(OwnerResponse::Acknowledged)
            }
        }
    }

    fn complete_control(
        &mut self,
        pending: PendingControl,
        result: Result<ClientResponse, Failure>,
    ) {
        if result.is_err() {
            self.uncertain_controls = self.uncertain_controls.saturating_add(1);
            let _ = self.host.mark_remote_window_stale(pending.window);
            pending.admission.uncertain(&self.shared);
        }
        pending.admission.state.store(COMPLETED, Ordering::Release);
        if let Some(client) = self.clients.get(&pending.consumer_lease)
            && client.lease.live(&self.shared, Instant::now())
            && client
                .output
                .try_send(ServerMessage::Response {
                    request_id: pending.request_id,
                    reply: result.into(),
                })
                .is_ok()
        {
            return;
        }
        pending.admission.uncertain(&self.shared);
    }

    fn expire_controls(&mut self, now: Instant) {
        let expired: Vec<_> = self
            .pending_controls
            .iter()
            .filter_map(|(id, pending)| {
                let owner_live = self
                    .clients
                    .get(&pending.owner_lease)
                    .is_some_and(|client| client.lease.live(&self.shared, now));
                let consumer_live = self
                    .clients
                    .get(&pending.consumer_lease)
                    .is_some_and(|client| client.lease.live(&self.shared, now));
                (now >= pending.deadline
                    || !owner_live
                    || !consumer_live
                    || !self.remote.contains_key(&pending.window))
                .then_some(*id)
            })
            .collect();
        for id in expired {
            if let Some(pending) = self.pending_controls.remove(&id) {
                self.complete_control(
                    pending,
                    Err(failure(
                        FailureCode::Uncertain,
                        "window action outcome unconfirmed; refresh the owner observation",
                    )),
                );
            }
        }
    }

    fn retire_remote_for_lease(&mut self, lease_id: u64) {
        self.retired_remote.retain(|(id, _, _), _| *id != lease_id);
        let windows: Vec<_> = self
            .remote
            .iter()
            .filter_map(|(id, binding)| {
                (binding.lease_id == lease_id).then_some((*id, binding.owner))
            })
            .collect();
        for (window, owner) in windows {
            let _ = self.host.retire_remote_window(owner, window);
            self.remote.remove(&window);
        }
    }

    /// UI-thread Stop/Quit boundary. No join, network operation or descriptor IO.
    pub fn stop(&mut self) {
        self.shared.request_stop();
        self.requests.close();
        self.expire_controls(Instant::now());
        self.expire_host_actions(Instant::now());
        self.remote.clear();
        self.retired_remote.clear();
        self.host.end();
        self.mark_all_clean();
        while let Ok(request) = self.requests.try_recv() {
            match request {
                UiRequest::Command { admission, .. } | UiRequest::Owner { admission, .. } => {
                    admission.cancel_queued();
                }
                UiRequest::Connect { .. } => {}
            }
        }
        self.expire_controls(Instant::now());
        self.expire_host_actions(Instant::now());
    }
}
impl<A: WindowBackend> Drop for HostBridge<A> {
    fn drop(&mut self) {
        self.stop();
    }
}

fn sanitize_observation(observed: &mut WindowObservation, identity_metadata_consent: bool) {
    if !identity_metadata_consent {
        observed.metadata.process_id = None;
        observed.metadata.ui_thread_id = None;
    }
}

pub(super) fn failure(code: FailureCode, message: &str) -> Failure {
    Failure {
        code,
        message: message.into(),
    }
}
fn stamp_capabilities(capabilities: &mut Vec<CapabilityStatus>, host_state: bool) {
    if let Some(status) = capabilities
        .iter_mut()
        .find(|status| status.capability == Capability::BrowserTransport)
    {
        status.available = true;
        status.detail =
            "Event-only loopback WebSocket transport for cooperative virtual users".into();
    }
    if host_state {
        if let Some(status) = capabilities
            .iter_mut()
            .find(|status| status.capability == Capability::HostVisibility)
        {
            status.available = true;
            status.detail = "GUI-owned window minimize and restore observation".into();
        }
        if !capabilities
            .iter()
            .any(|status| status.capability == Capability::HostedRouting)
        {
            capabilities.push(CapabilityStatus {
                capability: Capability::HostedRouting,
                available: true,
                detail: "GUI-owned routing mode observation and approved control".into(),
            });
        }
    }
}
fn stamp_response(response: &mut ClientResponse, host_state: bool) {
    match response {
        ClientResponse::Snapshot { snapshot } | ClientResponse::Subscribed { snapshot, .. } => {
            stamp_capabilities(&mut snapshot.capabilities, host_state)
        }
        _ => {}
    }
}
