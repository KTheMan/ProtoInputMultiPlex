//! External event client: consumes the GUI server's Engine; never constructs Host.
//! All methods and model state belong to the creating UI thread. No reconnect or replay.
mod completion;
mod ffi;
mod network;
mod owned_windows;
#[cfg(test)]
mod tests;
mod wake;
use crate::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    marker::PhantomData,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};
use tokio::sync::Notify;

pub const MAX_NATIVE_CLIENTS: usize = 16;
static ACTIVE_WORKERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
struct WorkerPermit;
impl WorkerPermit {
    fn acquire() -> Result<Self, ClientError> {
        ACTIVE_WORKERS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_NATIVE_CLIENTS).then_some(n + 1)
            })
            .map(|_| Self)
            .map_err(|_| ClientError::Limit)
    }
}
impl Drop for WorkerPermit {
    fn drop(&mut self) {
        ACTIVE_WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}
pub const MAX_PENDING: usize = 32;
const MAX_INCOMING: usize = 64;
const MAX_QUEUED_BYTES: usize = 4 * 1024 * 1024;

/// Explicit transient connection setup. Never log/serialize this structure.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectConfig {
    pub endpoint: String,
    pub exact_origin: String,
    pub event_credential: String,
}
impl std::fmt::Debug for ConnectConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConnectConfig([private])")
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    LocalStop,
    Transport,
    Protocol,
    QueueOverflow,
    WindowLost,
    DeadlineUncertain,
    WorkerPanic,
    RemoteEnd,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientError {
    Invalid,
    Busy,
    Limit,
    Window,
    Closed,
    Protocol,
    Denied,
}
impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ClientError {}

struct Mailbox {
    outgoing: VecDeque<Outgoing>,
    urgent: VecDeque<OwnerFrame>,
    incoming: VecDeque<(usize, ServerMessage)>,
    bytes: usize,
    signalled: bool,
    terminal: Option<StopReason>,
}
#[derive(Clone)]
enum Outgoing {
    Client(ClientMessage),
    Owner(OwnerFrame),
}
impl Outgoing {
    fn request_id(&self) -> u64 {
        match self {
            Self::Client(value) => value.request_id,
            Self::Owner(value) => value.request_id,
        }
    }
}

enum OwnerPending {
    Register(u64),
    Refresh(u64),
    Retire,
    ApplyResult,
}
fn redact_owner_identity(observed: &mut WindowObservation, allowed: bool) {
    if !allowed {
        observed.metadata.process_id = None;
        observed.metadata.ui_thread_id = None;
    }
}
struct Shared {
    mailbox: Mutex<Mailbox>,
    stop: AtomicBool,
    changed: Notify,
    requests_changed: Notify,
    wake: wake::Target,
}
impl Shared {
    fn enqueue(&self, message: ServerMessage, bytes: usize) -> bool {
        let mut q = self
            .mailbox
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.stop.load(Ordering::Acquire) {
            return false;
        }
        if q.incoming.len() >= MAX_INCOMING || q.bytes.saturating_add(bytes) > MAX_QUEUED_BYTES {
            drop(q);
            self.finish(StopReason::QueueOverflow);
            return false;
        }
        q.bytes += bytes;
        q.incoming.push_back((bytes, message));
        if !q.signalled {
            q.signalled = self.wake.post();
            if !q.signalled {
                drop(q);
                self.finish(StopReason::WindowLost);
                return false;
            }
        }
        true
    }
    fn finish(&self, reason: StopReason) {
        let mut q = self
            .mailbox
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.stop.store(true, Ordering::Release);
        q.incoming.clear();
        q.outgoing.clear();
        q.urgent.clear();
        q.bytes = 0;
        if q.terminal.is_none() {
            q.terminal = Some(reason);
        }
        if !q.signalled {
            q.signalled = self.wake.post();
        }
        self.changed.notify_one();
    }
}

#[derive(Serialize)]
pub struct NativeStatus {
    pub connecting: bool,
    pub live: bool,
    pub needs_snapshot: bool,
    pub pointer_enabled: bool,
    pub key_enabled: bool,
    pub pending_requests: usize,
    pub owned_windows: usize,
    pub pending_owner_operations: usize,
    pub local_worker_retired: bool,
    /// EOF/retirement is not evidence that the GUI owner has reconciled producer holds.
    pub remote_cleanup_unconfirmed: bool,
    pub uncertain_request_ids: Vec<u64>,
    pub terminal: Option<StopReason>,
    pub wake_message: u32,
}
#[derive(Serialize)]
pub struct ModelView<'a> {
    pub epoch: Option<u64>,
    pub sequence: u64,
    pub users: Vec<&'a UserView>,
    pub hosted: Option<&'a HostedState>,
    pub screens: Option<&'a ScreenState>,
    pub windows: Vec<&'a WindowInfo>,
    pub ready: Option<&'a EventReady>,
    pub subscription: Option<SubscriptionId>,
    pub status: NativeStatus,
}

