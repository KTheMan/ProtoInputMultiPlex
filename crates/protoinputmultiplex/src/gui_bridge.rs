//! Authenticated local named-pipe bridge for a running GUI-owned session.
//!
//! The descriptor contains a bearer token and must stay in a user-private location. The
//! broker creates both it and its pipe with a protected DACL for the current logon SID.
#[cfg(test)]
#[path = "gui_application_event_projection_tests.rs"]
mod application_event_projection_tests;

use crate::{agent_runtime, session};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
    },
    Security::Authorization::ConvertSidToStringSidW,
    Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom},
    Security::{GetTokenInformation, TOKEN_GROUPS, TOKEN_QUERY},
    Storage::FileSystem::{CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, PIPE_ACCESS_DUPLEX},
    System::{
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_TYPE_BYTE, PeekNamedPipe, SetNamedPipeHandleState,
        },
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

pub type Reply = agent_runtime::State;

#[derive(Clone)]
pub struct AppHandle {
    activity: Option<crate::gui_activity::Observer>,
    sender: Sender<AppRequest>,
    discovery: Arc<RwLock<(crate::gui_discovery::Status, Instant)>>,
    application_events: Arc<RwLock<Option<(crate::gui_application_events::PublicStatus, Instant)>>>,
    controller: u64,
    incarnation: u64,
    recovery: Arc<RwLock<RecoveryPublication>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BridgeMode {
    Live,
    Reconfiguring,
    Stopping,
    Terminal,
    Revoked,
}

struct ConnectionAuthority {
    certified: BTreeSet<(u64, u64)>,
}

struct RecoveryPublication {
    controller: u64,
    incarnation: u64,
    revision: u64,
    mode: BridgeMode,
    policy_generation: u64,
    policy: agent_runtime::Policy,
    active_users: Vec<String>,
    connections: BTreeMap<u64, ConnectionAuthority>,
    pending_app: BTreeMap<u64, Weak<crate::app_lifecycle::CommitDecision>>,
    sample: Option<(crate::session_recovery::Disclosure, Instant)>,
}

impl RecoveryPublication {
    fn new(controller: u64, incarnation: u64) -> Self {
        Self {
            controller,
            incarnation,
            revision: 0,
            mode: BridgeMode::Live,
            policy_generation: 0,
            policy: Default::default(),
            active_users: Vec::new(),
            connections: BTreeMap::new(),
            pending_app: BTreeMap::new(),
            sample: None,
        }
    }

    fn invalidate_sample(&mut self) {
        self.revision = self
            .revision
            .checked_add(1)
            .expect("publication version exhausted");
        self.sample = None;
    }

    fn track_app(&mut self, client: u64, decision: &Arc<crate::app_lifecycle::CommitDecision>) {
        if let Some(previous) = self.pending_app.insert(client, Arc::downgrade(decision))
            && let Some(previous) = previous.upgrade()
        {
            previous.cancel();
        }
    }

    fn close_commands(&mut self) {
        for (_, cancelled) in std::mem::take(&mut self.pending_app) {
            if let Some(cancelled) = cancelled.upgrade() {
                cancelled.cancel();
            }
        }
    }

    fn revoke(&mut self) {
        self.close_commands();
        self.mode = BridgeMode::Revoked;
        self.connections.clear();
        self.policy = Default::default();
        self.invalidate_sample();
    }

    fn projection(
        &mut self,
        client: u64,
        certify: bool,
    ) -> Option<crate::session_recovery::Projection> {
        let connection = self.connections.get_mut(&client)?;
        if !self.policy.enabled || self.mode == BridgeMode::Revoked {
            return None;
        }
        let (sample, when) = self.sample.as_ref()?;
        if when.elapsed() > Duration::from_secs(2) {
            return None;
        }
        let mut projection = sample.projection.clone();
        let all_users = !self.active_users.is_empty()
            && self
                .active_users
                .iter()
                .all(|id| self.policy.user_ids.contains(id));
        if !all_users {
            projection.targets.clear();
            return Some(projection);
        }
        // Only a live Inspect can certify an affected original. The sample was
        // polled after this connection and acknowledged policy installed a new
        // publication revision. Terminal inspection can only retain that proof.
        if certify && self.mode == BridgeMode::Live {
            for row in &sample.originals {
                if row.known_live && self.policy.allowed_pids.contains(&row.pid) {
                    connection.certified.insert((sample.generation, row.token));
                }
            }
        }
        projection.targets.retain(|target| {
            self.policy.allowed_pids.contains(&target.pid)
                && sample.originals.iter().any(|original| {
                    original.pid == target.pid
                        && connection
                            .certified
                            .contains(&(sample.generation, original.token))
                })
        });
        Some(projection)
    }
}

pub struct AppRequest {
    pub client: u64,
    pub sequence: u64,
    pub authority: (u64, u64, u64),
    pub command: agent_runtime::AppPreferencesCommand,
    pub connection_alive: Arc<AtomicBool>,
    pub decision: Arc<crate::app_lifecycle::CommitDecision>,
    reply: SyncSender<agent_runtime::AppPreferencesState>,
}

impl AppRequest {
    pub fn reply(self, state: agent_runtime::AppPreferencesState) {
        let _ = self.reply.try_send(state);
    }
}

pub fn app_channel() -> (AppHandle, Receiver<AppRequest>) {
    let (sender, receiver) = mpsc::channel();
    (
        AppHandle {
            activity: None,
            sender,
            controller: 0,
            incarnation: 0,
            application_events: Arc::new(RwLock::new(None)),
            recovery: Arc::new(RwLock::new(RecoveryPublication::new(0, 0))),
            discovery: Arc::new(RwLock::new((
                crate::gui_discovery::Status::default(),
                Instant::now(),
            ))),
        },
        receiver,
    )
}

impl AppHandle {
    pub(crate) fn publish_application_events(
        &self,
        status: crate::gui_application_events::PublicStatus,
    ) {
        *self
            .application_events
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((status, Instant::now()));
    }

    fn application_event_status(&self) -> Option<crate::gui_application_events::PublicStatus> {
        let publication = self.application_events.try_read().ok()?;
        let (status, sampled) = publication.as_ref()?;
        let age = sampled.elapsed();
        if age > Duration::from_secs(2) {
            return None;
        }
        let mut status = status.clone();
        status.age_by(age);
        Some(status)
    }

    pub fn attach_activity(&mut self, observer: crate::gui_activity::Observer) {
        self.activity = Some(observer);
    }

    pub fn activity_snapshot(&self) -> Option<crate::gui_activity::Snapshot> {
        self.activity
            .as_ref()
            .map(crate::gui_activity::Observer::snapshot)
    }

    pub fn begin_controller(&mut self, controller: u64) {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let incarnation = NEXT
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .expect("bridge incarnation exhausted");
        let mut state = self
            .recovery
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.close_commands();
        *state = RecoveryPublication::new(controller, incarnation);
        self.controller = controller;
        self.incarnation = incarnation;
    }

    pub fn prepare_broker(&mut self) {
        self.begin_controller(self.controller);
    }

    pub fn prepare_policy(&self) {
        let mut state = self
            .recovery
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.controller == self.controller
            && state.incarnation == self.incarnation
            && state.mode == BridgeMode::Live
        {
            state.close_commands();
            state.mode = BridgeMode::Reconfiguring;
            state.connections.clear();
            state.invalidate_sample();
        }
    }

    pub fn observe_roster(&self, roster: &crate::virtual_users::State) {
        let users: Vec<String> = roster
            .slots
            .iter()
            .filter(|slot| slot.active)
            .map(|slot| slot.seat.identity())
            .collect();
        let mut state = self
            .recovery
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.controller == self.controller
            && state.incarnation == self.incarnation
            && state.policy.enabled
            && state.active_users != users
        {
            state.revoke();
        }
    }

    pub fn revoke(&self) {
        let mut state = self
            .recovery
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.controller == self.controller && state.incarnation == self.incarnation {
            state.revoke();
        }
    }

    pub fn begin_stop(&self) {
        let mut state = self
            .recovery
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.controller == self.controller
            && state.incarnation == self.incarnation
            && state.mode == BridgeMode::Live
        {
            state.close_commands();
            state.mode = BridgeMode::Stopping;
            state.invalidate_sample();
        }
    }

    pub fn terminal(&self) {
        self.begin_stop();
        let mut state = self
            .recovery
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.controller == self.controller
            && state.incarnation == self.incarnation
            && state.mode == BridgeMode::Stopping
        {
            state.mode = BridgeMode::Terminal;
        }
    }

    pub fn continuing(&self) -> bool {
        let state = self
            .recovery
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.controller == self.controller
            && state.incarnation == self.incarnation
            && matches!(state.mode, BridgeMode::Stopping | BridgeMode::Terminal)
            && state.policy.enabled
            && !state.connections.is_empty()
    }

    pub fn retirement_due(&self) -> bool {
        let state = self
            .recovery
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.controller == self.controller
            && state.incarnation == self.incarnation
            && (state.mode == BridgeMode::Revoked
                || (state.mode == BridgeMode::Terminal && state.connections.is_empty()))
    }

    pub fn publication_ticket(&self) -> (u64, u64, u64, Instant) {
        let state = self
            .recovery
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (
            self.controller,
            self.incarnation,
            state.revision,
            Instant::now(),
        )
    }

    #[cfg(test)]
    pub(crate) fn publish_recovery(
        &self,
        ticket: (u64, u64, u64, Instant),
        sample: crate::session_recovery::Disclosure,
    ) {
        self.publish_recovery_polled(ticket, sample, ticket.3);
    }

    pub(crate) fn publish_recovery_polled(
        &self,
        ticket: (u64, u64, u64, Instant),
        sample: crate::session_recovery::Disclosure,
        native_poll_started: Instant,
    ) {
        let mut state = self
            .recovery
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if (ticket.0, ticket.1, ticket.2) != (state.controller, state.incarnation, state.revision)
            || state.mode == BridgeMode::Revoked
            || native_poll_started < ticket.3
            || native_poll_started.elapsed() > Duration::from_secs(2)
        {
            return;
        }
        if state
            .sample
            .as_ref()
            .is_some_and(|(old, _)| old.generation != sample.generation)
        {
            for connection in state.connections.values_mut() {
                connection.certified.clear();
            }
        }
        state.sample = Some((sample, native_poll_started));
    }

    pub fn observe_policy(&self, reply: &Reply) {
        let Some(authority) = &reply.authority else {
            return;
        };
        if !reply.acknowledged {
            return;
        }
        let mut state = self
            .recovery
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if (state.controller != self.controller || state.incarnation != self.incarnation)
            || !(state.mode == BridgeMode::Live
                || (state.mode == BridgeMode::Reconfiguring && reply.client == 0))
            || authority.policy_generation < state.policy_generation
        {
            return;
        }
        if authority.policy_generation == state.policy_generation
            && !state.active_users.is_empty()
            && authority.active_user_ids != state.active_users
        {
            state.revoke();
            return;
        }
        if authority.policy_generation != state.policy_generation
            || authority.active_user_ids != state.active_users
        {
            state.close_commands();
            state.connections.clear();
            state.invalidate_sample();
        }
        state.policy_generation = authority.policy_generation;
        state.policy = reply.policy.clone();
        state.active_users = authority.active_user_ids.clone();
        if !state.policy.enabled {
            state.revoke();
        } else {
            state.mode = BridgeMode::Live;
        }
    }

    fn admit(&self, client: u64, reply: &Reply) -> Result<()> {
        self.observe_policy(reply);
        let mut state = self
            .recovery
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ensure!(
            state.controller == self.controller
                && state.incarnation == self.incarnation
                && state.mode == BridgeMode::Live
                && state.policy.enabled
                && reply.error.is_none()
                && reply.clients.contains(&client)
                && reply
                    .authority
                    .as_ref()
                    .is_some_and(|a| a.policy_generation == state.policy_generation),
            "GUI connection admission closed"
        );
        state.connections.insert(
            client,
            ConnectionAuthority {
                certified: BTreeSet::new(),
            },
        );
        state.invalidate_sample();
        Ok(())
    }

