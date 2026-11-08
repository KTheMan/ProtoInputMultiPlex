//! External event client: consumes the GUI server's Engine; never constructs Host.
//! All methods and model state belong to the creating UI thread. No reconnect or replay.
mod network;
mod wake;
mod completion;
mod ffi;
#[cfg(test)] mod tests;
use crate::*;
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, marker::PhantomData, rc::Rc, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}, thread::JoinHandle};
use tokio::sync::Notify;

pub const MAX_NATIVE_CLIENTS: usize = 16;
static ACTIVE_WORKERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
struct WorkerPermit;
impl WorkerPermit {
    fn acquire() -> Result<Self, ClientError> {
        ACTIVE_WORKERS.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < MAX_NATIVE_CLIENTS).then_some(n + 1))
            .map(|_| Self).map_err(|_| ClientError::Limit)
    }
}
impl Drop for WorkerPermit { fn drop(&mut self) { ACTIVE_WORKERS.fetch_sub(1, Ordering::AcqRel); } }
pub const MAX_PENDING: usize = 32;
const MAX_INCOMING: usize = 64;
const MAX_QUEUED_BYTES: usize = 4 * 1024 * 1024;

/// Explicit transient connection setup. Never log/serialize this structure.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectConfig { pub endpoint: String, pub exact_origin: String, pub event_credential: String }
impl std::fmt::Debug for ConnectConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("ConnectConfig([private])") }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason { LocalStop, Transport, Protocol, QueueOverflow, WindowLost, DeadlineUncertain, WorkerPanic, RemoteEnd }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientError { Invalid, Busy, Limit, Window, Closed, Protocol, Denied }
impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{self:?}") }
}
impl std::error::Error for ClientError {}

struct Mailbox {
    outgoing: VecDeque<ClientMessage>, incoming: VecDeque<(usize, ServerMessage)>, bytes: usize, signalled: bool,
    terminal: Option<StopReason>,
}
struct Shared { mailbox: Mutex<Mailbox>, stop: AtomicBool, changed: Notify, requests_changed: Notify, wake: wake::Target }
impl Shared {
    fn enqueue(&self, message: ServerMessage, bytes: usize) -> bool {
        let mut q = self.mailbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.stop.load(Ordering::Acquire) { return false; }
        if q.incoming.len() >= MAX_INCOMING || q.bytes.saturating_add(bytes) > MAX_QUEUED_BYTES {
            drop(q); self.finish(StopReason::QueueOverflow); return false;
        }
        q.bytes += bytes; q.incoming.push_back((bytes, message));
        if !q.signalled {
            q.signalled = self.wake.post();
            if !q.signalled { drop(q); self.finish(StopReason::WindowLost); return false; }
        }
        true
    }
    fn finish(&self, reason: StopReason) {
        let mut q = self.mailbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.stop.store(true, Ordering::Release);
        q.incoming.clear(); q.outgoing.clear(); q.bytes = 0;
        if q.terminal.is_none() { q.terminal = Some(reason); }
        if !q.signalled { q.signalled = self.wake.post(); }
        self.changed.notify_one();
    }
}

#[derive(Serialize)]
pub struct NativeStatus {
    pub connecting: bool, pub live: bool, pub needs_snapshot: bool,
    pub pointer_enabled: bool, pub key_enabled: bool, pub pending_requests: usize,
    pub local_worker_retired: bool,
    /// EOF/retirement is not evidence that the GUI owner has reconciled producer holds.
    pub remote_cleanup_unconfirmed: bool,
    pub uncertain_request_ids: Vec<u64>, pub terminal: Option<StopReason>,
    pub wake_message: u32,
}
#[derive(Serialize)]
pub struct ModelView<'a> {
    pub epoch: Option<u64>, pub sequence: u64,
    pub users: Vec<&'a UserView>,
    pub screens: Option<&'a ScreenState>, pub windows: Vec<&'a WindowInfo>,
    pub ready: Option<&'a EventReady>, pub subscription: Option<SubscriptionId>, pub status: NativeStatus,
}