pub struct NativeClient {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    binding: wake::Binding,
    model: EventClientState,
    hosted_receipt: crate::client::HostedReceipt,
    ready: Option<EventReady>,
    subscription: Option<SubscriptionId>,
    pending: VecDeque<(u64, ClientCommand)>,
    owner_pending: BTreeMap<u64, OwnerPending>,
    seen_apply: BTreeSet<u64>,
    owned: Option<owned_windows::OwnedWindows>,
    next_id: u64,
    next_ack_id: u64,
    pointer: bool,
    keys: bool,
    terminal: Option<StopReason>,
    uncertain: Vec<u64>,
    _local: PhantomData<Rc<()>>,
}
impl NativeClient {
    /// Registers an own top-level HWND on this thread, then starts one bounded I/O worker.
    /// Call stop from WM_NCDESTROY before returning. Drain/query only on this thread.
    pub fn connect(config: ConnectConfig, own_hwnd: usize) -> Result<Self, ClientError> {
        let address = network::validate_config(&config)?;
        let binding = wake::Binding::new(own_hwnd)?;
        let shared = Arc::new(Shared {
            mailbox: Mutex::new(Mailbox {
                outgoing: VecDeque::new(),
                urgent: VecDeque::new(),
                incoming: VecDeque::new(),
                bytes: 0,
                signalled: false,
                terminal: None,
            }),
            stop: AtomicBool::new(false),
            changed: Notify::new(),
            requests_changed: Notify::new(),
            wake: binding.target.clone(),
        });
        let worker_shared = Arc::clone(&shared);
        let permit = WorkerPermit::acquire()?;
        let worker = std::thread::Builder::new()
            .name("application-event-client".into())
            .spawn(move || {
                let _permit = permit;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    match tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        Ok(runtime) => runtime.block_on(network::run(
                            config,
                            address,
                            Arc::clone(&worker_shared),
                        )),
                        Err(_) => worker_shared.finish(StopReason::Transport),
                    }
                }));
                if result.is_err() {
                    worker_shared.finish(StopReason::WorkerPanic);
                }
            })
            .map_err(|_| ClientError::Limit)?;
        Ok(Self {
            shared,
            worker: Some(worker),
            binding,
            model: EventClientState::default(),
            hosted_receipt: crate::client::HostedReceipt::default(),
            ready: None,
            subscription: None,
            pending: VecDeque::new(),
            owner_pending: BTreeMap::new(),
            seen_apply: BTreeSet::new(),
            owned: None,
            next_id: 1,
            next_ack_id: 1,
            pointer: false,
            keys: false,
            terminal: None,
            uncertain: Vec::new(),
            _local: PhantomData,
        })
    }
    fn observe_terminal(&mut self) {
        let reason = self
            .shared
            .mailbox
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .terminal;
        if let Some(reason) = reason {
            self.clear(reason);
        }
    }
    fn clear(&mut self, reason: StopReason) {
        if self.terminal.is_none() {
            for id in self
                .pending
                .iter()
                .map(|(id, _)| *id)
                .chain(self.owner_pending.keys().copied())
            {
                if self.uncertain.len() < 64 && !self.uncertain.contains(&id) {
                    self.uncertain.push(id);
                }
            }
        }
        self.pending.clear();
        self.owner_pending.clear();
        self.seen_apply.clear();
        if let Some(owned) = &mut self.owned {
            owned.clear();
        }
        self.owned = None;
        self.subscription = None;
        self.ready = None;
        self.pointer = false;
        self.keys = false;
        self.model.disconnect();
        self.hosted_receipt.clear();
        self.terminal = Some(reason);
    }
    pub fn stop(&mut self) {
        self.shared.finish(StopReason::LocalStop);
        self.observe_terminal();
    }
    /// True only after the Windows thread object signals actual termination.
    pub fn retired(&self) -> bool {
        self.worker.as_ref().is_none_or(completion::terminated)
    }
    /// Independent of HWND lifetime. Caller owns this SYNCHRONIZE-only duplicate.
    pub fn completion_handle(&self) -> Result<std::os::windows::io::OwnedHandle, ClientError> {
        completion::duplicate(self.worker.as_ref().ok_or(ClientError::Closed)?)
    }
    /// Zero-time probe first; joining an already terminated thread cannot wait
    /// for socket/runtime/TLS teardown. Consume its JoinHandle before success.
    pub fn complete_destroy(&mut self) -> bool {
        if !self.retired() {
            return false;
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        true
    }
    pub fn set_input_enabled(&mut self, pointer: bool, keys: bool) -> Result<(), ClientError> {
        self.observe_terminal();
        if self.terminal.is_some() {
            return Err(ClientError::Closed);
        }
        // Disabling an enabled producer fences the connection; server EOF cleanup
        // reconciles its holds. This never guesses which producer owns a held key.
        if (self.pointer && !pointer) || (self.keys && !keys) {
            self.stop();
            return Ok(());
        }
        self.pointer = pointer;
        self.keys = keys;
        Ok(())
    }
    /// Query the current GUI-owned hosted session through the existing request ABI.
    pub fn query_hosted(&mut self) -> Result<u64, ClientError> {
        let epoch = self.ready.as_ref().ok_or(ClientError::Busy)?.epoch;
        self.request(ClientRequest {
            version: PROTOCOL_VERSION,
            command: ClientCommand::HostedStateQuery { epoch },
        })
    }
    /// Explicit, revision-fenced host action; never retried or replayed.
    pub fn hosted_action(
        &mut self,
        reference: HostRevision,
        action: HostedAction,
    ) -> Result<u64, ClientError> {
        let command = match action {
            HostedAction::SetMode(mode) => ClientCommand::HostedSetMode { reference, mode },
            HostedAction::Viewport(action) => ClientCommand::HostedViewport { reference, action },
        };
        self.request(ClientRequest {
            version: PROTOCOL_VERSION,
            command,
        })
    }
    pub fn request(&mut self, request: ClientRequest) -> Result<u64, ClientError> {
        self.observe_terminal();
        if self.terminal.is_some() {
            return Err(ClientError::Closed);
        }
        if request.version != PROTOCOL_VERSION {
            return Err(ClientError::Invalid);
        }
        let ready = self.ready.as_ref().ok_or(ClientError::Busy)?;
        authorize(
            &request.command,
            &self.model,
            &self.hosted_receipt,
            ready,
            self.subscription,
            &self.pending,
            (self.pointer, self.keys),
        )?;
        if self.pending.len() >= MAX_PENDING || self.next_id > 65_536 {
            return Err(ClientError::Limit);
        }
        let id = self.next_id;
        let command = request.command.clone();
        let message = ClientMessage {
            request_id: id,
            request,
        };
        if serde_json::to_vec(&message)
            .map_err(|_| ClientError::Invalid)?
            .len()
            > ready.limits.max_request_bytes
        {
            return Err(ClientError::Limit);
        }
        {
            let mut q = self
                .shared
                .mailbox
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.shared.stop.load(Ordering::Acquire) {
                return Err(ClientError::Closed);
            }
            if q.outgoing.len() >= MAX_PENDING {
                return Err(ClientError::Busy);
            }
            q.outgoing.push_back(Outgoing::Client(message));
        }
        self.shared.requests_changed.notify_one();
        if matches!(
            &command,
            ClientCommand::Subscribe { .. } | ClientCommand::Resubscribe { .. }
        ) {
            self.hosted_receipt.clear();
        }
        self.pending.push_back((id, command));
        self.next_id += 1;
        Ok(id)
    }
    fn queue_owner(
        &mut self,
        operation: OwnerOperation,
        pending: OwnerPending,
    ) -> Result<u64, ClientError> {
        self.observe_terminal();
        if self.terminal.is_some() {
            return Err(ClientError::Closed);
        }
        if self.owner_pending.len() >= MAX_PENDING || self.next_id > 65_536 {
            return Err(ClientError::Limit);
        }
        let request_id = self.next_id;
        let frame = OwnerFrame {
            request_id,
            operation,
        };
        if serde_json::to_vec(&frame)
            .map_err(|_| ClientError::Invalid)?
            .len()
            > MAX_REQUEST_BYTES
        {
            return Err(ClientError::Limit);
        }
        {
            let mut q = self
                .shared
                .mailbox
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if q.outgoing.len() >= MAX_PENDING || self.shared.stop.load(Ordering::Acquire) {
                return Err(ClientError::Busy);
            }
            q.outgoing.push_back(Outgoing::Owner(frame));
        }
        self.owner_pending.insert(request_id, pending);
        self.next_id += 1;
        self.shared.requests_changed.notify_one();
        Ok(request_id)
    }

    /// Locally bind a same-process, creating-thread top-level HWND. The returned
    /// token is owner-local; GUI registration completes only on OwnerResponse.
    pub fn register_own_window(
        &mut self,
        hwnd: usize,
        identity_metadata_consent: bool,
    ) -> Result<u64, ClientError> {
        self.observe_terminal();
        let ready = self.ready.as_ref().ok_or(ClientError::Busy)?;
        if !ready.events.owned_windows || !ready.controls.window_publication {
            return Err(ClientError::Denied);
        }
        let identity_allowed = ready.controls.window_identity_metadata;
        if self.owned.is_none() {
            self.owned = Some(owned_windows::OwnedWindows::new()?);
        }
        let (token, generation, mut observed) = self
            .owned
            .as_mut()
            .ok_or(ClientError::Closed)?
            .register(hwnd)?;
        let consent = identity_metadata_consent && identity_allowed;
        redact_owner_identity(&mut observed, consent);
        let operation = OwnerOperation::Register {
            token,
            generation,
            observed,
            identity_metadata_consent: consent,
        };
        if let Err(error) = self.queue_owner(operation, OwnerPending::Register(token)) {
            if let Some(owned) = &mut self.owned {
                let _ = owned.retire(token);
            }
            return Err(error);
        }
        Ok(token)
    }

    pub fn refresh_own_window(
        &mut self,
        token: u64,
        identity_metadata_consent: bool,
    ) -> Result<u64, ClientError> {
        self.observe_terminal();
        let (window, generation, mut observed) = self
            .owned
            .as_mut()
            .ok_or(ClientError::Window)?
            .refresh(token)?;
        let consent = identity_metadata_consent
            && self
                .ready
                .as_ref()
                .is_some_and(|ready| ready.controls.window_identity_metadata);
        redact_owner_identity(&mut observed, consent);
        let result = self.queue_owner(
            OwnerOperation::Refresh {
                window,
                token,
                generation,
                observed,
                identity_metadata_consent: consent,
            },
            OwnerPending::Refresh(token),
        );
        if result.is_err()
            && let Some(owned) = &mut self.owned
        {
            owned.invalidate_report(token);
        }
        result
    }

    /// Call from the owner's WndProc after a state-changing message. Unchanged
    /// observations emit no frame; the owner retains responsibility for notices.
    pub fn notify_own_window_changed(
        &mut self,
        token: u64,
        identity_metadata_consent: bool,
    ) -> Result<Option<u64>, ClientError> {
        self.observe_terminal();
        let change = self
            .owned
            .as_mut()
            .ok_or(ClientError::Window)?
            .notify_changed(token)?;
        change
            .map(|(window, generation, mut observed)| {
                let raw_observation = observed.clone();
                let consent = identity_metadata_consent
                    && self
                        .ready
                        .as_ref()
                        .is_some_and(|ready| ready.controls.window_identity_metadata);
                redact_owner_identity(&mut observed, consent);
                let result = self.queue_owner(
                    OwnerOperation::Refresh {
                        window,
                        token,
                        generation,
                        observed: observed.clone(),
                        identity_metadata_consent: consent,
                    },
                    OwnerPending::Refresh(token),
                );
                if let Some(owned) = &mut self.owned {
                    if result.is_ok() {
                        owned.mark_reported(token, raw_observation);
                    } else {
                        owned.invalidate_report(token);
                    }
                }
                result
            })
            .transpose()
    }

    /// Call from WM_NCDESTROY before the HWND can be recycled. Local binding is
    /// retired immediately; the remote acknowledgement is separate and fallible.
    pub fn retire_own_window(&mut self, token: u64) -> Result<Option<u64>, ClientError> {
        self.observe_terminal();
        let (remote, generation, local_result) = self
            .owned
            .as_mut()
            .ok_or(ClientError::Window)?
            .retire(token)?;
        let remote_result = remote
            .map(|window| {
                self.queue_owner(
                    OwnerOperation::Retire {
                        window,
                        token,
                        generation,
                    },
                    OwnerPending::Retire,
                )
            })
            .transpose();
        local_result?;
        remote_result
    }

    fn queue_apply_result(&mut self, operation: OwnerOperation) -> Result<(), ClientError> {
        if self.owner_pending.len() >= MAX_PENDING || self.next_ack_id > 65_536 {
            return Err(ClientError::Limit);
        }
        let request_id = (1_u64 << 63) | self.next_ack_id;
        let frame = OwnerFrame {
            request_id,
            operation,
        };
        if serde_json::to_vec(&frame)
            .map_err(|_| ClientError::Invalid)?
            .len()
            > MAX_REQUEST_BYTES
        {
            return Err(ClientError::Limit);
        }
        {
            let mut q = self
                .shared
                .mailbox
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if q.urgent.len() >= owned_windows::MAX_OWNED_WINDOWS {
                return Err(ClientError::Limit);
            }
            q.urgent.push_back(frame);
        }
        self.owner_pending
            .insert(request_id, OwnerPending::ApplyResult);
        self.next_ack_id += 1;
        self.shared.requests_changed.notify_one();
        Ok(())
    }

    fn apply_on_owner_thread(
        &mut self,
        operation_id: u64,
        window: WindowId,
        token: u64,
        generation: u64,
        control: WindowControl,
    ) -> Result<(), ClientError> {
        if operation_id == 0
            || self.seen_apply.len() >= 65_536
            || !self.seen_apply.insert(operation_id)
        {
            return Err(ClientError::Protocol);
        }
        let result = self
            .owned
            .as_mut()
            .ok_or(Error::MissingWindow)
            .and_then(|owned| owned.apply(window, token, generation, control));
        let result = match result {
            Ok(mut response) => {
                redact_owner_identity(&mut response, false);
                Reply::Ok {
                    version: PROTOCOL_VERSION,
                    response,
                }
            }
            Err(error) => Reply::Error {
                version: PROTOCOL_VERSION,
                error: Failure {
                    code: FailureCode::Uncertain,
                    message: error.to_string(),
                },
            },
        };
        self.queue_apply_result(OwnerOperation::ApplyResult {
            operation_id,
            window,
            token,
            generation,
            result,
        })
    }
    /// Acknowledge every wake by draining until None. Each pop re-arms while data
    /// remains under the same queue mutex, so arrival vs acknowledge cannot lose a wake.
    /// Returned messages are authenticated and folded before caller callbacks/rendering.
    pub fn drain(&mut self) -> Result<Option<ServerMessage>, ClientError> {
        self.observe_terminal();
        if self.terminal.is_some() {
            return Ok(None);
        }
        let item = {
            let mut q = self
                .shared
                .mailbox
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            q.signalled = false;
            let item = q.incoming.pop_front();
            if let Some((bytes, _)) = &item {
                q.bytes -= bytes;
            }
            if !q.incoming.is_empty() {
                q.signalled = self.shared.wake.post();
                if !q.signalled {
                    drop(q);
                    self.shared.finish(StopReason::WindowLost);
                    self.observe_terminal();
                    return Err(ClientError::Window);
                }
            }
            item.map(|(_, message)| message)
        };
        if let Some(message) = &item
            && self.fold(message).is_err()
        {
            self.shared.finish(StopReason::Protocol);
            self.observe_terminal();
            return Err(ClientError::Protocol);
        }
        Ok(item)
    }
    fn fold(&mut self, message: &ServerMessage) -> Result<(), ClientError> {
        match message {
            ServerMessage::Ready { ready } => {
                if self.ready.is_some()
                    || ready.version != PROTOCOL_VERSION
                    || ready.epoch == 0
                    || ready.limits.max_request_bytes == 0
                    || ready.limits.max_request_bytes > MAX_REQUEST_BYTES
                    || ready.limits.max_poll_events == 0
                    || ready.limits.max_poll_events > MAX_POLL_EVENTS
                    || ready.limits.max_queue == 0
                    || ready.limits.max_queue > MAX_QUEUE
                    || !ready.events.enabled()
                    || !valid_scope(&ready.events.users, ready.epoch)
                    || !valid_scope(&ready.controls.users, ready.epoch)
                    || ready.capabilities.len() > 32
                    || ready.capabilities.iter().any(|c| c.detail.len() > 512)
                {
                    return Err(ClientError::Protocol);
                }
                self.ready = Some(ready.clone());
            }
            ServerMessage::Response { request_id, reply } => {
                let (id, command) = self.pending.front().cloned().ok_or(ClientError::Protocol)?;
                if id != *request_id {
                    return Err(ClientError::Protocol);
                }
                match reply {
                    Reply::Ok { version, response } => {
                        if *version != PROTOCOL_VERSION || !response_matches(&command, response) {
                            return Err(ClientError::Protocol);
                        }
                        if let ClientResponse::Subscribed {
                            subscription,
                            snapshot,
                        } = response
                        {
                            let epoch = self.ready.as_ref().ok_or(ClientError::Protocol)?.epoch;
                            self.model
                                .install_snapshot(snapshot.clone(), epoch)
                                .map_err(|_| ClientError::Protocol)?;
                            self.hosted_receipt.clear();
                            self.subscription = Some(*subscription);
                        } else if let ClientResponse::HostedState { state }
                        | ClientResponse::HostedAction { state, .. } = response
                        {
                            let epoch = self.ready.as_ref().ok_or(ClientError::Protocol)?.epoch;
                            self.hosted_receipt
                                .observe(epoch, state, &self.model)
                                .map_err(|_| ClientError::Protocol)?;
                        }
                    }
                    Reply::Error { version, error } if *version == PROTOCOL_VERSION => {
                        if error.code == FailureCode::Uncertain
                            && matches!(
                                command,
                                ClientCommand::HostedSetMode { .. }
                                    | ClientCommand::HostedViewport { .. }
                            )
                        {
                            return Err(ClientError::Protocol);
                        }
                    }
                    _ => return Err(ClientError::Protocol),
                }
                self.pending.pop_front();
            }
            ServerMessage::OwnerResponse { request_id, reply } => {
                let pending = self
                    .owner_pending
                    .remove(request_id)
                    .ok_or(ClientError::Protocol)?;
                match (pending, reply) {
                    (
                        OwnerPending::Register(token),
                        Reply::Ok {
                            version,
                            response: OwnerResponse::Registered { window },
                        },
                    ) if *version == PROTOCOL_VERSION => {
                        self.owned
                            .as_mut()
                            .ok_or(ClientError::Protocol)?
                            .bind_remote(token, *window)?;
                    }
                    (OwnerPending::Register(token), Reply::Error { version, .. })
                        if *version == PROTOCOL_VERSION =>
                    {
                        if let Some(owned) = &mut self.owned {
                            let _ = owned.retire(token);
                        }
                    }
                    (
                        OwnerPending::Refresh(_),
                        Reply::Ok {
                            version,
                            response: OwnerResponse::Window { .. },
                        },
                    )
                    | (
                        OwnerPending::Retire | OwnerPending::ApplyResult,
                        Reply::Ok {
                            version,
                            response: OwnerResponse::Acknowledged,
                        },
                    ) if *version == PROTOCOL_VERSION => {}
                    (OwnerPending::Refresh(token), Reply::Error { version, .. })
                        if *version == PROTOCOL_VERSION =>
                    {
                        if let Some(owned) = &mut self.owned {
                            owned.invalidate_report(token);
                        }
                    }
                    (
                        OwnerPending::Retire | OwnerPending::ApplyResult,
                        Reply::Error { version, .. },
                    ) if *version == PROTOCOL_VERSION => {
                        if self.uncertain.len() < 64 {
                            self.uncertain.push(*request_id);
                        }
                    }
                    _ => return Err(ClientError::Protocol),
                }
            }
            ServerMessage::WindowApply {
                operation_id,
                window,
                token,
                generation,
                control,
            } => {
                if self.ready.is_none() {
                    return Err(ClientError::Protocol);
                }
                if self
                    .apply_on_owner_thread(
                        *operation_id,
                        *window,
                        *token,
                        *generation,
                        control.clone(),
                    )
                    .is_err()
                {
                    self.shared.finish(StopReason::DeadlineUncertain);
                    self.observe_terminal();
                    return Err(ClientError::Protocol);
                }
            }
            ServerMessage::EventStream {
                subscription,
                stream,
            } => {
                if self.subscription != Some(*subscription) {
                    return Err(ClientError::Protocol);
                }
                // Refresh response orders the old/new token boundary. Queued
                // old-token loss can still raise the snapshot floor, but cannot
                // restore old user/input state while that response is in flight.
                let refreshing = self.pending.iter().any(|(_, command)| {
                    matches!(command,
                    ClientCommand::Resubscribe { subscription: old, .. } if old == subscription)
                });
                if refreshing && matches!(stream, Poll::Events { .. }) {
                    return Ok(());
                }
                let outcome = self
                    .model
                    .fold_poll(stream.clone())
                    .map_err(|_| ClientError::Protocol)?;
                match outcome {
                    FoldOutcome::Applied(_) => self
                        .hosted_receipt
                        .reconcile(&self.model)
                        .map_err(|_| ClientError::Protocol)?,
                    FoldOutcome::NeedsSnapshot | FoldOutcome::Ended(_) => {
                        self.hosted_receipt.clear();
                    }
                }
                if matches!(self.model.state(), StreamState::Ended(_)) {
                    self.shared.finish(StopReason::RemoteEnd);
                    self.observe_terminal();
                }
            }
            ServerMessage::Disconnected { .. } => {
                self.shared.finish(StopReason::RemoteEnd);
                self.observe_terminal();
            }
        }
        Ok(())
    }
    /// Owner-thread, consumer-local operation; does not allocate a remote request ID.
    pub fn local_toolkit(&mut self, request: &str) -> Result<String, crate::user_toolkit::ToolkitError> {
        self.observe_terminal();
        self.model.local_toolkit_json(request)
    }

    pub fn query(&mut self) -> ModelView<'_> {
        self.observe_terminal();
        let status = NativeStatus {
            connecting: self.ready.is_none() && self.terminal.is_none(),
            live: self.model.state() == &StreamState::Live,
            needs_snapshot: self.model.state() == &StreamState::NeedsSnapshot,
            pointer_enabled: self.pointer,
            key_enabled: self.keys,
            pending_requests: self.pending.len(),
            owned_windows: self
                .owned
                .as_ref()
                .map_or(0, owned_windows::OwnedWindows::len),
            pending_owner_operations: self.owner_pending.len(),
            local_worker_retired: self.retired(),
            remote_cleanup_unconfirmed: self.terminal.is_some(),
            uncertain_request_ids: self.uncertain.clone(),
            terminal: self.terminal,
            wake_message: self.binding.target.message,
        };
        ModelView {
            epoch: self.model.epoch(),
            sequence: self.model.last_sequence(),
            users: self.model.users().values().collect(),
            screens: self.model.screens(),
            hosted: self.model.hosted(),
            windows: self.model.windows().values().collect(),
            ready: self.ready.as_ref(),
            subscription: self.subscription,
            status,
        }
    }
}
impl Drop for NativeClient {
    fn drop(&mut self) {
        self.stop();
        // Rust Drop / owner TLS exit cannot synchronously wait for I/O. Detach
        // only as emergency cleanup; it is NOT authorization to unload the DLL.
        // Normal owner cleanup MUST call complete_destroy before dropping.
        let _ = self.complete_destroy();
    }
}