    fn remove_connection(&self, client: u64) {
        let mut state = self
            .recovery
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.controller == self.controller && state.incarnation == self.incarnation {
            state.connections.remove(&client);
            if let Some(cancelled) = state.pending_app.remove(&client)
                && let Some(cancelled) = cancelled.upgrade()
            {
                cancelled.cancel();
            }
        }
    }

    pub fn permits_app(&self, client: u64, authority: (u64, u64, u64)) -> bool {
        let state = self
            .recovery
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (state.controller, state.incarnation, state.policy_generation) == authority
            && state.controller == self.controller
            && state.incarnation == self.incarnation
            && state.mode == BridgeMode::Live
            && state.policy.enabled
            && state.connections.contains_key(&client)
    }

    fn forward(&self, client: Option<u64>, send: impl FnOnce()) -> Result<()> {
        let state = self
            .recovery
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ensure!(
            state.controller == self.controller
                && state.incarnation == self.incarnation
                && state.mode == BridgeMode::Live
                && state.policy.enabled
                && client.is_none_or(|client| state.connections.contains_key(&client)),
            "Routing is stopped or this connection was revoked"
        );
        // Only a nonblocking channel insertion belongs inside this boundary.
        send();
        Ok(())
    }

    fn dispatch(
        &self,
        handle: &session::SharedHandle,
        command: agent_runtime::Command,
    ) -> Result<()> {
        let client = if matches!(&command, agent_runtime::Command::Connect { .. }) {
            None
        } else {
            command.tag().map(|(client, _)| client)
        };
        self.forward(client, || handle.request(session::Request::Agent(command)))
    }

    fn stopped(&self) -> bool {
        let state = self
            .recovery
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (state.controller != self.controller || state.incarnation != self.incarnation)
            || state.mode != BridgeMode::Live
    }

    fn public_reply(
        &self,
        mut reply: Reply,
        client: u64,
        sequence: u64,
        inspect: bool,
    ) -> Result<Reply> {
        self.observe_policy(&reply);
        let mut state = self
            .recovery
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ensure!(
            state.controller == self.controller
                && state.incarnation == self.incarnation
                && state.mode != BridgeMode::Revoked
                && state.connections.contains_key(&client),
            "Agent connection was revoked"
        );
        let terminal = state.mode != BridgeMode::Live;
        let projection = state.projection(client, inspect && reply.error.is_none());
        if terminal {
            return Ok(Reply {
                acknowledged: true,
                client,
                sequence,
                terminal: true,
                recovery: projection,
                error: (!inspect)
                    .then(|| "Routing stopped; only recovery inspection is available".into()),
                ..Default::default()
            });
        }
        ensure!(
            reply
                .authority
                .as_ref()
                .is_some_and(|authority| authority.policy_generation == state.policy_generation)
                && reply.clients.contains(&client),
            "Agent connection was revoked"
        );
        reply.authority = None;
        reply.recovery = projection;
        // This branch follows live connection/policy validation above. No event
        // credential, user identities or input payloads enter this projection.
        reply.application_events = self.application_event_status().map(Box::new);
        Ok(reply)
    }
    pub fn publish_discovery(&self, status: crate::gui_discovery::Status) {
        *self
            .discovery
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = (status, Instant::now());
    }

    fn discovery_status(&self) -> crate::gui_discovery::Status {
        let (status, published_at) = self
            .discovery
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut status = age_discovery_status(status, published_at, Instant::now());
        // Inventory may have been published long ago; callback metadata is read now.
        status.gui_activity = self.activity_snapshot();
        status
    }

    fn execute(
        &self,
        client: u64,
        sequence: u64,
        command: agent_runtime::AppPreferencesCommand,
        connection_alive: Arc<AtomicBool>,
        stop: &AtomicBool,
    ) -> Result<agent_runtime::AppPreferencesState> {
        ensure!(
            connection_alive.load(Ordering::Acquire),
            "GUI client disconnected"
        );
        ensure!(!stop.load(Ordering::Acquire), "GUI bridge is stopping");
        let (reply, receiver) = mpsc::sync_channel(1);
        let cancelled = Arc::new(crate::app_lifecycle::CommitDecision::default());
        let mut publication = self
            .recovery
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ensure!(
            publication.controller == self.controller
                && publication.incarnation == self.incarnation
                && publication.mode == BridgeMode::Live
                && publication.connections.contains_key(&client),
            "App preferences are unavailable during recovery inspection"
        );
        publication.track_app(client, &cancelled);
        self.sender
            .send(AppRequest {
                client,
                sequence,
                authority: (
                    publication.controller,
                    publication.incarnation,
                    publication.policy_generation,
                ),
                command,
                connection_alive: connection_alive.clone(),
                decision: cancelled.clone(),
                reply,
            })
            .context("queue app-preference request for GUI thread")?;
        drop(publication);
        let deadline = Instant::now() + IO_TIMEOUT;
        loop {
            if self.stopped() {
                cancelled.cancel();
                bail!("App preferences are unavailable during recovery inspection");
            }
            if !connection_alive.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
                cancelled.cancel();
                bail!(if stop.load(Ordering::Acquire) {
                    "GUI bridge is stopping"
                } else {
                    "GUI client disconnected"
                });
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                cancelled.cancel();
                bail!("GUI app-preference request timed out");
            }
            match receiver.recv_timeout(remaining.min(POLL)) {
                Ok(state) => return Ok(state),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    cancelled.cancel();
                    bail!("GUI app-preference request was abandoned")
                }
            }
        }
    }
}

fn age_discovery_status(
    mut status: crate::gui_discovery::Status,
    published_at: Instant,
    now: Instant,
) -> crate::gui_discovery::Status {
    let age_increment = now
        .saturating_duration_since(published_at)
        .as_millis()
        .min(u64::MAX as u128) as u64;
    status.sample_age_ms = status.sample_age_ms.saturating_add(age_increment);
    status.age_ms = status.age_ms.map(|age| age.saturating_add(age_increment));
    if let Some(activity) = &mut status.gui_activity {
        activity.age_by(now.saturating_duration_since(published_at));
    }
    status
}

/// Windows File::read normalizes ERROR_NO_DATA to EOF. Probe actual pipe
/// availability first so an idle NOWAIT connection retains its input owners.
struct Pipe(File);
impl Pipe {
    fn available(&self) -> std::io::Result<u32> {
        let mut available = 0;
        if unsafe {
            PeekNamedPipe(
                self.0.as_raw_handle() as HANDLE,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(available)
    }
}
impl AsRawHandle for Pipe {
    fn as_raw_handle(&self) -> std::os::windows::io::RawHandle {
        self.0.as_raw_handle()
    }
}
impl Read for Pipe {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let available = self.available()? as usize;
        if available == 0 {
            return Err(std::io::ErrorKind::WouldBlock.into());
        }
        let length = available.min(bytes.len());
        self.0.read(&mut bytes[..length])
    }
}
impl Write for Pipe {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.available()?;
        self.0.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 8;
const IO_TIMEOUT: Duration = Duration::from_secs(50);
const POLL: Duration = Duration::from_millis(10);
const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_PIPE_BUSY: i32 = 231;
const ERROR_NO_DATA: i32 = 232;
const ERROR_PIPE_CONNECTED: u32 = 535;
const ERROR_PIPE_LISTENING: u32 = 536;
const PIPE_READMODE_BYTE: u32 = 0;
const PIPE_NOWAIT: u32 = 1;
const PIPE_UNLIMITED_INSTANCES: u32 = 255;

#[derive(Serialize, Deserialize)]
struct Descriptor {
    pipe: String,
    token: String,
    controller_pid: u32,
    #[serde(default)]
    kind: DescriptorKind,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DescriptorKind {
    #[default]
    Live,
    StoppedSetup,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ClientFrame {
    Connect {
        token: String,
    },
    Inspect {
        sequence: u64,
    },
    Execute {
        sequence: u64,
        request: Box<session::Request>,
    },
    Capture {
        sequence: u64,
        user: usize,
        #[serde(default)]
        query: Option<crate::screen_analysis::Query>,
    },
    AppPreferences {
        sequence: u64,
        command: agent_runtime::AppPreferencesCommand,
    },
    Setup {
        sequence: u64,
        command: crate::gui_setup::Command,
    },
    Disconnect {
        sequence: u64,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerFrame {
    reply: Reply,
    #[serde(default)]
    app_preferences: Option<agent_runtime::AppPreferencesState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    setup: Option<crate::gui_setup::Reply>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    discovery: Option<crate::gui_discovery::Status>,
}

pub struct Broker {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    descriptor_path: PathBuf,
}

#[derive(Clone)]
enum BrokerTarget {
    Live(session::SharedHandle, AppHandle),
    StoppedSetup(crate::gui_setup::Handle),
}

impl Broker {
    pub fn start_with_app_cancellable(
        handle: session::SharedHandle,
        app_handle: AppHandle,
        descriptor_path: PathBuf,
        current_generation: Arc<std::sync::atomic::AtomicU64>,
        generation: u64,
        stop: Arc<AtomicBool>,
    ) -> Result<Self> {
        Self::start_inner_cancellable(
            BrokerTarget::Live(handle, app_handle),
            descriptor_path,
            Some((current_generation, generation)),
            stop,
        )
    }

    pub fn start_setup_cancellable(
        setup: crate::gui_setup::Handle,
        descriptor_path: PathBuf,
        current_generation: Arc<AtomicU64>,
        generation: u64,
        stop: Arc<AtomicBool>,
    ) -> Result<Self> {
        Self::start_inner_cancellable(
            BrokerTarget::StoppedSetup(setup),
            descriptor_path,
            Some((current_generation, generation)),
            stop,
        )
    }

    fn start_inner_cancellable(
        target: BrokerTarget,
        descriptor_path: PathBuf,
        ticket: Option<(Arc<std::sync::atomic::AtomicU64>, u64)>,
        stop: Arc<AtomicBool>,
    ) -> Result<Self> {
        let current = || {
            !stop.load(Ordering::Acquire)
                && ticket.as_ref().is_none_or(|(current, generation)| {
                    current.load(Ordering::Acquire) == *generation
                })
        };
        ensure!(current(), "Agent Broker start was superseded");
        let token = random_token()?;
        let pipe = format!(
            r"\\.\pipe\ProtoInputMultiPlex.{}.{}",
            std::process::id(),
            random_token()?
        );
        let descriptor = Descriptor {
            pipe: pipe.clone(),
            token: token.clone(),
            controller_pid: std::process::id(),
            kind: match &target {
                BrokerTarget::Live(..) => DescriptorKind::Live,
                BrokerTarget::StoppedSetup(..) => DescriptorKind::StoppedSetup,
            },
        };
        let bytes = serde_json::to_vec(&descriptor)?;
        let first_pipe = unsafe { File::from_raw_handle(create_pipe(&pipe)? as _) };
        ensure!(current(), "Agent Broker start was superseded");
        write_private_descriptor(&descriptor_path, &bytes)?;
        if !current() {
            remove_descriptor_file(&descriptor_path)
                .context("remove superseded Agent Broker descriptor")?;
            anyhow::bail!("Agent Broker start was superseded");
        }
        let thread_stop = stop.clone();
        let thread = match thread::Builder::new()
            .name("gui-agent-pipe".into())
            .spawn(move || serve(target, pipe, token, thread_stop, first_pipe))
        {
            Ok(thread) => thread,
            Err(error) => {
                remove_descriptor_file(&descriptor_path)
                    .context("remove descriptor after Agent Broker thread spawn failure")?;
                return Err(error.into());
            }
        };
        if !current() {
            stop.store(true, Ordering::Release);
            let _ = thread.join();
            remove_descriptor_file(&descriptor_path)
                .context("remove superseded Agent Broker descriptor")?;
            anyhow::bail!("Agent Broker start was superseded");
        }
        Ok(Self {
            stop,
            thread: Some(thread),
            descriptor_path,
        })
    }

    pub fn shutdown(mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().map_err(|payload| {
                anyhow!("Agent Broker thread panicked: {}", panic_text(payload))
            })?;
        }
        remove_descriptor_file(&self.descriptor_path).context("remove Agent Broker descriptor")?;
        Ok(())
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_file(&self.descriptor_path);
    }
}

pub fn remove_descriptor_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("remove private Agent descriptor"),
    }
}

fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic payload".into())
}

pub struct Client {
    discovery_received_at: Option<Instant>,
    pipe: Pipe,
    client: u64,
    next_sequence: u64,
    discovery: Option<crate::gui_discovery::Status>,
    kind: DescriptorKind,
}

impl Client {
    pub fn connect(descriptor_path: impl AsRef<Path>) -> Result<Self> {
        Self::connect_kind(descriptor_path, DescriptorKind::Live)
    }