pub struct NativeClient {
    shared: Arc<Shared>, worker: Option<JoinHandle<()>>,
    binding: wake::Binding, model: EventClientState, ready: Option<EventReady>,
    subscription: Option<SubscriptionId>, pending: VecDeque<(u64, ClientCommand)>,
    next_id: u64, pointer: bool, keys: bool, terminal: Option<StopReason>, uncertain: Vec<u64>,
    _local: PhantomData<Rc<()>>,
}
impl NativeClient {
    /// Registers an own top-level HWND on this thread, then starts one bounded I/O worker.
    /// Call stop from WM_NCDESTROY before returning. Drain/query only on this thread.
    pub fn connect(config: ConnectConfig, own_hwnd: usize) -> Result<Self, ClientError> {
        let address = network::validate_config(&config)?;
        let binding = wake::Binding::new(own_hwnd)?;
        let shared = Arc::new(Shared { mailbox: Mutex::new(Mailbox { outgoing: VecDeque::new(), incoming: VecDeque::new(), bytes: 0, signalled: false, terminal: None }),
            stop: AtomicBool::new(false), changed: Notify::new(), requests_changed: Notify::new(), wake: binding.target.clone() });
        let worker_shared = Arc::clone(&shared);
        let permit = WorkerPermit::acquire()?;
        let worker = std::thread::Builder::new().name("application-event-client".into()).spawn(move || {
            let _permit = permit;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(runtime) => runtime.block_on(network::run(config, address, Arc::clone(&worker_shared))),
                    Err(_) => worker_shared.finish(StopReason::Transport),
                }
            }));
            if result.is_err() { worker_shared.finish(StopReason::WorkerPanic); }
        }).map_err(|_| ClientError::Limit)?;
        Ok(Self { shared, worker: Some(worker), binding, model: EventClientState::default(), ready: None,
            subscription: None, pending: VecDeque::new(), next_id: 1, pointer: false, keys: false,
            terminal: None, uncertain: Vec::new(), _local: PhantomData })
    }
    fn observe_terminal(&mut self) {
        let reason = self.shared.mailbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner).terminal;
        if let Some(reason) = reason { self.clear(reason); }
    }
    fn clear(&mut self, reason: StopReason) {
        if self.terminal.is_none() {
            self.uncertain = self.pending.iter().map(|(id, _)| *id).collect();
        }
        self.pending.clear(); self.subscription = None; self.ready = None;
        self.pointer = false; self.keys = false; self.model.disconnect(); self.terminal = Some(reason);
    }
    pub fn stop(&mut self) { self.shared.finish(StopReason::LocalStop); self.observe_terminal(); }
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
        if !self.retired() { return false; }
        if let Some(worker) = self.worker.take() { let _ = worker.join(); }
        true
    }
    pub fn set_input_enabled(&mut self, pointer: bool, keys: bool) -> Result<(), ClientError> {
        self.observe_terminal();
        if self.terminal.is_some() { return Err(ClientError::Closed); }
        // Disabling an enabled producer fences the connection; server EOF cleanup
        // reconciles its holds. This never guesses which producer owns a held key.
        if (self.pointer && !pointer) || (self.keys && !keys) { self.stop(); return Ok(()); }
        self.pointer = pointer; self.keys = keys; Ok(())
    }
    pub fn request(&mut self, request: ClientRequest) -> Result<u64, ClientError> {
        self.observe_terminal();
        if self.terminal.is_some() { return Err(ClientError::Closed); }
        if request.version != PROTOCOL_VERSION { return Err(ClientError::Invalid); }
        let ready = self.ready.as_ref().ok_or(ClientError::Busy)?;
        authorize(&request.command, &self.model, ready, self.subscription, &self.pending, (self.pointer, self.keys))?;
        if self.pending.len() >= MAX_PENDING || self.next_id > 65_536 { return Err(ClientError::Limit); }
        let id = self.next_id;
        let command = request.command.clone();
        let message = ClientMessage { request_id: id, request };
        if serde_json::to_vec(&message).map_err(|_| ClientError::Invalid)?.len() > ready.limits.max_request_bytes { return Err(ClientError::Limit); }
        {
            let mut q = self.shared.mailbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.shared.stop.load(Ordering::Acquire) { return Err(ClientError::Closed); }
            if q.outgoing.len() >= MAX_PENDING { return Err(ClientError::Busy); }
            q.outgoing.push_back(message);
        }
        self.shared.requests_changed.notify_one();
        self.pending.push_back((id, command)); self.next_id += 1; Ok(id)
    }
    /// Acknowledge every wake by draining until None. Each pop re-arms while data
    /// remains under the same queue mutex, so arrival vs acknowledge cannot lose a wake.
    /// Returned messages are authenticated and folded before caller callbacks/rendering.
    pub fn drain(&mut self) -> Result<Option<ServerMessage>, ClientError> {
        self.observe_terminal();
        if self.terminal.is_some() { return Ok(None); }
        let item = {
            let mut q = self.shared.mailbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            q.signalled = false;
            let item = q.incoming.pop_front();
            if let Some((bytes, _)) = &item { q.bytes -= bytes; }
            if !q.incoming.is_empty() { q.signalled = self.shared.wake.post();
                if !q.signalled { drop(q); self.shared.finish(StopReason::WindowLost); self.observe_terminal(); return Err(ClientError::Window); }
            }
            item.map(|(_, message)| message)
        };
        if let Some(message) = &item && self.fold(message).is_err() {
            self.shared.finish(StopReason::Protocol); self.observe_terminal(); return Err(ClientError::Protocol);
        }
        Ok(item)
    }
    fn fold(&mut self, message: &ServerMessage) -> Result<(), ClientError> {
        match message {
            ServerMessage::Ready { ready } => {
                if self.ready.is_some() || ready.version != PROTOCOL_VERSION || ready.epoch == 0
                    || ready.limits.max_request_bytes == 0 || ready.limits.max_request_bytes > MAX_REQUEST_BYTES
                    || ready.limits.max_poll_events == 0 || ready.limits.max_poll_events > MAX_POLL_EVENTS
                    || ready.limits.max_queue == 0 || ready.limits.max_queue > MAX_QUEUE
                    || !ready.events.enabled() || !valid_scope(&ready.events.users, ready.epoch) || !valid_scope(&ready.controls.users, ready.epoch)
                    || ready.capabilities.len() > 32 || ready.capabilities.iter().any(|c| c.detail.len() > 512) { return Err(ClientError::Protocol); }
                self.ready = Some(ready.clone());
            }
            ServerMessage::Response { request_id, reply } => {
                let (id, command) = self.pending.front().cloned().ok_or(ClientError::Protocol)?;
                if id != *request_id { return Err(ClientError::Protocol); }
                match reply {
                    Reply::Ok { version, response } => {
                        if *version != PROTOCOL_VERSION || !response_matches(&command, response) { return Err(ClientError::Protocol); }
                        if let ClientResponse::Subscribed { subscription, snapshot } = response {
                            let epoch = self.ready.as_ref().ok_or(ClientError::Protocol)?.epoch;
                            self.model.install_snapshot(snapshot.clone(), epoch).map_err(|_| ClientError::Protocol)?;
                            self.subscription = Some(*subscription);
                        }
                    }
                    Reply::Error { version, .. } if *version == PROTOCOL_VERSION => {},
                    _ => return Err(ClientError::Protocol),
                }
                self.pending.pop_front();
            }
            ServerMessage::EventStream { subscription, stream } => {
                if self.subscription != Some(*subscription) { return Err(ClientError::Protocol); }
                // Refresh response orders the old/new token boundary. Queued
                // old-token loss can still raise the snapshot floor, but cannot
                // restore old user/input state while that response is in flight.
                let refreshing = self.pending.iter().any(|(_, command)| matches!(command,
                    ClientCommand::Resubscribe { subscription: old, .. } if old == subscription));
                if refreshing && matches!(stream, Poll::Events { .. }) { return Ok(()); }
                self.model.fold_poll(stream.clone()).map_err(|_| ClientError::Protocol)?;
                if matches!(self.model.state(), StreamState::Ended(_)) {
                    self.shared.finish(StopReason::RemoteEnd); self.observe_terminal();
                }
            }
            ServerMessage::Disconnected { .. } => { self.shared.finish(StopReason::RemoteEnd); self.observe_terminal(); }
        }
        Ok(())
    }
    pub fn query(&mut self) -> ModelView<'_> {
        self.observe_terminal();
        let status = NativeStatus { connecting: self.ready.is_none() && self.terminal.is_none(), live: self.model.state() == &StreamState::Live,
            needs_snapshot: self.model.state() == &StreamState::NeedsSnapshot, pointer_enabled: self.pointer, key_enabled: self.keys,
            pending_requests: self.pending.len(), local_worker_retired: self.retired(), remote_cleanup_unconfirmed: self.terminal.is_some(),
            uncertain_request_ids: self.uncertain.clone(), terminal: self.terminal, wake_message: self.binding.target.message };
        ModelView { epoch: self.model.epoch(), sequence: self.model.last_sequence(), users: self.model.users().values().collect(), screens: self.model.screens(),
            windows: self.model.windows().values().collect(), ready: self.ready.as_ref(), subscription: self.subscription, status }
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
    match scope { UserScope::AllOwned => view.owned_by_recipient, UserScope::Only(users) => users.contains(&view.id) }
}
fn authorize(command: &ClientCommand, model: &EventClientState, ready: &EventReady, subscription: Option<SubscriptionId>, pending: &VecDeque<(u64, ClientCommand)>, enables: (bool, bool)) -> Result<(), ClientError> {
    let (pointer, keys) = enables;
    match command {
        ClientCommand::Subscribe { capacity } if subscription.is_none() && pending.is_empty() && (1..=MAX_QUEUE).contains(capacity) => return Ok(()),
        ClientCommand::Resubscribe { subscription: token, capacity } if subscription == Some(*token) && pending.is_empty()
            && model.state() == &StreamState::NeedsSnapshot && (1..=MAX_QUEUE).contains(capacity) => return Ok(()),
        ClientCommand::Snapshot | ClientCommand::Subscribe { .. } | ClientCommand::Resubscribe { .. } | ClientCommand::Poll { .. }
        | ClientCommand::Stats { .. } | ClientCommand::Unsubscribe { .. } | ClientCommand::Disconnect => return Err(ClientError::Denied),
        _ => {},
    }
    if model.state() != &StreamState::Live { return Err(ClientError::Busy); }
    let control = &ready.controls;
    let require_user = |id: &UserId| -> Result<(), ClientError> {
        let view = model.users().get(id).ok_or(ClientError::Denied)?;
        if !selects(&control.users, view) || (!view.owned_by_recipient && !control.delegated_users) { return Err(ClientError::Denied); }
        Ok(())
    };
    match command {
        ClientCommand::Input { user, input } | ClientCommand::InputAtRevision { user, input, .. } => {
            require_user(user)?;
            let view = &model.users()[user];
            if !selects(&ready.events.users, view) { return Err(ClientError::Denied); }
            match input {
                Input::Key { .. } if keys && control.key_input && ready.events.key_events => {},
                Input::Motion { .. } | Input::Button { .. } | Input::Wheel { .. } if pointer && control.pointer_input && ready.events.pointer_events => {},
                _ => return Err(ClientError::Denied),
            }
        }
        ClientCommand::Click { user, .. } => {
            require_user(user)?;
            if !pointer || !control.pointer_input || !ready.events.pointer_events || !selects(&ready.events.users, &model.users()[user]) { return Err(ClientError::Denied); }
        }
        ClientCommand::CreateUser { .. } if control.user_lifecycle => {},
        ClientCommand::RenameUser { user, .. } | ClientCommand::DisposeUser { user } if control.user_lifecycle => require_user(user)?,
        ClientCommand::Cursor { user, .. } if control.cursor => require_user(user)?,
        ClientCommand::WindowQuery { .. } if ready.events.owned_windows => {},
        ClientCommand::WindowTrack { .. } | ClientCommand::WindowUntrack { .. } if control.window_tracking => {},
        ClientCommand::WindowControl { .. } if control.window_control => {},
        _ => return Err(ClientError::Denied),
    }
    Ok(())
}
fn response_matches(command: &ClientCommand, response: &ClientResponse) -> bool {
    matches!((command, response),
        (ClientCommand::Subscribe { .. } | ClientCommand::Resubscribe { .. }, ClientResponse::Subscribed { .. }) |
        (ClientCommand::CreateUser { .. }, ClientResponse::UserCreated { .. }) |
        (ClientCommand::Cursor { .. }, ClientResponse::Cursor { .. }) |
        (ClientCommand::Input { .. } | ClientCommand::InputAtRevision { .. } | ClientCommand::Click { .. }, ClientResponse::InputAccepted { .. }) |
        (ClientCommand::WindowQuery { .. } | ClientCommand::WindowTrack { .. }, ClientResponse::Window { .. }) |
        (ClientCommand::WindowControl { .. }, ClientResponse::WindowControlled { .. }) |
        (ClientCommand::RenameUser { .. } | ClientCommand::DisposeUser { .. } | ClientCommand::WindowUntrack { .. }, ClientResponse::Acknowledged))
}

fn valid_scope(scope: &UserScope, epoch: u64) -> bool {
    match scope { UserScope::AllOwned => true, UserScope::Only(users) => users.len() <= MAX_USERS && users.iter().all(|u| u.epoch == epoch && u.local != 0) }
}