fn selects(scope: &UserScope, view: &UserView) -> bool {
    match scope {
        UserScope::AllOwned => view.owned_by_recipient,
        UserScope::Only(users) => users.contains(&view.id),
    }
}
fn authorize(
    command: &ClientCommand,
    model: &EventClientState,
    hosted_receipt: &crate::client::HostedReceipt,
    ready: &EventReady,
    subscription: Option<SubscriptionId>,
    pending: &VecDeque<(u64, ClientCommand)>,
    enables: (bool, bool),
) -> Result<(), ClientError> {
    let (pointer, keys) = enables;
    match command {
        ClientCommand::Subscribe { capacity }
            if subscription.is_none()
                && pending.is_empty()
                && (1..=MAX_QUEUE).contains(capacity) =>
        {
            return Ok(());
        }
        ClientCommand::Resubscribe {
            subscription: token,
            capacity,
        } if subscription == Some(*token)
            && pending.is_empty()
            && model.state() == &StreamState::NeedsSnapshot
            && (1..=MAX_QUEUE).contains(capacity) =>
        {
            return Ok(());
        }
        ClientCommand::Snapshot
        | ClientCommand::Subscribe { .. }
        | ClientCommand::Resubscribe { .. }
        | ClientCommand::Poll { .. }
        | ClientCommand::Stats { .. }
        | ClientCommand::Unsubscribe { .. }
        | ClientCommand::Disconnect => return Err(ClientError::Denied),
        _ => {}
    }
    if model.state() != &StreamState::Live {
        return Err(ClientError::Busy);
    }
    match command {
        ClientCommand::HostedStateQuery { epoch } => {
            return if ready.events.host_state && *epoch == ready.epoch {
                Ok(())
            } else {
                Err(ClientError::Denied)
            };
        }
        ClientCommand::HostedSetMode { reference, .. }
        | ClientCommand::HostedViewport { reference, .. } => {
            let granted = match command {
                ClientCommand::HostedSetMode { .. } => ready.controls.host_mode_control,
                _ => ready.controls.host_viewport_control,
            };
            return if ready.events.host_state
                && granted
                && reference.epoch == ready.epoch
                && hosted_receipt.permits(model, *reference)
            {
                Ok(())
            } else {
                Err(ClientError::Denied)
            };
        }
        _ => {}
    }
    let control = &ready.controls;
    if let ClientCommand::RemoteWindowQuery { reference }
    | ClientCommand::RemoteWindowTrack { reference }
    | ClientCommand::RemoteWindowUntrack { reference }
    | ClientCommand::RemoteWindowControl { reference, .. } = command
        && reference.epoch != ready.epoch
    {
        return Err(ClientError::Denied);
    }
    let require_user = |id: &UserId| -> Result<(), ClientError> {
        let view = model.users().get(id).ok_or(ClientError::Denied)?;
        if !selects(&control.users, view) || (!view.owned_by_recipient && !control.delegated_users)
        {
            return Err(ClientError::Denied);
        }
        Ok(())
    };
    match command {
        ClientCommand::Input { user, input }
        | ClientCommand::InputAtRevision { user, input, .. } => {
            require_user(user)?;
            let view = &model.users()[user];
            if !selects(&ready.events.users, view) {
                return Err(ClientError::Denied);
            }
            match input {
                Input::Key { .. } if keys && control.key_input && ready.events.key_events => {}
                Input::Motion { .. } | Input::Button { .. } | Input::Wheel { .. }
                    if pointer && control.pointer_input && ready.events.pointer_events => {}
                _ => return Err(ClientError::Denied),
            }
        }
        ClientCommand::Click { user, .. } => {
            require_user(user)?;
            if !pointer
                || !control.pointer_input
                || !ready.events.pointer_events
                || !selects(&ready.events.users, &model.users()[user])
            {
                return Err(ClientError::Denied);
            }
        }
        ClientCommand::CreateUser { .. } if control.user_lifecycle => {}
        ClientCommand::RenameUser { user, .. } | ClientCommand::DisposeUser { user }
            if control.user_lifecycle =>
        {
            require_user(user)?
        }
        ClientCommand::Cursor { user, .. } if control.cursor => require_user(user)?,
        ClientCommand::WindowQuery { .. } | ClientCommand::RemoteWindowQuery { .. }
            if ready.events.owned_windows => {}
        ClientCommand::WindowTrack { .. }
        | ClientCommand::WindowUntrack { .. }
        | ClientCommand::RemoteWindowTrack { .. }
        | ClientCommand::RemoteWindowUntrack { .. }
            if control.window_tracking => {}
        ClientCommand::WindowControl { .. } | ClientCommand::RemoteWindowControl { .. }
            if control.window_control => {}
        _ => return Err(ClientError::Denied),
    }
    Ok(())
}
fn response_matches(command: &ClientCommand, response: &ClientResponse) -> bool {
    match (command, response) {
        (ClientCommand::HostedStateQuery { .. }, ClientResponse::HostedState { state }) => {
            return state.revision != 0;
        }
        (
            ClientCommand::HostedSetMode { reference, mode },
            ClientResponse::HostedAction { action, state },
        ) => {
            return *action == HostedAction::SetMode(*mode)
                && state.revision >= reference.revision
                && state.mode == Some(*mode)
                && state.phase
                    == if *mode == HostedMode::Native {
                        HostedPhase::Native
                    } else {
                        HostedPhase::Ready
                    };
        }
        (
            ClientCommand::HostedViewport {
                reference,
                action: requested,
            },
            ClientResponse::HostedAction { action, state },
        ) => {
            return *action == HostedAction::Viewport(*requested)
                && state.revision >= reference.revision
                && state.minimized == Some(matches!(requested, HostViewportAction::Minimize));
        }
        (
            ClientCommand::HostedStateQuery { .. }
            | ClientCommand::HostedSetMode { .. }
            | ClientCommand::HostedViewport { .. },
            _,
        ) => return false,
        _ => {}
    }
    matches!(
        (command, response),
        (
            ClientCommand::Subscribe { .. } | ClientCommand::Resubscribe { .. },
            ClientResponse::Subscribed { .. }
        ) | (
            ClientCommand::CreateUser { .. },
            ClientResponse::UserCreated { .. }
        ) | (ClientCommand::Cursor { .. }, ClientResponse::Cursor { .. })
            | (
                ClientCommand::Input { .. }
                    | ClientCommand::InputAtRevision { .. }
                    | ClientCommand::Click { .. },
                ClientResponse::InputAccepted { .. }
            )
            | (
                ClientCommand::WindowQuery { .. }
                    | ClientCommand::WindowTrack { .. }
                    | ClientCommand::RemoteWindowQuery { .. }
                    | ClientCommand::RemoteWindowTrack { .. },
                ClientResponse::Window { .. }
            )
            | (
                ClientCommand::WindowControl { .. } | ClientCommand::RemoteWindowControl { .. },
                ClientResponse::WindowControlled { .. }
            )
            | (
                ClientCommand::RenameUser { .. }
                    | ClientCommand::DisposeUser { .. }
                    | ClientCommand::WindowUntrack { .. }
                    | ClientCommand::RemoteWindowUntrack { .. },
                ClientResponse::Acknowledged
            )
    )
}