    pub fn connect_setup(descriptor_path: impl AsRef<Path>) -> Result<Self> {
        Self::connect_kind(descriptor_path, DescriptorKind::StoppedSetup)
    }

    fn connect_kind(descriptor_path: impl AsRef<Path>, kind: DescriptorKind) -> Result<Self> {
        let descriptor = read_descriptor(descriptor_path.as_ref())?;
        ensure!(
            descriptor.kind == kind,
            "GUI descriptor has the wrong connection kind"
        );
        let deadline = Instant::now() + IO_TIMEOUT;
        let pipe = loop {
            match OpenOptions::new()
                .read(true)
                .write(true)
                .open(&descriptor.pipe)
            {
                Ok(pipe) => break pipe,
                Err(error)
                    if matches!(
                        error.raw_os_error(),
                        Some(ERROR_FILE_NOT_FOUND) | Some(ERROR_PIPE_BUSY)
                    ) && Instant::now() < deadline =>
                {
                    thread::sleep(POLL);
                }
                Err(error) => return Err(error).context("connect to GUI session pipe"),
            }
        };
        set_nonblocking(pipe.as_raw_handle() as HANDLE)?;
        let mut pipe = Pipe(pipe);
        write_frame(
            &mut pipe,
            &ClientFrame::Connect {
                token: descriptor.token,
            },
        )?;
        let frame = read_server_frame(&mut pipe)?;
        let reply = frame.reply;
        ensure!(reply.acknowledged, "GUI session rejected the connection");
        ensure!(
            reply.client != 0 && reply.sequence == 0,
            "Invalid GUI connection acknowledgement"
        );
        ensure!(
            reply.error.is_none(),
            "GUI session rejected the connection: {}",
            reply.error.as_deref().unwrap_or("unknown error")
        );
        Ok(Self {
            pipe,
            client: reply.client,
            next_sequence: 1,
            discovery: frame.discovery,
            discovery_received_at: Some(Instant::now()),
            kind,
        })
    }

    pub fn setup(&mut self, command: crate::gui_setup::Command) -> Result<crate::gui_setup::Reply> {
        ensure!(
            self.kind == DescriptorKind::StoppedSetup,
            "Not a stopped setup connection"
        );
        if let crate::gui_setup::Command::Configure { document, .. } = &command {
            ensure!(
                document.len() <= crate::profile_store::MAX_PROFILE_BYTES,
                "Stopped setup profile exceeds the 16 MiB document limit"
            );
        }
        let sequence = self.next_sequence;
        let frame = ClientFrame::Setup { sequence, command };
        // Check encoded size before dispatch. A rejected oversized Configure never mutates GUI state.
        ensure_setup_frame_size(&frame)?;
        self.sequence()?;
        write_frame(&mut self.pipe, &frame)?;
        let response = read_server_frame(&mut self.pipe)?;
        ensure!(
            response.reply.client == self.client && response.reply.sequence == sequence,
            "Stopped setup transport reply mismatch"
        );
        let setup = response
            .setup
            .context("GUI bridge omitted stopped setup response")?;
        ensure!(
            setup.client == self.client && setup.sequence == sequence,
            "Stopped setup reply mismatch"
        );
        Ok(setup)
    }

    pub fn discovery_status(&self) -> Option<crate::gui_discovery::Status> {
        self.discovery.as_ref().map(|status| {
            self.discovery_received_at.map_or_else(
                || status.clone(),
                |received_at| age_discovery_status(status.clone(), received_at, Instant::now()),
            )
        })
    }

    pub fn inspect(&mut self) -> Result<Reply> {
        let sequence = self.sequence()?;
        self.exchange(ClientFrame::Inspect { sequence })
    }

    pub fn execute(&mut self, request: session::Request) -> Result<Reply> {
        let sequence = self.sequence()?;
        self.exchange(ClientFrame::Execute {
            sequence,
            request: Box::new(request),
        })
    }

    pub fn capture(
        &mut self,
        user: usize,
        query: Option<crate::screen_analysis::Query>,
    ) -> Result<Reply> {
        let sequence = self.sequence()?;
        self.exchange(ClientFrame::Capture {
            sequence,
            user,
            query,
        })
    }

    pub fn app_preferences(
        &mut self,
        command: agent_runtime::AppPreferencesCommand,
    ) -> Result<agent_runtime::AppPreferencesState> {
        let sequence = self.sequence()?;
        write_frame(
            &mut self.pipe,
            &ClientFrame::AppPreferences { sequence, command },
        )?;
        let frame = read_server_frame(&mut self.pipe)?;
        ensure!(
            frame.reply.client == self.client,
            "GUI bridge reply client mismatch"
        );
        ensure!(
            frame.reply.sequence == sequence,
            "GUI bridge reply sequence mismatch"
        );
        let state = frame
            .app_preferences
            .context("GUI bridge omitted app-preference response")?;
        ensure!(
            state.acknowledged,
            "GUI app-preference request was not acknowledged"
        );
        ensure!(
            state.client == self.client,
            "GUI app-preference client mismatch"
        );
        ensure!(
            state.sequence == sequence,
            "GUI app-preference sequence mismatch"
        );
        // A legacy frame explicitly has no current discovery snapshot. Clear
        // any older value so consumers cannot mistake it for fresh metadata.
        self.discovery = frame.discovery;
        self.discovery_received_at = Some(Instant::now());
        Ok(state)
    }

    fn sequence(&mut self) -> Result<u64> {
        let sequence = self.next_sequence;
        self.next_sequence = sequence
            .checked_add(1)
            .context("GUI client sequence exhausted")?;
        Ok(sequence)
    }

    fn exchange(&mut self, frame: ClientFrame) -> Result<Reply> {
        write_frame(&mut self.pipe, &frame)?;
        let response = read_server_frame(&mut self.pipe)?;
        let reply = response.reply;
        ensure!(
            reply.client == self.client,
            "GUI bridge reply client mismatch"
        );
        let sequence = match frame {
            ClientFrame::Inspect { sequence }
            | ClientFrame::Execute { sequence, .. }
            | ClientFrame::Capture { sequence, .. }
            | ClientFrame::AppPreferences { sequence, .. }
            | ClientFrame::Setup { sequence, .. } => sequence,
            _ => unreachable!("exchange only accepts client operations"),
        };
        ensure!(
            reply.sequence == sequence,
            "GUI bridge reply sequence mismatch"
        );
        // Missing optional metadata on a validated legacy reply means there is
        // no current snapshot to expose; never keep an older status looking fresh.
        self.discovery = response.discovery;
        self.discovery_received_at = Some(Instant::now());
        Ok(reply)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Ok(sequence) = self.sequence() {
            let _ = write_frame(&mut self.pipe, &ClientFrame::Disconnect { sequence });
        }
    }
}

fn read_descriptor(path: &Path) -> Result<Descriptor> {
    let metadata = fs::metadata(path).context("read GUI bridge descriptor metadata")?;
    ensure!(
        metadata.len() <= 64 * 1024,
        "GUI bridge descriptor is too large"
    );
    let bytes = fs::read(path).context("read GUI bridge descriptor")?;
    ensure!(
        bytes.len() <= 64 * 1024,
        "GUI bridge descriptor is too large"
    );
    let descriptor: Descriptor = serde_json::from_slice(&bytes)?;
    ensure!(
        descriptor
            .pipe
            .starts_with(r"\\.\pipe\ProtoInputMultiPlex."),
        "GUI bridge descriptor has an invalid pipe name"
    );
    ensure!(
        descriptor.token.len() == 64,
        "GUI bridge token has invalid length"
    );
    ensure!(
        descriptor.controller_pid != 0,
        "GUI bridge controller PID is invalid"
    );
    Ok(descriptor)
}

pub fn is_setup_descriptor(path: &Path) -> Result<bool> {
    Ok(read_descriptor(path)?.kind == DescriptorKind::StoppedSetup)
}

pub(crate) fn write_private_descriptor(path: &Path, bytes: &[u8]) -> Result<()> {
    ensure!(
        bytes.len() <= 64 * 1024,
        "GUI bridge descriptor is too large"
    );
    let filename = path
        .file_name()
        .context("GUI bridge descriptor needs a filename")?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    // On Windows canonicalize returns the extended-length form. Resolve the
    // existing directory before adding the random temporary suffix: the final
    // name can fit MAX_PATH while the protected CREATE_NEW name does not.
    let parent = fs::canonicalize(parent).context("resolve GUI bridge descriptor parent")?;
    let path = parent.join(filename);
    let security = SecurityDescriptor::current_logon()?;
    let attributes = security.attributes();
    let mut temporary_name = filename.to_os_string();
    temporary_name.push(".tmp.");
    temporary_name.push(random_token()?);
    let temporary_path = path.with_file_name(temporary_name);
    let name: Vec<u16> = temporary_path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let file = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_WRITE,
            0,
            &attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    ensure!(
        file != INVALID_HANDLE_VALUE,
        "create private GUI bridge descriptor: {}",
        std::io::Error::last_os_error()
    );
    let mut file = unsafe { File::from_raw_handle(file as _) };
    let write_result = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary_path);
        return Err(error).context("write private GUI bridge descriptor");
    }
    if let Err(error) = fs::rename(&temporary_path, &path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(error).context("publish private GUI bridge descriptor");
    }
    Ok(())
}

pub(crate) fn random_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            bytes.as_mut_ptr(),
            bytes.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    ensure!(status == 0, "Windows CSPRNG failed with status {status:#x}");
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

struct SecurityDescriptor(*mut core::ffi::c_void);

impl SecurityDescriptor {
    fn current_logon() -> Result<Self> {
        let mut token: HANDLE = std::ptr::null_mut();
        ensure!(
            unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } != 0,
            "open current process token"
        );
        let mut required = 0u32;
        unsafe {
            GetTokenInformation(
                token,
                windows_sys::Win32::Security::TokenGroups,
                std::ptr::null_mut(),
                0,
                &mut required,
            );
        }
        if required == 0 {
            unsafe { CloseHandle(token) };
            bail!("current token has no group information");
        }
        let mut words = vec![0usize; (required as usize).div_ceil(std::mem::size_of::<usize>())];
        let read = unsafe {
            GetTokenInformation(
                token,
                windows_sys::Win32::Security::TokenGroups,
                words.as_mut_ptr().cast(),
                required,
                &mut required,
            )
        };
        unsafe { CloseHandle(token) };
        ensure!(read != 0, "read current token groups");
        let groups = words.as_ptr().cast::<TOKEN_GROUPS>();
        let entries = unsafe { std::ptr::addr_of!((*groups).Groups) };
        let mut logon_sid = std::ptr::null_mut();
        for index in 0..unsafe { (*groups).GroupCount as usize } {
            let entry = unsafe {
                entries
                    .cast::<windows_sys::Win32::Security::SID_AND_ATTRIBUTES>()
                    .add(index)
                    .read()
            };
            if entry.Attributes & 0xC000_0000 == 0xC000_0000 {
                logon_sid = entry.Sid;
                break;
            }
        }
        ensure!(!logon_sid.is_null(), "current logon SID is unavailable");
        let mut sid_text = std::ptr::null_mut();
        ensure!(
            unsafe { ConvertSidToStringSidW(logon_sid, &mut sid_text) } != 0,
            "convert current logon SID"
        );
        let mut length = 0usize;
        unsafe {
            while *sid_text.add(length) != 0 {
                length += 1;
            }
        }
        let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(sid_text, length) });
        unsafe { LocalFree(sid_text.cast()) };
        let sid = sid?;
        let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut descriptor = std::ptr::null_mut();
        ensure!(
            unsafe {
                windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    1,
                    &mut descriptor,
                    std::ptr::null_mut(),
                )
            } != 0,
            "create protected logon-SID security descriptor"
        );
        Ok(Self(descriptor))
    }

    fn attributes(&self) -> windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
        windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>()
                as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

fn serve(
    target: BrokerTarget,
    pipe_name: String,
    token: String,
    stop: Arc<AtomicBool>,
    first_pipe: File,
) {
    use std::os::windows::io::IntoRawHandle;
    let mut first_pipe = Some(first_pipe);
    static NEXT_CLIENT: std::sync::OnceLock<Arc<AtomicU64>> = std::sync::OnceLock::new();
    let next_client = NEXT_CLIENT
        .get_or_init(|| Arc::new(AtomicU64::new(1)))
        .clone();
    let connections = Arc::new(AtomicUsize::new(0));
    let mut workers: Vec<JoinHandle<()>> = Vec::new();
    while !stop.load(Ordering::Acquire) {
        reap_workers(&mut workers);
        let next_pipe = first_pipe
            .take()
            .map(|file| Ok(file.into_raw_handle() as HANDLE))
            .unwrap_or_else(|| create_pipe(&pipe_name));
        let pipe = match next_pipe {
            Ok(pipe) => pipe,
            Err(_) => {
                thread::sleep(POLL);
                continue;
            }
        };
        if !wait_for_client(pipe, &stop) {
            unsafe {
                DisconnectNamedPipe(pipe);
                CloseHandle(pipe);
            }
            if stop.load(Ordering::Acquire) {
                break;
            }
            continue;
        }
        if connections.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            connections.fetch_sub(1, Ordering::AcqRel);
            unsafe {
                DisconnectNamedPipe(pipe);
                CloseHandle(pipe);
            }
            continue;
        }
        let target = target.clone();
        let token = token.clone();
        let stop = stop.clone();
        let worker_connections = connections.clone();
        let next_client = next_client.clone();
        let pipe_value = pipe as usize;
        let worker = thread::Builder::new()
            .name("gui-agent-client".into())
            .spawn(move || {
                let _connection = ConnectionCount(worker_connections);
                let file = unsafe { File::from_raw_handle(pipe_value as _) };
                let _ = match target {
                    BrokerTarget::Live(handle, app_handle) => serve_client(
                        Pipe(file),
                        handle,
                        token,
                        next_client,
                        stop,
                        Some(app_handle),
                    ),
                    BrokerTarget::StoppedSetup(setup) => {
                        serve_setup_client(Pipe(file), token, next_client, stop, setup)
                    }
                };
            });
        if let Ok(worker) = worker {
            workers.push(worker);
        } else {
            connections.fetch_sub(1, Ordering::AcqRel);
            unsafe {
                DisconnectNamedPipe(pipe);
                CloseHandle(pipe);
            }
        }
    }
    stop.store(true, Ordering::Release);
    for worker in workers {
        let _ = worker.join();
    }
}

struct ConnectionCount(Arc<AtomicUsize>);
impl Drop for ConnectionCount {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn reap_workers(workers: &mut Vec<JoinHandle<()>>) {
    let mut index = 0;
    while index < workers.len() {
        if workers[index].is_finished() {
            let worker = workers.swap_remove(index);
            let _ = worker.join();
        } else {
            index += 1;
        }
    }
}

fn create_pipe(name: &str) -> Result<HANDLE> {
    let security = SecurityDescriptor::current_logon()?;
    let attributes = security.attributes();
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let pipe = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            &attributes,
        )
    };
    ensure!(pipe != INVALID_HANDLE_VALUE, "create private agent pipe");
    Ok(pipe)
}

fn wait_for_client(pipe: HANDLE, stop: &AtomicBool) -> bool {
    loop {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        let connected = unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) };
        if connected != 0 {
            return true;
        }
        let error = unsafe { GetLastError() };
        if error == ERROR_PIPE_CONNECTED {
            return true;
        }
        if error != ERROR_PIPE_LISTENING {
            return false;
        }
        thread::sleep(POLL);
    }
}

fn set_nonblocking(handle: HANDLE) -> Result<()> {
    let mode = PIPE_READMODE_BYTE | PIPE_NOWAIT;
    ensure!(
        unsafe {
            SetNamedPipeHandleState(handle, &mode, std::ptr::null_mut(), std::ptr::null_mut())
        } != 0,
        "set named pipe nonblocking mode"
    );
    Ok(())
}

fn serve_client(
    mut pipe: Pipe,
    handle: session::SharedHandle,
    token: String,
    next_client: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    app_handle: Option<AppHandle>,
) -> Result<()> {
    set_nonblocking(pipe.as_raw_handle() as HANDLE)?;
    let first: ClientFrame = read_frame(&mut pipe, Some(&stop))?;
    let ClientFrame::Connect { token: presented } = first else {
        bail!("first GUI bridge frame must authenticate");
    };
    ensure!(
        constant_time_eq(&token, &presented),
        "GUI bridge authentication failed"
    );
    let client = next_client
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(1)
        })
        .map_err(|_| anyhow::anyhow!("GUI bridge client ID space exhausted"))?;
    let mut session = AgentSession {
        handle,
        client,
        next_sequence: 1,
        stop,
        connected: true,
        connection_alive: Arc::new(AtomicBool::new(true)),
        app: app_handle.clone(),
    };
    let connected = session.exchange(
        agent_runtime::Command::Connect {
            client,
            sequence: 0,
        },
        0,
    )?;
    if let Some(app) = &app_handle {
        app.admit(client, &connected)?;
    }
    session.write_reply(&mut pipe, connected, 0, false, None)?;
    loop {
        let frame =
            match read_authenticated_frame::<_, ClientFrame>(&mut pipe, &session.stop, || true) {
                Ok(frame) => frame,
                Err(_) => break,
            };
        let inspect = matches!(&frame, ClientFrame::Inspect { .. });
        let sequence = match &frame {
            ClientFrame::Inspect { sequence }
            | ClientFrame::Execute { sequence, .. }
            | ClientFrame::Capture { sequence, .. }
            | ClientFrame::AppPreferences { sequence, .. }
            | ClientFrame::Setup { sequence, .. }
            | ClientFrame::Disconnect { sequence } => *sequence,
            ClientFrame::Connect { .. } => 0,
        };
        let result = match frame {
            ClientFrame::Connect { .. } => bail!("authentication may only be sent once"),
            ClientFrame::Inspect { sequence } => {
                session.sequence(sequence)?;
                session.exchange(
                    agent_runtime::Command::Inspect { client, sequence },
                    sequence,
                )?
            }
            ClientFrame::Execute { sequence, request } => {
                session.sequence(sequence)?;
                session.exchange(
                    agent_runtime::Command::Execute {
                        client,
                        sequence,
                        request,
                    },
                    sequence,
                )?
            }
            ClientFrame::Capture {
                sequence,
                user,
                query,
            } => {
                session.sequence(sequence)?;
                session.exchange(
                    agent_runtime::Command::Capture {
                        client,
                        sequence,
                        user,
                        query,
                    },
                    sequence,
                )?
            }
            ClientFrame::AppPreferences { sequence, command } => {
                session.sequence(sequence)?;
                if session.app.as_ref().is_some_and(AppHandle::stopped) {
                    session.write_reply(&mut pipe, Reply::default(), sequence, false, None)?;
                    continue;
                }
                let state = match &app_handle {
                    Some(app_handle) => match app_handle.execute(
                        client,
                        sequence,
                        command,
                        session.connection_alive.clone(),
                        &session.stop,
                    ) {
                        Ok(state) => state,
                        Err(error) if app_handle.stopped() => {
                            // The closed gate wins this race without dropping a
                            // still-authorized terminal inspection connection.
                            session
                                .write_reply(&mut pipe, Reply::default(), sequence, false, None)
                                .map_err(|_| error)?;
                            continue;
                        }
                        Err(error) => return Err(error),
                    },
                    None => agent_runtime::AppPreferencesState {
                        acknowledged: true,
                        client,
                        sequence,
                        error: Some("GUI app-preference service is unavailable".into()),
                        ..Default::default()
                    },
                };
                let reply = session.exchange(
                    agent_runtime::Command::Inspect { client, sequence },
                    sequence,
                )?;
                session.write_reply(&mut pipe, reply, sequence, false, Some(state))?;
                continue;
            }
            ClientFrame::Setup { .. } => bail!("Stopped setup command on live bridge"),
            ClientFrame::Disconnect { sequence } => {
                session.sequence(sequence)?;
                session.connection_alive.store(false, Ordering::Release);
                let _ = session.exchange(
                    agent_runtime::Command::Disconnect { client, sequence },
                    sequence,
                );
                if let Some(app) = &session.app {
                    app.remove_connection(client);
                }
                session.connected = false;
                return Ok(());
            }
        };
        if session
            .write_reply(&mut pipe, result, sequence, inspect, None)
            .is_err()
        {
            break;
        }
    }
    session.disconnect();
    Ok(())
}