fn valid_scope(scope: &UserScope, epoch: u64) -> bool {
    match scope {
        UserScope::AllOwned => true,
        UserScope::Only(users) => {
            users.len() <= MAX_USERS && users.iter().all(|u| u.epoch == epoch && u.local != 0)
        }
    }
}

#[cfg(test)]
mod hosted_consumer_tests {
    use super::*;

    #[test]
    fn native_host_action_uses_only_a_validated_current_receipt_or_stream_revision() {
        let bounds = mux_core::desktop::Bounds {
            left: 0,
            top: 0,
            right: 640,
            bottom: 480,
        };
        let first = HostedState {
            revision: 1,
            routing_generation: 0,
            phase: HostedPhase::Native,
            mode: Some(HostedMode::Native),
            multi_keyboard: None,
            minimized: Some(false),
        };
        let mut model = EventClientState::default();
        model
            .install_snapshot(
                Snapshot {
                    hosted: Some(first),
                    version: PROTOCOL_VERSION,
                    epoch: 71,
                    through_sequence: 1,
                    users: Vec::new(),
                    screens: ScreenState {
                        revision: 1,
                        desktop: bounds,
                        screens: vec![bounds],
                    },
                    windows: Vec::new(),
                    capabilities: Vec::new(),
                    mode: ApplicationMode::Cooperative,
                },
                71,
            )
            .unwrap();
        let ready = EventReady {
            version: PROTOCOL_VERSION,
            epoch: 71,
            recipient: serde_json::from_str("1").unwrap(),
            events: Grant {
                host_state: true,
                ..Default::default()
            },
            controls: ControlGrant {
                host_mode_control: true,
                ..Default::default()
            },
            capabilities: Vec::new(),
            limits: EventLimits::default(),
        };
        let mut receipt = crate::client::HostedReceipt::default();
        let observed = HostedState {
            revision: 2,
            routing_generation: 1,
            phase: HostedPhase::Ready,
            mode: Some(HostedMode::Switched),
            multi_keyboard: Some(false),
            minimized: Some(false),
        };
        let action = |epoch, revision| ClientCommand::HostedSetMode {
            reference: HostRevision { epoch, revision },
            mode: HostedMode::Concurrent,
        };
        let pending = VecDeque::new();
        assert!(
            authorize(
                &action(71, 2),
                &model,
                &receipt,
                &ready,
                None,
                &pending,
                (false, false)
            )
            .is_err()
        );
        receipt.observe(71, &observed, &model).unwrap();
        assert!(
            authorize(
                &action(71, 2),
                &model,
                &receipt,
                &ready,
                None,
                &pending,
                (false, false)
            )
            .is_ok()
        );
        for (epoch, revision) in [(71, 1), (71, 3), (72, 2)] {
            assert!(
                authorize(
                    &action(epoch, revision),
                    &model,
                    &receipt,
                    &ready,
                    None,
                    &pending,
                    (false, false)
                )
                .is_err()
            );
        }
        let mut denied = ready.clone();
        denied.controls.host_mode_control = false;
        assert!(
            authorize(
                &action(71, 2),
                &model,
                &receipt,
                &denied,
                None,
                &pending,
                (false, false)
            )
            .is_err()
        );
        model.disconnect();
        receipt.reconcile(&model).unwrap();
        assert!(
            authorize(
                &action(71, 2),
                &model,
                &receipt,
                &ready,
                None,
                &pending,
                (false, false)
            )
            .is_err()
        );
    }

    #[test]
    fn typed_hosted_receipts_require_matching_action_and_new_revision() {
        let reference = HostRevision {
            epoch: u64::MAX,
            revision: 9,
        };
        let command = ClientCommand::HostedSetMode {
            reference,
            mode: HostedMode::Concurrent,
        };
        let state: HostedState = serde_json::from_str(r#"{"revision":10,"routing_generation":4,"phase":"ready","mode":"concurrent","multi_keyboard":true,"minimized":false}"#).unwrap();
        assert!(response_matches(
            &command,
            &ClientResponse::HostedAction {
                action: HostedAction::SetMode(HostedMode::Concurrent),
                state: state.clone()
            }
        ));
        assert!(!response_matches(
            &command,
            &ClientResponse::HostedAction {
                action: HostedAction::SetMode(HostedMode::Native),
                state: state.clone()
            }
        ));
        let mut stale = state;
        stale.revision = reference.revision - 1;
        assert!(!response_matches(
            &command,
            &ClientResponse::HostedAction {
                action: HostedAction::SetMode(HostedMode::Concurrent),
                state: stale
            }
        ));
        assert!(!response_matches(&command, &ClientResponse::Acknowledged));
    }
}