fn serve_setup_client(
    mut pipe: Pipe,
    token: String,
    next_client: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    setup: crate::gui_setup::Handle,
) -> Result<()> {
    set_nonblocking(pipe.as_raw_handle() as HANDLE)?;
    let first: ClientFrame = read_frame(&mut pipe, Some(&stop))?;
    let ClientFrame::Connect { token: presented } = first else {
        bail!("first GUI setup frame must authenticate");
    };
    ensure!(
        constant_time_eq(&token, &presented),
        "GUI setup authentication failed"
    );
    let client = next_client
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(1)
        })
        .map_err(|_| anyhow!("GUI bridge client ID space exhausted"))?;
    let epoch = setup.admit(client)?;
    struct Admission(crate::gui_setup::Handle, u64);
    impl Drop for Admission {
        fn drop(&mut self) {
            self.0.remove(self.1);
        }
    }
    let _admission = Admission(setup.clone(), client);
    write_frame_with_stop(
        &mut pipe,
        &ServerFrame {
            reply: Reply {
                acknowledged: true,
                client,
                sequence: 0,
                ..Default::default()
            },
            app_preferences: None,
            setup: None,
            discovery: None,
        },
        Some(&stop),
    )?;
    let mut next_sequence = 1_u64;
    while !stop.load(Ordering::Acquire) && setup.permits(client, epoch) {
        let frame: ClientFrame =
            match read_authenticated_frame(&mut pipe, &stop, || setup.permits(client, epoch)) {
                Ok(frame) => frame,
                Err(_) => break,
            };
        match frame {
            ClientFrame::Setup { sequence, command } => {
                ensure!(
                    sequence == next_sequence,
                    "GUI setup sequence is out of order"
                );
                next_sequence = next_sequence
                    .checked_add(1)
                    .context("GUI setup sequence exhausted")?;
                if matches!(&command, crate::gui_setup::Command::Activity) {
                    ensure!(
                        !stop.load(Ordering::Acquire),
                        "GUI setup bridge is stopping"
                    );
                    let response = setup.authorized_activity(client, sequence, epoch)?;
                    ensure!(
                        !stop.load(Ordering::Acquire) && setup.permits(client, epoch),
                        "GUI setup connection was revoked"
                    );
                    write_frame_with_stop(
                        &mut pipe,
                        &ServerFrame {
                            reply: Reply {
                                acknowledged: true,
                                client,
                                sequence,
                                ..Default::default()
                            },
                            app_preferences: None,
                            setup: Some(response),
                            discovery: None,
                        },
                        Some(&stop),
                    )?;
                    continue;
                }
                let command = match command.prepare() {
                    Ok(command) => command,
                    Err(error) => {
                        write_frame_with_stop(
                            &mut pipe,
                            &ServerFrame {
                                reply: Reply {
                                    acknowledged: true,
                                    client,
                                    sequence,
                                    ..Default::default()
                                },
                                app_preferences: None,
                                setup: Some(crate::gui_setup::Reply {
                                    client,
                                    sequence,
                                    value: serde_json::Value::Null,
                                    error: Some(format!("{error:#}")),
                                    profile: None,
                                }),
                                discovery: None,
                            },
                            Some(&stop),
                        )?;
                        continue;
                    }
                };
                let result = setup.submit(client, sequence, epoch, command)?;
                let deadline = Instant::now() + IO_TIMEOUT;
                let response = loop {
                    ensure!(
                        !stop.load(Ordering::Acquire) && setup.permits(client, epoch),
                        "GUI setup connection was revoked"
                    );
                    ensure!(
                        pipe.available().is_ok(),
                        "GUI setup client disconnected; an accepted operation may still complete"
                    );
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    ensure!(
                        !remaining.is_zero(),
                        "GUI setup request timed out; disk completion may be unconfirmed"
                    );
                    match result.recv_timeout(remaining.min(POLL)) {
                        Ok(reply) => break reply,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            bail!("GUI setup request was abandoned")
                        }
                    }
                };
                let mut frame = ServerFrame {
                    reply: Reply {
                        acknowledged: true,
                        client,
                        sequence,
                        ..Default::default()
                    },
                    app_preferences: None,
                    setup: Some(response),
                    discovery: None,
                };
                bound_setup_response(&mut frame)?;
                write_frame_with_stop(&mut pipe, &frame, Some(&stop))?;
            }
            ClientFrame::Disconnect { sequence } => {
                ensure!(
                    sequence == next_sequence,
                    "GUI setup sequence is out of order"
                );
                break;
            }
            _ => bail!("Routing command on stopped setup bridge"),
        }
    }
    Ok(())
}

struct AgentSession {
    handle: session::SharedHandle,
    client: u64,
    next_sequence: u64,
    stop: Arc<AtomicBool>,
    connected: bool,
    connection_alive: Arc<AtomicBool>,
    app: Option<AppHandle>,
}

impl AgentSession {
    fn sequence(&mut self, sequence: u64) -> Result<()> {
        ensure!(
            sequence == self.next_sequence,
            "GUI bridge sequence is out of order"
        );
        self.next_sequence = sequence
            .checked_add(1)
            .context("GUI bridge sequence exhausted")?;
        Ok(())
    }

    fn write_reply(
        &self,
        pipe: &mut Pipe,
        mut reply: Reply,
        sequence: u64,
        inspect: bool,
        app_preferences: Option<agent_runtime::AppPreferencesState>,
    ) -> Result<()> {
        if let Some(app) = &self.app {
            reply = app.public_reply(reply, self.client, sequence, inspect)?;
        } else {
            reply.authority = None;
        }
        let terminal = reply.terminal;
        write_frame_with_stop(
            pipe,
            &ServerFrame {
                reply,
                app_preferences: if terminal { None } else { app_preferences },
                setup: None,
                discovery: if terminal {
                    None
                } else {
                    self.app.as_ref().map(AppHandle::discovery_status)
                },
            },
            Some(&self.stop),
        )
    }

    fn exchange(&self, command: agent_runtime::Command, sequence: u64) -> Result<Reply> {
        let inspect = matches!(&command, agent_runtime::Command::Inspect { .. });
        if let Some(app) = &self.app
            && app.stopped()
        {
            return app.public_reply(Reply::default(), self.client, sequence, inspect);
        }
        let events = self.handle.subscribe(self.client, sequence);
        if let Some(app) = &self.app {
            if let Err(error) = app.dispatch(&self.handle, command) {
                // Stop may close the gate after the optimistic precheck. No
                // command was queued; preserve an admitted inspection connection.
                return app
                    .public_reply(Reply::default(), self.client, sequence, inspect)
                    .or(Err(error));
            }
        } else {
            self.handle.request(session::Request::Agent(command));
        }
        let deadline = Instant::now() + IO_TIMEOUT;
        while Instant::now() < deadline && !self.stop.load(Ordering::Acquire) {
            if let Some(app) = &self.app
                && app.stopped()
            {
                return app.public_reply(Reply::default(), self.client, sequence, inspect);
            }
            match events.recv_timeout(POLL) {
                Ok(session::Event::Agent(state)) if is_ack(&state, self.client, sequence) => {
                    return Ok(state);
                }
                Ok(session::Event::Stopped | session::Event::Failed(_)) => {
                    if let Some(app) = &self.app {
                        app.terminal();
                        return app.public_reply(Reply::default(), self.client, sequence, inspect);
                    }
                    bail!("GUI routing session is terminal");
                }
                Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(error) => return Err(error).context("GUI session event stream closed"),
            }
        }
        bail!(
            "GUI session did not acknowledge client {} sequence {sequence}",
            self.client
        )
    }

    fn disconnect(&mut self) {
        if !self.connected {
            return;
        }
        self.connection_alive.store(false, Ordering::Release);
        let sequence = self.next_sequence;
        if let Some(app) = &self.app {
            let _ = app.dispatch(
                &self.handle,
                agent_runtime::Command::Disconnect {
                    client: self.client,
                    sequence,
                },
            );
            app.remove_connection(self.client);
        } else {
            self.handle.request(session::Request::Agent(
                agent_runtime::Command::Disconnect {
                    client: self.client,
                    sequence,
                },
            ));
        }
        self.connected = false;
        self.connection_alive.store(false, Ordering::Release);
    }
}

impl Drop for AgentSession {
    fn drop(&mut self) {
        self.disconnect();
    }
}

fn is_ack(reply: &Reply, client: u64, sequence: u64) -> bool {
    reply.acknowledged && reply.client == client && reply.sequence == sequence
}

fn ensure_setup_frame_size(frame: &ClientFrame) -> Result<()> {
    ensure!(
        serde_json::to_vec(frame)?.len() <= MAX_FRAME_BYTES,
        "Stopped setup request exceeds the 16 MiB pipe frame limit"
    );
    Ok(())
}

fn bound_setup_response(frame: &mut ServerFrame) -> Result<()> {
    if serde_json::to_vec(&*frame)?.len() > MAX_FRAME_BYTES {
        frame.setup = Some(crate::gui_setup::Reply {
            client: frame.reply.client,
            sequence: frame.reply.sequence,
            value: serde_json::Value::Null,
            error: Some("GUI profile response exceeds the 16 MiB pipe frame limit".into()),
            profile: None,
        });
    }
    Ok(())
}

fn constant_time_eq(expected: &str, actual: &str) -> bool {
    let mut difference = expected.len() ^ actual.len();
    for index in 0..expected.len().max(actual.len()) {
        let left = expected.as_bytes().get(index).copied().unwrap_or(0);
        let right = actual.as_bytes().get(index).copied().unwrap_or(0);
        difference |= usize::from(left ^ right);
    }
    difference == 0
}

fn write_frame<W: Write, T: Serialize>(writer: &mut W, value: &T) -> Result<()> {
    write_frame_with_stop(writer, value, None)
}

fn write_frame_with_stop<W: Write, T: Serialize>(
    writer: &mut W,
    value: &T,
    stop: Option<&AtomicBool>,
) -> Result<()> {
    let payload = serde_json::to_vec(value)?;
    ensure!(
        payload.len() <= MAX_FRAME_BYTES,
        "GUI bridge frame exceeds 16 MiB"
    );
    let mut frame = Vec::with_capacity(payload.len() + 4);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    write_deadline(writer, &frame, Instant::now() + IO_TIMEOUT, stop)
}

fn read_frame<R: Read, T: for<'de> Deserialize<'de>>(
    reader: &mut R,
    stop: Option<&AtomicBool>,
) -> Result<T> {
    read_frame_with_timeout(reader, stop, IO_TIMEOUT, None)
}

fn read_authenticated_frame<R: Read, T: for<'de> Deserialize<'de>>(
    reader: &mut R,
    stop: &AtomicBool,
    permitted: impl Fn() -> bool,
) -> Result<T> {
    read_frame_with_timeout(reader, Some(stop), IO_TIMEOUT, Some(&permitted))
}

fn read_frame_with_timeout<R: Read, T: for<'de> Deserialize<'de>>(
    reader: &mut R,
    stop: Option<&AtomicBool>,
    timeout: Duration,
    idle_permitted: Option<&dyn Fn() -> bool>,
) -> Result<T> {
    // Authentication and response reads keep their original whole-frame deadline.
    // An admitted client may remain idle without losing its input owners. Once
    // its first byte arrives, one deadline covers the remaining header and body.
    let initial_deadline = idle_permitted.is_none().then(|| Instant::now() + timeout);
    let mut header = [0u8; 4];
    let deadline = loop {
        ensure!(
            stop.is_none_or(|flag| !flag.load(Ordering::Acquire)),
            "GUI broker is stopping"
        );
        ensure!(
            idle_permitted.is_none_or(|permitted| permitted()),
            "GUI bridge connection was revoked"
        );
        ensure!(
            initial_deadline.is_none_or(|deadline| Instant::now() < deadline),
            "GUI bridge request timed out"
        );
        match reader.read(&mut header[..1]) {
            Ok(0) => bail!("GUI bridge client disconnected"),
            Ok(_) => break initial_deadline.unwrap_or_else(|| Instant::now() + timeout),
            Err(error) if retryable(&error) => thread::sleep(POLL),
            Err(error) => return Err(error.into()),
        }
    };
    read_deadline(reader, &mut header[1..], deadline, stop)?;
    let length = u32::from_be_bytes(header) as usize;
    ensure!(
        length > 0 && length <= MAX_FRAME_BYTES,
        "invalid GUI bridge frame length"
    );
    let mut payload = vec![0; length];
    read_deadline(reader, &mut payload, deadline, stop)?;
    Ok(serde_json::from_slice(&payload)?)
}

fn read_server_frame<R: Read>(reader: &mut R) -> Result<ServerFrame> {
    read_frame::<_, ServerFrame>(reader, None)
}

fn read_deadline<R: Read>(
    reader: &mut R,
    buffer: &mut [u8],
    deadline: Instant,
    stop: Option<&AtomicBool>,
) -> Result<()> {
    let mut offset = 0;
    while offset < buffer.len() {
        ensure!(
            stop.is_none_or(|flag| !flag.load(Ordering::Acquire)),
            "GUI broker is stopping"
        );
        ensure!(Instant::now() < deadline, "GUI bridge request timed out");
        match reader.read(&mut buffer[offset..]) {
            Ok(0) => bail!("GUI bridge client disconnected"),
            Ok(read) => offset += read,
            Err(error) if retryable(&error) => thread::sleep(POLL),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn write_deadline<W: Write>(
    writer: &mut W,
    buffer: &[u8],
    deadline: Instant,
    stop: Option<&AtomicBool>,
) -> Result<()> {
    let mut offset = 0;
    while offset < buffer.len() {
        ensure!(
            stop.is_none_or(|flag| !flag.load(Ordering::Acquire)),
            "GUI broker is stopping"
        );
        ensure!(Instant::now() < deadline, "GUI bridge response timed out");
        // A NOWAIT write larger than the available pipe quota can report zero
        // even for an empty pipe. Keep each attempt below our 64 KiB quota.
        let end = buffer.len().min(offset + 16 * 1024);
        match writer.write(&buffer[offset..end]) {
            Ok(0) => thread::sleep(POLL),
            Ok(written) => offset += written,
            Err(error) if retryable(&error) => thread::sleep(POLL),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn retryable(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
        || matches!(error.raw_os_error(), Some(ERROR_NO_DATA))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn private_descriptor_publishes_long_normal_path_with_private_logon_acl() {
        use windows_sys::Win32::Security::{
            Authorization::{
                ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
                SE_FILE_OBJECT,
            },
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl, SE_DACL_PROTECTED,
        };
        struct OwnedDirectory(PathBuf);
        impl Drop for OwnedDirectory {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        fn dacl_text(descriptor: *mut core::ffi::c_void) -> String {
            let mut text = std::ptr::null_mut();
            assert_ne!(
                unsafe {
                    ConvertSecurityDescriptorToStringSecurityDescriptorW(
                        descriptor,
                        1,
                        DACL_SECURITY_INFORMATION,
                        &mut text,
                        std::ptr::null_mut(),
                    )
                },
                0
            );
            let mut length = 0;
            unsafe {
                while *text.add(length) != 0 {
                    length += 1;
                }
            }
            let value = String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) });
            unsafe {
                LocalFree(text.cast());
            }
            value.unwrap()
        }
        let root = std::env::temp_dir().join(format!(
            "pim-desc-regression-{}-{}",
            std::process::id(),
            &random_token().unwrap()[..12]
        ));
        assert!(
            root.is_absolute() && !root.to_string_lossy().starts_with(r"\\?\"),
            "regression requires an ordinary absolute Windows path"
        );
        fs::create_dir(&root).unwrap();
        let _owned = OwnedDirectory(root.clone());
        let root_units = root.as_os_str().encode_wide().count();
        let parent_units = 195;
        assert!(
            root_units + 1 < parent_units,
            "temporary root is too long for the ordinary-path regression"
        );
        let directory = root.join("p".repeat(parent_units - root_units - 1));
        fs::create_dir(&directory).unwrap();
        let descriptor = directory.join("connection.json");
        let final_units = descriptor.as_os_str().encode_wide().count();
        assert_eq!(final_units, 211);
        assert!(
            final_units < 260 && final_units + ".tmp.".len() + 64 > 260,
            "only the protected temporary filename must cross MAX_PATH"
        );
        let bytes = br#"{"private_descriptor_regression":true}"#;
        write_private_descriptor(&descriptor, bytes)
            .expect("publish a protected descriptor whose ordinary final path fits MAX_PATH");
        assert_eq!(fs::read(&descriptor).unwrap(), bytes);
        let name: Vec<u16> = descriptor
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        let mut security = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                GetNamedSecurityInfoW(
                    name.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut security,
                )
            },
            0
        );
        assert!(!security.is_null());
        let security = SecurityDescriptor(security);
        let mut control = 0_u16;
        let mut revision = 0_u32;
        assert_ne!(
            unsafe { GetSecurityDescriptorControl(security.0, &mut control, &mut revision) },
            0
        );
        assert_ne!(
            control & SE_DACL_PROTECTED,
            0,
            "private descriptor DACL must reject inheritance"
        );
        let actual = dacl_text(security.0);
        let expected = dacl_text(SecurityDescriptor::current_logon().unwrap().0);
        let aces: Vec<_> = actual
            .split('(')
            .skip(1)
            .map(|ace| {
                ace.split(')')
                    .next()
                    .unwrap()
                    .split(';')
                    .collect::<Vec<_>>()
            })
            .collect();
        let expected_aces: Vec<_> = expected
            .split('(')
            .skip(1)
            .map(|ace| {
                ace.split(')')
                    .next()
                    .unwrap()
                    .split(';')
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(aces.len(), 1);
        assert_eq!(expected_aces.len(), 1);
        assert_eq!(aces[0].len(), 6);
        assert_eq!(aces[0][0], "A");
        assert!(
            matches!(aces[0][2], "GA" | "0x10000000" | "FA" | "0x1f01ff"),
            "full access may be file-generic mapped"
        );
        assert!(aces[0][5].starts_with("S-1-5-5-"));
        assert_eq!(
            aces[0][5], expected_aces[0][5],
            "only the actual current logon SID may access the descriptor"
        );
        let files: Vec<_> = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(
            files,
            vec![std::ffi::OsString::from("connection.json")],
            "atomic publication must leave no temporary file"
        );
    }

    #[test]
    fn capture_query_wire_is_optional_and_rejects_analysis_control_fields() {
        let legacy: ClientFrame =
            serde_json::from_value(serde_json::json!({"type":"capture","sequence":7,"user":0}))
                .unwrap();
        assert!(matches!(legacy, ClientFrame::Capture { query: None, .. }));
        let current: ClientFrame = serde_json::from_value(serde_json::json!({
            "type":"capture","sequence":8,"user":0,"query":{"type":"pixel","x":1,"y":2}
        }))
        .unwrap();
        assert!(matches!(
            current,
            ClientFrame::Capture {
                query: Some(crate::screen_analysis::Query::Pixel { x: 1, y: 2 }),
                ..
            }
        ));
        assert!(serde_json::from_value::<ClientFrame>(serde_json::json!({
            "type":"capture","sequence":9,"user":0,"query":{"type":"pixel","x":1,"y":2,"click":true}
        })).is_err());
    }

    #[test]
    fn stopped_event_status_traverses_real_broker_and_returns_only_safe_aged_summary() {
        let directory = std::env::temp_dir().join(format!(
            "protoinputmultiplex-event-status-{}-{}", std::process::id(), random_token().unwrap()
        ));
        fs::create_dir(&directory).unwrap();
        let descriptor = directory.join("connection.json");
        let (setup, requests) = crate::gui_setup::channel();
        setup.enable();
        let broker = Broker::start_setup_cancellable(
            setup.clone(), descriptor.clone(), Arc::new(AtomicU64::new(1)), 1,
            Arc::new(AtomicBool::new(false)),
        ).unwrap();
        let (sent, received) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let result = (|| -> Result<crate::gui_setup::Reply> {
                let mut client = Client::connect_setup(descriptor)?;
                client.setup(crate::gui_setup::Command::ApplicationEventStatus)
            })();
            let _ = sent.send(result);
        });
        let queued = requests.recv_timeout(Duration::from_secs(2));
        // Exercise the actual authenticated Setup request/reply transport. This
        // responder stands in for the GUI event loop; it does not launch a GUI.
        let admitted = queued.as_ref().is_ok_and(|request| {
            matches!(&request.command, crate::gui_setup::PreparedCommand::ApplicationEventStatus)
                && setup.permits(request.client, request.epoch)
        });
        if let Ok(request) = queued {
            let mut status = crate::gui_application_events::Controller::default().status();
            status.transport_sample_age_ms = Some(3_000);
            status.connected_clients = Some(2);
            status.cleanup_pending = Some(1);
            status.uncertain_admissions = Some(7);
            status.queue_overflows = Some(3);
            status.age_by(Duration::ZERO);
            request.reply(serde_json::to_value(status).unwrap(), None, None);
        }
        let result = received.recv_timeout(Duration::from_secs(2));
        broker.shutdown().unwrap();
        worker.join().unwrap();
        assert!(admitted, "Event status did not use the admitted setup queue");
        let reply = result.expect("Setup status did not complete its queued reply").unwrap();
        assert!(reply.error.is_none() && reply.profile.is_none());
        let keys: BTreeSet<_> = reply.value.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, BTreeSet::from([
            "phase", "session_active", "local_virtual_users", "native_observed_users",
            "native_sequence", "pointer_export", "keyboard_export", "browser_approved",
            "approved_receivers", "transport_sample_age_ms", "retired", "sample_age_ms",
        ]));
        let status: crate::gui_application_events::PublicStatus = serde_json::from_value(reply.value).unwrap();
        assert!(status.transport_sample_age_ms.unwrap() >= 3_000);
        assert!(status.connected_clients.is_none() && status.cleanup_pending.is_none());
        assert!(status.uncertain_admissions.is_none() && status.queue_overflows.is_none());
        assert!(!status.session_active && !status.pointer_export && !status.keyboard_export);
        fs::remove_dir(directory).unwrap();
    }
    #[test]
    fn stopped_activity_uses_real_broker_without_draining_gui_requests() {
        let directory = std::env::temp_dir().join(format!(
            "protoinputmultiplex-activity-{}-{}",
            std::process::id(),
            random_token().unwrap()
        ));
        fs::create_dir(&directory).unwrap();
        let descriptor = directory.join("connection.json");
        let (mut setup, requests) = crate::gui_setup::channel();
        let probe = crate::gui_activity::Probe::new();
        setup.attach_activity(probe.observer());
        setup.enable();
        let broker = Broker::start_setup_cancellable(
            setup.clone(),
            descriptor.clone(),
            Arc::new(AtomicU64::new(1)),
            1,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let (sent, received) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let result = (|| -> Result<crate::gui_setup::Reply> {
                let mut client = Client::connect_setup(descriptor)?;
                client.setup(crate::gui_setup::Command::Activity)
            })();
            let _ = sent.send(result);
        });
        // Even a regression that queues Activity cannot strand the test for IO_TIMEOUT.
        let result = received.recv_timeout(Duration::from_secs(2));
        broker.shutdown().unwrap();
        worker.join().unwrap();
        let reply = result
            .expect("Activity waited for the undrained GUI queue")
            .unwrap();
        assert!(reply.error.is_none());
        assert!(reply.profile.is_none());
        let activity: crate::gui_activity::Snapshot =
            serde_json::from_value(reply.value["gui_activity"].clone()).unwrap();
        assert!(activity.observation.is_some());
        assert!(matches!(
            requests.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn client_discovery_cache_ages_and_legacy_reply_clears_activity() {
        let name = format!(
            r"\\.\pipe\ProtoInputMultiPlex.test.{}.{}",
            std::process::id(),
            random_token().unwrap()
        );
        let server = unsafe { File::from_raw_handle(create_pipe(&name).unwrap() as _) };
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&name)
            .unwrap();
        assert!(wait_for_client(
            server.as_raw_handle() as HANDLE,
            &AtomicBool::new(false)
        ));
        let probe = crate::gui_activity::Probe::new();
        let mut client = Client {
            pipe: Pipe(file),
            client: 17,
            next_sequence: 1,
            kind: DescriptorKind::Live,
            discovery: Some(crate::gui_discovery::Status {
                gui_activity: Some(probe.observer().snapshot()),
                ..Default::default()
            }),
            discovery_received_at: Some(Instant::now() - Duration::from_millis(100)),
        };
        let aged = client.discovery_status().unwrap().gui_activity.unwrap();
        assert!(aged.sample_age_ms >= 100);
        let worker = thread::spawn(move || {
            let mut server = Pipe(server);
            let ClientFrame::Inspect { sequence } = read_frame(&mut server, None).unwrap() else {
                panic!("Expected the read-only inspection request");
            };
            let frame = ServerFrame {
                reply: Reply {
                    acknowledged: true,
                    client: 17,
                    sequence,
                    ..Default::default()
                },
                app_preferences: None,
                setup: None,
                discovery: None,
            };
            // The old peer omits discovery entirely, rather than sending fresh empty data.
            let mut legacy = serde_json::to_value(frame).unwrap();
            legacy.as_object_mut().unwrap().remove("discovery");
            write_frame(&mut server, &legacy).unwrap();
        });
        client.inspect().unwrap();
        assert!(client.discovery_status().is_none());
        worker.join().unwrap();
    }

    #[test]
    fn discovery_activity_is_fresh_after_old_inventory_and_ages_at_observation() {
        let (mut app, _requests) = app_channel();
        assert!(app.activity_snapshot().is_none());
        let probe = crate::gui_activity::Probe::new();
        app.attach_activity(probe.observer());
        let published_at = Instant::now() - Duration::from_secs(60);
        *app.discovery.write().unwrap() = (Default::default(), published_at);
        let status = app.discovery_status();
        assert!(status.sample_age_ms >= 60_000);
        let initial_activity_age = status.gui_activity.as_ref().unwrap().sample_age_ms;
        // Inventory's 60-second publication age must not contaminate a fresh callback sample.
        assert!(initial_activity_age < 60_000);
        let received = Instant::now();
        let aged = age_discovery_status(status, received, received + Duration::from_millis(25));
        assert_eq!(
            aged.gui_activity.unwrap().sample_age_ms,
            initial_activity_age + 25
        );
    }

    #[test]
    fn idle_named_pipe_is_not_eof_and_peer_close_is_detected() {
        let name = format!(
            r"\\.\pipe\ProtoInputMultiPlex.test.{}.{}",
            std::process::id(),
            random_token().unwrap()
        );
        let server = unsafe { File::from_raw_handle(create_pipe(&name).unwrap() as _) };
        let client = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&name)
            .unwrap();
        set_nonblocking(client.as_raw_handle() as HANDLE).unwrap();
        assert!(wait_for_client(
            server.as_raw_handle() as HANDLE,
            &AtomicBool::new(false)
        ));
        let mut server = Pipe(server);
        let mut client = Pipe(client);
        let mut byte = [0];
        assert_eq!(
            server.read(&mut byte).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        client.write_all(b"x").unwrap();
        assert_eq!(server.read(&mut byte).unwrap(), 1);
        assert_eq!(byte, *b"x");
        drop(client);
        assert_ne!(
            server.read(&mut byte).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn named_pipe_transfers_png_budget_sized_json_in_both_directions() {
        let name = format!(
            r"\\.\pipe\ProtoInputMultiPlex.test.{}.{}",
            std::process::id(),
            random_token().unwrap()
        );
        let server = unsafe { File::from_raw_handle(create_pipe(&name).unwrap() as _) };
        let client = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&name)
            .unwrap();
        set_nonblocking(client.as_raw_handle() as HANDLE).unwrap();
        assert!(wait_for_client(
            server.as_raw_handle() as HANDLE,
            &AtomicBool::new(false)
        ));
        let mut server = Pipe(server);
        let mut client = Pipe(client);
        let payload: Vec<u8> = (0..2 * 1024 * 1024)
            .map(|index| (index % 256) as u8)
            .collect();
        let expected = payload.clone();
        let peer = std::thread::spawn(move || {
            let received: Vec<u8> = read_frame(&mut client, None).unwrap();
            assert_eq!(received, expected);
            write_frame(&mut client, &received).unwrap();
        });
        write_frame(&mut server, &payload).unwrap();
        let received: Vec<u8> = read_frame(&mut server, None).unwrap();
        assert_eq!(received, payload);
        peer.join().unwrap();
    }

    #[test]
    fn frames_roundtrip_and_enforce_payload_limit() {
        let frame = ClientFrame::Inspect { sequence: 7 };
        let mut wire = Vec::new();
        write_frame(&mut wire, &frame).unwrap();
        let decoded: ClientFrame = read_frame(&mut Cursor::new(wire), None).unwrap();
        assert!(matches!(decoded, ClientFrame::Inspect { sequence: 7 }));

        let too_large = vec![b'x'; MAX_FRAME_BYTES + 1];
        assert!(write_frame(&mut Vec::new(), &too_large).is_err());
    }

    struct DelayedFrame {
        bytes: Cursor<Vec<u8>>,
        idle: Option<Duration>,
        stall_at_end: bool,
    }

    impl Read for DelayedFrame {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            if let Some(idle) = self.idle.take() {
                thread::sleep(idle);
                return Err(std::io::ErrorKind::WouldBlock.into());
            }
            if self.stall_at_end && self.bytes.position() == self.bytes.get_ref().len() as u64 {
                return Err(std::io::ErrorKind::WouldBlock.into());
            }
            self.bytes.read(bytes)
        }
    }

    #[test]
    fn authenticated_idle_outlives_frame_budget_but_authentication_does_not() {
        let mut wire = Vec::new();
        write_frame(&mut wire, &ClientFrame::Inspect { sequence: 7 }).unwrap();
        let budget = Duration::from_millis(100);
        for authenticated in [true, false] {
            let mut reader = DelayedFrame {
                bytes: Cursor::new(wire.clone()),
                idle: Some(budget * 3),
                stall_at_end: false,
            };
            let permitted = || true;
            let result = read_frame_with_timeout::<_, ClientFrame>(
                &mut reader,
                None,
                budget,
                authenticated.then_some(&permitted as &dyn Fn() -> bool),
            );
            if authenticated {
                assert!(matches!(
                    result.unwrap(),
                    ClientFrame::Inspect { sequence: 7 }
                ));
            } else {
                assert!(result.err().unwrap().to_string().contains("timed out"));
            }
        }
    }

    #[test]
    fn authenticated_partial_header_and_body_still_time_out() {
        let mut wire = Vec::new();
        write_frame(&mut wire, &ClientFrame::Inspect { sequence: 7 }).unwrap();
        for prefix in [1, 5] {
            let mut reader = DelayedFrame {
                bytes: Cursor::new(wire[..prefix].to_vec()),
                idle: None,
                stall_at_end: true,
            };
            let error = read_frame_with_timeout::<_, ClientFrame>(
                &mut reader,
                None,
                Duration::from_millis(20),
                Some(&|| true),
            )
            .err()
            .unwrap();
            assert!(error.to_string().contains("timed out"));
        }
    }

    #[test]
    fn authenticated_idle_notices_stop_revocation_and_eof() {
        struct RaiseFlagOnRead<'a>(&'a AtomicBool);
        impl Read for RaiseFlagOnRead<'_> {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                self.0.store(true, Ordering::Release);
                Err(std::io::ErrorKind::WouldBlock.into())
            }
        }
        let stop = AtomicBool::new(false);
        let error =
            read_authenticated_frame::<_, ClientFrame>(&mut RaiseFlagOnRead(&stop), &stop, || true)
                .err()
                .unwrap();
        assert!(error.to_string().contains("stopping"));
        let revoked = AtomicBool::new(false);
        let error = read_authenticated_frame::<_, ClientFrame>(
            &mut RaiseFlagOnRead(&revoked),
            &AtomicBool::new(false),
            || !revoked.load(Ordering::Acquire),
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("revoked"));
        let error = read_authenticated_frame::<_, ClientFrame>(
            &mut Cursor::new(Vec::<u8>::new()),
            &AtomicBool::new(false),
            || true,
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("disconnected"));
    }

    #[test]
    fn setup_encoded_frame_bound_rejects_before_dispatch_and_reply_is_explicit() {
        let oversized = ClientFrame::Setup {
            sequence: 1,
            command: crate::gui_setup::Command::Configure {
                expected_revision: 1,
                document: "x".repeat(MAX_FRAME_BYTES),
            },
        };
        assert!(ensure_setup_frame_size(&oversized).is_err());
        let mut response = ServerFrame {
            reply: Reply {
                acknowledged: true,
                client: 5,
                sequence: 1,
                ..Default::default()
            },
            app_preferences: None,
            setup: Some(crate::gui_setup::Reply {
                client: 5,
                sequence: 1,
                value: serde_json::Value::String("x".repeat(MAX_FRAME_BYTES)),
                error: None,
                profile: None,
            }),
            discovery: None,
        };
        bound_setup_response(&mut response).unwrap();
        let setup = response.setup.unwrap();
        assert_eq!(setup.value, serde_json::Value::Null);
        assert!(setup.error.unwrap().contains("16 MiB"));
    }

    #[test]
    fn oversized_setup_call_preserves_client_sequence_and_writes_no_bytes() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "protoinputmultiplex-setup-preflight-{}-{unique}.tmp",
            std::process::id()
        ));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let mut client = Client {
            pipe: Pipe(file),
            client: 17,
            next_sequence: 1,
            discovery: None,
            kind: DescriptorKind::StoppedSetup,
            discovery_received_at: None,
        };
        let result = client.setup(crate::gui_setup::Command::Configure {
            expected_revision: 1,
            document: "x".repeat(MAX_FRAME_BYTES),
        });
        assert!(result.is_err());
        assert_eq!(client.next_sequence, 1);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        drop(client);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn server_frame_discovery_is_optional_and_roundtrips_without_private_identity() {
        let legacy = ServerFrame {
            reply: Reply::default(),
            app_preferences: None,
            setup: None,
            discovery: None,
        };
        let mut legacy_value = serde_json::to_value(&legacy).unwrap();
        legacy_value.as_object_mut().unwrap().remove("discovery");
        let decoded_legacy: ServerFrame = serde_json::from_value(legacy_value).unwrap();
        assert!(decoded_legacy.discovery.is_none());

        let status = crate::gui_discovery::Status {
            requested_generation: 4,
            completed_generation: 3,
            in_flight: true,
            device_count: 2,
            window_count: 5,
            monitor_count: 1,
            ..Default::default()
        };
        let frame = ServerFrame {
            reply: Reply::default(),
            app_preferences: None,
            setup: None,
            discovery: Some(status),
        };
        let mut wire = serde_json::to_value(&frame).unwrap();
        let legacy_status: crate::gui_discovery::Status =
            serde_json::from_value(wire["discovery"].clone()).unwrap();
        assert!(legacy_status.gui_activity.is_none());
        let probe = crate::gui_activity::Probe::new();
        wire["discovery"]["gui_activity"] =
            serde_json::to_value(probe.observer().snapshot()).unwrap();
        // A reader with the old permissive Status shape accepts the added nested field.
        #[derive(Deserialize)]
        struct LegacyStatus {
            requested_generation: u64,
        }
        let legacy: LegacyStatus = serde_json::from_value(wire["discovery"].clone()).unwrap();
        assert_eq!(legacy.requested_generation, 4);
        for invalid_activity in [serde_json::json!({"version":"malformed"}), {
            let mut future = wire["discovery"]["gui_activity"].clone();
            future["version"] = serde_json::json!(2);
            future
        }] {
            let mut invalid = wire.clone();
            invalid["discovery"]["gui_activity"] = invalid_activity;
            let decoded: ServerFrame = serde_json::from_value(invalid).unwrap();
            assert!(decoded.discovery.unwrap().gui_activity.is_none());
        }
        let decoded: ServerFrame = serde_json::from_value(wire).unwrap();
        let status = decoded.discovery.unwrap();
        assert!(status.gui_activity.is_some());
        assert_eq!(status.requested_generation, 4);
        assert_eq!(status.completed_generation, 3);
        assert!(status.in_flight);
        assert_eq!(status.device_count, 2);
        assert_eq!(status.window_count, 5);
        assert_eq!(status.monitor_count, 1);
        assert!(!serde_json::to_string(&status).unwrap().contains("path"));
    }

    #[test]
    fn published_discovery_ages_from_its_sample_and_publish_time() {
        let status = crate::gui_discovery::Status {
            sample_age_ms: 12,
            age_ms: Some(40),
            ..Default::default()
        };
        let published_at = Instant::now();
        let aged = age_discovery_status(
            status,
            published_at,
            published_at + Duration::from_millis(25),
        );

        assert_eq!(aged.sample_age_ms, 37);
        assert_eq!(aged.age_ms, Some(65));
    }

    #[test]
    fn only_exact_client_sequence_acknowledges_a_request() {
        let expected = Reply {
            acknowledged: true,
            client: 12,
            sequence: 4,
            ..Default::default()
        };
        assert!(is_ack(&expected, 12, 4));
        assert!(!is_ack(&expected, 13, 4));
        assert!(!is_ack(&expected, 12, 5));
        assert!(!is_ack(
            &Reply {
                acknowledged: false,
                ..expected
            },
            12,
            4
        ));
    }

    #[test]
    fn bearer_token_comparison_handles_length_without_early_return() {
        assert!(constant_time_eq("ab12", "ab12"));
        assert!(!constant_time_eq("ab12", "ab13"));
        assert!(!constant_time_eq("ab12", "ab1"));
    }
}

#[cfg(test)]
mod recovery_continuation_tests {
    use super::*;
    use crate::session_recovery::{
        Disclosure, OriginalEnrollment, Phase, Projection, TargetSnapshot, TargetStatus,
    };

    fn worker(client: u64, generation: u64) -> Reply {
        Reply {
            acknowledged: true,
            client,
            sequence: 1,
            policy: agent_runtime::Policy {
                enabled: true,
                user_ids: vec!["user".into()],
                allowed_pids: vec![42],
                ..Default::default()
            },
            clients: vec![1],
            authority: Some(agent_runtime::WorkerAuthority {
                policy_generation: generation,
                active_user_ids: vec!["user".into()],
            }),
            ..Default::default()
        }
    }

    fn sample(generation: u64, status: TargetStatus, known_live: bool) -> Disclosure {
        Disclosure {
            generation,
            projection: Projection {
                phase: if status == TargetStatus::Exited {
                    Phase::Stopped
                } else {
                    Phase::Running
                },
                can_start: status == TargetStatus::Exited,
                targets: vec![TargetSnapshot {
                    pid: 42,
                    status,
                    affected: true,
                    application_name: Some("original.exe".into()),
                }],
            },
            originals: vec![OriginalEnrollment {
                pid: 42,
                token: 700,
                known_live,
            }],
        }
    }

    fn connection() -> AppHandle {
        let (mut app, _) = app_channel();
        app.begin_controller(9);
        app.observe_policy(&worker(0, 1));
        app.admit(1, &worker(1, 1)).unwrap();
        app
    }

    fn inspect(app: &AppHandle, sample: Disclosure) -> Reply {
        let ticket = app.publication_ticket();
        app.publish_recovery(ticket, sample);
        app.public_reply(worker(1, 1), 1, 2, true).unwrap()
    }

    #[test]
    fn pre_grant_or_pre_connection_sample_cannot_certify() {
        let (mut app, _) = app_channel();
        app.begin_controller(9);
        let before_grant = app.publication_ticket();
        app.observe_policy(&worker(0, 1));
        let before_connect = app.publication_ticket();
        app.admit(1, &worker(1, 1)).unwrap();
        for stale in [before_grant, before_connect] {
            app.publish_recovery(stale, sample(11, TargetStatus::Running, true));
            assert!(
                app.public_reply(worker(1, 1), 1, 2, true)
                    .unwrap()
                    .recovery
                    .is_none()
            );
        }
        assert_eq!(
            inspect(&app, sample(11, TargetStatus::Running, true))
                .recovery
                .unwrap()
                .targets
                .len(),
            1
        );
    }

    #[test]
    fn terminal_keeps_only_certified_original_and_no_worker_payload() {
        let app = connection();
        let live = inspect(&app, sample(11, TargetStatus::Running, true));
        assert!(live.authority.is_none());
        app.begin_stop();
        app.terminal();
        app.publish_recovery(
            app.publication_ticket(),
            sample(11, TargetStatus::Exited, false),
        );
        let mut stale = worker(1, 1);
        stale.profile = Some(mux_core::Profile::default());
        stale.error = Some("private/raw/path cleanup secret".into());
        stale.events = vec![session::Event::Failed(
            "private/raw/path cleanup secret".into(),
        )];
        let reply = app.public_reply(stale, 1, 3, true).unwrap();
        assert!(
            reply.terminal
                && reply.events.is_empty()
                && reply.profile.is_none()
                && reply.frame.is_none()
                && reply.error.is_none()
                && reply.cleanup.is_empty()
                && reply.authority.is_none()
        );
        assert!(!reply.policy.enabled);
        let recovery = reply.recovery.as_ref().unwrap();
        assert!(recovery.can_start);
        assert_eq!(recovery.targets[0].status, TargetStatus::Exited);
        assert_eq!(
            recovery.targets[0].application_name.as_deref(),
            Some("original.exe")
        );
        let serialized = serde_json::to_string(&reply).unwrap();
        for private in [
            "policy_generation",
            "active_user_ids",
            "token",
            "private/raw/path",
        ] {
            assert!(!serialized.contains(private));
        }
    }

    #[test]
    fn late_identity_generation_or_partial_users_redacts_rows_before_serialization() {
        let app = connection();
        assert!(
            inspect(&app, sample(11, TargetStatus::Running, false))
                .recovery
                .unwrap()
                .targets
                .is_empty()
        );
        assert_eq!(
            inspect(&app, sample(11, TargetStatus::Running, true))
                .recovery
                .unwrap()
                .targets
                .len(),
            1
        );
        app.begin_stop();
        app.terminal();
        app.publish_recovery(
            app.publication_ticket(),
            sample(12, TargetStatus::Exited, false),
        );
        assert!(
            app.public_reply(Reply::default(), 1, 4, true)
                .unwrap()
                .recovery
                .unwrap()
                .targets
                .is_empty()
        );
        let app = connection();
        {
            app.recovery
                .write()
                .unwrap()
                .active_users
                .push("ungranted".into());
        }
        app.publish_recovery(
            app.publication_ticket(),
            sample(11, TargetStatus::Running, true),
        );
        // Use the exact acknowledged roster so this tests row filtering, not a
        // mismatching worker-authority receipt (which separately revokes).
        let mut reply = worker(1, 1);
        reply
            .authority
            .as_mut()
            .unwrap()
            .active_user_ids
            .push("ungranted".into());
        let filtered = app
            .public_reply(reply, 1, 2, true)
            .unwrap()
            .recovery
            .unwrap();
        assert!(filtered.targets.is_empty());
        assert!(
            !serde_json::to_string(&filtered)
                .unwrap()
                .contains("original.exe")
        );
    }

    #[test]
    fn preferences_transaction_abandoned_reply_cancels_the_original_native_gate() {
        let (mut app, requests) = app_channel();
        app.begin_controller(9);
        app.observe_policy(&worker(0, 1));
        app.admit(1, &worker(1, 1)).unwrap();
        let request = std::thread::spawn(move || {
            app.execute(
                1,
                99,
                agent_runtime::AppPreferencesCommand::Inspect,
                Arc::new(AtomicBool::new(true)),
                &AtomicBool::new(false),
            )
        });
        let queued = requests.recv_timeout(Duration::from_secs(3)).unwrap();
        let decision = queued.decision.clone();
        drop(queued); // GUI/owner abandoned its response, not an activation ACK.
        assert!(
            request
                .join()
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("abandoned")
        );
        assert!(decision.cancelled());
        assert!(!decision.claim());
    }

    #[test]
    fn preferences_transaction_all_publication_revocations_share_the_native_claim_gate() {
        for action in ["stop", "revoke", "policy", "disconnect", "supersede"] {
            for claimed in [false, true] {
                let app = connection();
                let decision = Arc::new(crate::app_lifecycle::CommitDecision::default());
                if claimed {
                    assert!(decision.claim());
                }
                app.recovery.write().unwrap().track_app(1, &decision);
                match action {
                    "stop" => app.begin_stop(),
                    "revoke" => app.revoke(),
                    "policy" => app.prepare_policy(),
                    "disconnect" => app.remove_connection(1),
                    "supersede" => app.recovery.write().unwrap().track_app(1, &Arc::default()),
                    _ => unreachable!(),
                }
                assert_eq!(decision.cancelled(), !claimed, "{action}");
                assert_eq!(decision.claimed(), claimed, "{action}");
            }
        }
    }

    #[test]
    fn stop_closes_forwarding_and_cancels_preferences_without_waiting_for_worker() {
        let app = connection();
        let cancelled = Arc::new(crate::app_lifecycle::CommitDecision::default());
        app.recovery
            .write()
            .unwrap()
            .pending_app
            .insert(1, Arc::downgrade(&cancelled));
        let mut forwarded = 0;
        app.forward(Some(1), || forwarded += 1).unwrap();
        app.begin_stop();
        assert!(cancelled.cancelled());
        for client in [Some(1), None] {
            assert!(app.forward(client, || forwarded += 1).is_err());
        }
        assert_eq!(forwarded, 1);
        assert!(!app.permits_app(1, (0, 0, 0)));
        assert!(app.admit(2, &worker(1, 1)).is_err());
        app.observe_policy(&worker(0, 2));
        assert!(app.stopped()); // late policy ACK cannot reopen Stop
        app.terminal();
        app.remove_connection(1);
        assert!(app.retirement_due());
    }

    #[test]
    fn policy_after_exit_and_new_transport_cannot_inherit_old_certification() {
        let mut app = connection();
        inspect(&app, sample(11, TargetStatus::Running, true));
        app.prepare_policy();
        app.observe_policy(&worker(0, 2));
        assert!(app.public_reply(worker(1, 1), 1, 3, true).is_err());
        app.admit(1, &worker(1, 2)).unwrap();
        app.publish_recovery(
            app.publication_ticket(),
            sample(11, TargetStatus::Exited, false),
        );
        assert!(
            app.public_reply(worker(1, 2), 1, 4, true)
                .unwrap()
                .recovery
                .unwrap()
                .targets
                .is_empty()
        );
        let old_transport = app.clone();
        app.revoke();
        app.prepare_broker();
        old_transport.observe_policy(&worker(0, 3));
        assert!(old_transport.admit(1, &worker(1, 3)).is_err());
        assert!(
            old_transport
                .public_reply(worker(1, 3), 1, 5, true)
                .is_err()
        );
        assert!(app.recovery.read().unwrap().connections.is_empty());
    }

    #[test]
    fn stale_observation_start_fails_closed_even_if_publication_is_recent() {
        let app = connection();
        let mut ticket = app.publication_ticket();
        ticket.3 -= Duration::from_secs(3);
        app.publish_recovery(ticket, sample(11, TargetStatus::Running, true));
        assert!(
            app.public_reply(worker(1, 1), 1, 2, true)
                .unwrap()
                .recovery
                .is_none()
        );
        app.revoke();
        assert!(app.public_reply(worker(1, 1), 1, 3, true).is_err());
        assert!(app.retirement_due());
    }

    #[test]
    fn a_fresh_ticket_cannot_relabel_an_old_native_sample() {
        let app = connection();
        let ticket = app.publication_ticket();
        let old_poll = ticket.3 - Duration::from_millis(1);
        app.publish_recovery_polled(ticket, sample(11, TargetStatus::Running, true), old_poll);
        assert!(
            app.public_reply(worker(1, 1), 1, 2, true)
                .unwrap()
                .recovery
                .is_none()
        );

        let fresh_poll = Instant::now();
        app.publish_recovery_polled(ticket, sample(11, TargetStatus::Exited, false), fresh_poll);
        let projection = app
            .public_reply(worker(1, 1), 1, 3, true)
            .unwrap()
            .recovery
            .unwrap();
        assert!(projection.targets.is_empty());
    }
}
