//! The UI never calls the native loader. A disposable process owns each session.
mod backend_selection;
mod transport;

use crate::{
    routing_engine::Engine,
    session_recovery::{PreparedRecovery, Recovery},
};
use crate::desktop_router::observation as desktop_observation;
use anyhow::{Context, Result, bail, ensure};
use mux_core::Profile;
use mux_windows::{protoinput::Backend, raw_input};
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Write},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

const PREFIX: &str = "MUX_EVENT:";

/// Private GUI/controller correlation, never a client authorization credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsTicket {
    pub request: u64,
    pub identity: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsAcknowledgement {
    pub ticket: SettingsTicket,
    pub index: usize,
    pub settings: mux_core::desktop::CursorSettings,
    pub error: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub enum Request {
    /// Internal direct-controller operation; SharedHandle cannot issue this.
    DesktopObservation(desktop_observation::Command),
    SelectWindow(crate::window_selection::Selection),
    Output(crate::output_runtime::Command),
    Agent(crate::agent_runtime::Command),
    Capture {
        user: usize,
        #[serde(default)]
        query: Option<crate::screen_analysis::Query>,
    },
    VirtualUsers(crate::virtual_users::Command),
    MacroStudio(crate::macro_studio::Command),
    Magnifier(crate::magnifier_runtime::Command),
    Annotation(crate::annotation_runtime::Command),
    Automation(crate::automation_runtime::Command),
    InspectMapperMonitors {
        request_id: u64,
    },
    /// Internal GUI file-load compare-and-dispatch; connected Agent Access cannot issue it.
    MacroFileLoad {
        clip: mux_core::automation::Clip,
        expected_macro_revision: u64,
    },
    Text {
        user: usize,
        text: String,
    },
    Mode {
        mode: mux_core::routing::Mode,
        multi_keyboard: bool,
    },
    /// GUI event-SDK mode request; the ticket never comes from a remote payload.
    HostedMode {
        ticket: u64,
        mode: mux_core::routing::Mode,
        multi_keyboard: bool,
    },
    MoveTo {
        user: usize,
        x: i32,
        y: i32,
    },
    UserPause {
        user: usize,
        paused: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gui: Option<SettingsTicket>,
    },
    Pause,
    Resume,
    Stop,
    Input(mux_core::emulated::Packet),
    Batch(Vec<mux_core::emulated::Packet>),
    ReleaseAll,
    Settings {
        index: usize,
        settings: mux_core::desktop::CursorSettings,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gui: Option<SettingsTicket>,
    },
    Tap {
        device: u32,
        key: u32,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureTarget {
    pub revision: u64,
    pub user_id: String,
    pub window: Option<mux_core::routing::Window>,
}

fn capture_targets(
    users: &crate::virtual_users::Runtime,
    router: &dyn Engine,
    previous: &mut Vec<CaptureTarget>,
) -> Vec<CaptureTarget> {
    let next = users
        .state()
        .slots
        .into_iter()
        .map(|slot| {
            let user_id = slot.seat.identity();
            let window = if slot.active {
                router.selected_window(slot.user).ok().flatten()
            } else {
                None
            };
            let old = previous.iter().find(|old| old.user_id == user_id);
            let revision = old.map_or(1, |old| {
                if old.window == window {
                    old.revision
                } else {
                    old.revision.wrapping_add(1)
                }
            });
            CaptureTarget {
                revision,
                user_id,
                window,
            }
        })
        .collect();
    previous.clone_from(&next);
    next
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    /// Routed by the stdout reader to its separately bounded private inbox.
    DesktopObservation(desktop_observation::Delivery),
    /// Private local GUI target identity; not an authorization credential.
    ScreenTargets(Vec<CaptureTarget>),
    SettingsAcknowledged(SettingsAcknowledgement),
    UserPause(crate::routing_engine::UserPauseState),
    UserPauseStates(Vec<crate::routing_engine::UserPauseState>),
    UserPauseAcknowledged {
        ticket: SettingsTicket,
        state: crate::routing_engine::UserPauseState,
    },
    WindowSelected(crate::window_selection::Acknowledgement),
    Output(crate::output_runtime::State),
    Agent(crate::agent_runtime::State),
    Capture(crate::agent_runtime::CaptureState),
    VirtualUsers(crate::virtual_users::State),
    MacroStudio(crate::macro_studio::State),
    Magnifier(crate::magnifier_runtime::State),
    Annotation(crate::annotation_runtime::State),
    Automation(crate::automation_runtime::State),
    MapperMonitors(crate::mapper_runtime::MonitorReceipt),
    Configured(usize),
    Attaching(u32),
    AttachmentFinished {
        pid: u32,
        error: Option<String>,
    },
    ModeChanged(mux_core::routing::Mode, bool),
    HostedModeCompleted {
        ticket: u64,
        mode: mux_core::routing::Mode,
        multi_keyboard: bool,
        error: Option<String>,
        uncertain: bool,
    },
    Paused,
    Resumed,
    Stopped,
    Failed(String),
    InputSent,
    /// A release failure whose saved input releases remain retryable.
    InputRejected(String),
    /// A read-only prevalidation rejection; no command effects were applied.
    CommandRejected(String),
    /// Emergency coverage was lost; the supervisor is requesting ordinary Stop.
    EmergencyUnavailable(String),
    State(Vec<crate::router::State>),
}

pub struct Controller {
    observation: desktop_observation::Inbox,
    requests: Sender<Request>,
    pub events: Receiver<Event>,
    shared: SharedHandle,
    recovery: Recovery,
    preflight_pending: Arc<AtomicBool>,
}

/// Subscribers do not own the GUI session lifetime. Slow clients are detached.
#[derive(Clone)]
pub struct SharedHandle {
    observation: desktop_observation::Inbox,
    requests: Sender<Request>,
    bus: Arc<Mutex<EventBus>>,
    recovery: Recovery,
    stop_requested: Arc<AtomicBool>,
}
#[derive(Default)]
struct EventBus {
    latest: Vec<Event>,
    subscribers: Vec<((u64, u64), SyncSender<Event>)>,
}
impl SharedHandle {
    pub fn request(&self, request: Request) {
        if matches!(&request, Request::DesktopObservation(_)) { return; }
        if matches!(&request, Request::Stop) {
            self.observation.fence(desktop_observation::LossReason::StopIntent);
            self.recovery.mark_stopping();
            self.stop_requested.store(true, Ordering::Release);
            return;
        }
        let _ = self.requests.send(request);
    }
    pub fn subscribe(&self, client: u64, sequence: u64) -> Receiver<Event> {
        let (tx, rx) = mpsc::sync_channel(256);
        let mut bus = self.bus.lock().unwrap();
        for event in &bus.latest {
            let _ = tx.try_send(event.clone());
        }
        bus.subscribers.push(((client, sequence), tx));
        rx
    }
}
impl EventBus {
    /// Publish to subscribers and return whether the direct controller may receive this event.
    fn publish(&mut self, event: &Event) -> bool {
        if matches!(event, Event::DesktopObservation(_)) { return false; }
        if let Event::Agent(state) = event
            && state.mapper_monitors.is_some()
        {
            if state.acknowledged && state.client != 0 {
                let tag = (state.client, state.sequence);
                self.subscribers.retain(|(subscriber, tx)| {
                    *subscriber != tag || tx.try_send(event.clone()).is_ok()
                });
            }
            return false;
        }
        // Private direct acknowledgements are available only on Controller.events.
        if matches!(
            event,
            Event::SettingsAcknowledged(_)
                | Event::UserPauseAcknowledged { .. }
                | Event::MapperMonitors(_)
                | Event::ScreenTargets(_)
                | Event::HostedModeCompleted { .. }
        ) {
            return true;
        }
        if let Event::AttachmentFinished { pid, .. } = event {
            self.latest
                .retain(|old| !matches!(old, Event::Attaching(opening) if opening == pid));
        }
        if !matches!(
            event,
            Event::UserPause(_)
                | Event::Capture(_)
                | Event::Agent(_)
                | Event::InputSent
                | Event::WindowSelected(_)
                | Event::InputRejected(_)
                | Event::CommandRejected(_)
                | Event::AttachmentFinished { .. }
        ) {
            let kind = std::mem::discriminant(event);
            self.latest
                .retain(|old| std::mem::discriminant(old) != kind);
            self.latest.push(event.clone());
        }
        self.subscribers
            .retain(|(_, tx)| tx.try_send(event.clone()).is_ok());
        true
    }
}

fn run_recovery_relay(
    worker_events: Receiver<Event>,
    gui_output: Sender<Event>,
    bus: Arc<Mutex<EventBus>>,
    relay_recovery: Recovery,
) {
    let interval = Duration::from_millis(100);
    let mut next_poll = Instant::now() + interval;
    let mut disconnected = false;
    loop {
        // A queued terminal event must be delivered before the last
        // Recovery owner exits, even after the supervisor drops its Arc.
        if disconnected && relay_recovery.relay_is_last_owner() {
            break;
        }
        let wait = next_poll.saturating_duration_since(Instant::now());
        if disconnected {
            if !wait.is_zero() {
                thread::sleep(wait);
            }
        } else {
            match worker_events.recv_timeout(wait) {
                Ok(event) => {
                    // Terminal events arrive only after supervise has waited
                    // for worker exit and released its resources.
                    relay_recovery.observe(&event);
                    if bus.lock().unwrap().publish(&event) {
                        let _ = gui_output.send(event);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => disconnected = true,
            }
        }
        if disconnected && relay_recovery.relay_is_last_owner() {
            break;
        }
        // Check after every event: a continuous stream cannot starve polling.
        if Instant::now() >= next_poll {
            relay_recovery.poll_native();
            next_poll = Instant::now() + interval;
        }
    }
}

#[cfg(test)]
mod recovery_relay_tests {
    use super::*;

    #[test]
    fn screen_target_identity_stays_on_the_private_controller_path() {
        let (controller, _, _, _) = Controller::test_fixture();
        let observer = controller.shared.subscribe(11, 2);
        let mut bus = controller.shared.bus.lock().unwrap();
        assert!(bus.publish(&Event::ScreenTargets(vec![CaptureTarget {
            revision: 3,
            user_id: "user-a".into(),
            window: Some(mux_core::routing::Window { pid: 7, hwnd: 11 }),
        }])));
        assert!(matches!(
            observer.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(bus.latest.is_empty());
    }
    #[test]
    fn monitor_receipts_reach_only_their_originating_request() {
        use crate::mapper_runtime::{MonitorReceipt, MonitorSnapshot};

        let (controller, _, _, _) = Controller::test_fixture();
        let exact = controller.shared.subscribe(11, 2);
        let other_client = controller.shared.subscribe(12, 2);
        let other_sequence = controller.shared.subscribe(11, 3);
        let receipt = MonitorReceipt {
            request_id: 7,
            snapshot: MonitorSnapshot {
                revision: 1,
                run: 2,
                samples: Vec::new(),
                traces: vec![mux_core::mapper::ScopeTrace {
                    node: "scope-a".into(),
                    samples: vec![mux_core::mapper::ScopeSample {
                        elapsed_us: 12,
                        value: mux_core::mapper::Value::Hold(true),
                    }],
                }],
            },
        };
        {
            let mut bus = controller.shared.bus.lock().unwrap();
            assert!(bus.publish(&Event::MapperMonitors(receipt.clone())));
            assert!(matches!(exact.try_recv(), Err(mpsc::TryRecvError::Empty)));
            assert!(!bus.publish(&Event::Agent(crate::agent_runtime::State {
                acknowledged: true,
                client: 11,
                sequence: 2,
                mapper_monitors: Some(receipt),
                ..Default::default()
            })));
            assert!(bus.latest.is_empty());
        }
        assert!(matches!(
            exact.try_recv(),
            Ok(Event::Agent(state))
                if state.mapper_monitors.as_ref().is_some_and(|receipt| {
                    receipt.request_id == 7
                        && receipt.snapshot.traces[0].samples[0].elapsed_us == 12
                })
        ));
        for receiver in [
            other_client,
            other_sequence,
            controller.shared.subscribe(11, 2),
        ] {
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::TryRecvError::Empty)
            ));
        }
    }

    #[test]
    fn continuous_events_do_not_starve_recovery_poll_ticks() {
        let recovery = Recovery::starting();
        let relay_owner = recovery.clone();
        let (events_tx, worker_events) = mpsc::channel();
        let (gui_output, gui_events) = mpsc::channel();
        let bus = Arc::new(Mutex::new(EventBus::default()));
        let relay =
            thread::spawn(move || run_recovery_relay(worker_events, gui_output, bus, relay_owner));
        let running = Arc::new(AtomicBool::new(true));
        let producer_running = Arc::clone(&running);
        let producer_tx = events_tx.clone();
        let producer = thread::spawn(move || {
            while producer_running.load(Ordering::Acquire) {
                if producer_tx.send(Event::State(Vec::new())).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
        });
        thread::sleep(Duration::from_millis(350));
        let sampled = recovery.published().native_sample.is_some();
        running.store(false, Ordering::Release);
        producer.join().unwrap();
        events_tx.send(Event::Stopped).unwrap();
        drop(events_tx);
        thread::sleep(Duration::from_millis(130));
        let retained_for_external_owner = !relay.is_finished();
        drop(recovery);
        relay.join().unwrap();
        assert!(sampled, "relay must poll even when events never pause");
        assert!(retained_for_external_owner);
        assert!(
            gui_events
                .try_iter()
                .any(|event| matches!(event, Event::Stopped))
        );
    }
}
impl Controller {
    #[cfg(test)]
    pub(crate) fn test_fixture() -> (Self, Sender<Event>, Receiver<Request>, Arc<AtomicBool>) {
        let observation = desktop_observation::Inbox::new().unwrap();
        let (requests, request_rx) = mpsc::channel();
        let (event_tx, events) = mpsc::channel();
        let recovery = Recovery::default();
        let shared = SharedHandle {
            observation: observation.clone(),
            requests: requests.clone(),
            bus: Arc::new(Mutex::new(EventBus::default())),
            recovery: recovery.clone(),
            stop_requested: Arc::new(AtomicBool::new(false)),
        };
        let stop_requested = Arc::clone(&shared.stop_requested);
        (
            Self {
                observation,
                requests,
                events,
                shared,
                recovery,
                preflight_pending: Arc::new(AtomicBool::new(false)),
            },
            event_tx,
            request_rx,
            stop_requested,
        )
    }

    pub fn start(profile: Profile, runtime: PathBuf) -> Result<Self> {
        Self::start_with_options(profile, runtime, StartOptions::default())
    }

    pub fn start_with_options(
        profile: Profile,
        runtime: PathBuf,
        options: StartOptions,
    ) -> Result<Self> {
        validate_start_options(&options)?;
        let prepared = preflight_start(&profile, &runtime, &options, None)?
            .expect("synchronous preflight cannot be cancelled");
        let recovery = Recovery::from_prepared(prepared.recovery);
        Self::start_supervisor(
            profile,
            options,
            recovery,
            StartPath::Prepared {
                executable: prepared.executable,
                runtime: prepared.runtime,
            },
        )
    }

    /// Start a GUI-owned session immediately and perform all authoritative
    /// preflight on its routing supervisor. The returned Controller owns the
    /// launch until a terminal event, including a Stop received during startup.
    pub fn start_background_with_options(
        profile: Profile,
        runtime: PathBuf,
        options: StartOptions,
    ) -> Result<Self> {
        validate_start_options(&options)?;
        Self::start_supervisor(
            profile,
            options,
            Recovery::starting(),
            StartPath::Background { runtime },
        )
    }

    fn start_supervisor(
        profile: Profile,
        options: StartOptions,
        recovery: Recovery,
        path: StartPath,
    ) -> Result<Self> {
        let observation = desktop_observation::Inbox::new()?;
        let preflight_pending = Arc::new(AtomicBool::new(matches!(
            &path,
            StartPath::Background { .. }
        )));
        let (requests, input) = mpsc::channel();
        let (output, worker_events) = mpsc::channel();
        let (gui_output, events) = mpsc::channel();
        let shared = SharedHandle {
            observation: observation.clone(),
            requests: requests.clone(),
            bus: Arc::new(Mutex::new(EventBus::default())),
            recovery: recovery.clone(),
            stop_requested: Arc::new(AtomicBool::new(false)),
        };
        let bus = shared.bus.clone();
        let relay_recovery = recovery.clone();
        thread::spawn(move || run_recovery_relay(worker_events, gui_output, bus, relay_recovery));
        let supervisor_recovery = recovery.clone();
        let supervisor_observation = observation.clone();
        let stop_requested = shared.stop_requested.clone();
        let supervisor_preflight_pending = preflight_pending.clone();
        thread::Builder::new()
            .name("routing-supervisor".into())
            .spawn(move || {
                let result = match path {
                    StartPath::Prepared {
                        executable,
                        runtime,
                    } => supervise(
                        executable,
                        runtime,
                        profile,
                        input,
                        &output,
                        options,
                        SupervisorControl {
                            observation: supervisor_observation.clone(),
                            recovery: supervisor_recovery.clone(),
                            stop_requested: stop_requested.clone(),
                            prepared_recovery: None,
                        },
                    ),
                    StartPath::Background { runtime } => {
                        let prepared =
                            preflight_start(&profile, &runtime, &options, Some(&stop_requested));
                        supervisor_preflight_pending.store(false, Ordering::Release);
                        match prepared {
                            Ok(Some(prepared)) => supervise(
                                prepared.executable,
                                prepared.runtime,
                                profile,
                                input,
                                &output,
                                options,
                                SupervisorControl {
                                    observation: supervisor_observation.clone(),
                                    recovery: supervisor_recovery.clone(),
                                    stop_requested: stop_requested.clone(),
                                    prepared_recovery: Some(prepared.recovery),
                                },
                            ),
                            Ok(None) => {
                                supervisor_recovery.cancel_prelaunch();
                                let _ = output.send(Event::Stopped);
                                Ok(())
                            }
                            Err(error) => Err(error),
                        }
                    }
                };
                if let Err(error) = result {
                    supervisor_observation.fence(desktop_observation::LossReason::Eof);
                    let _ = output.send(Event::Failed(format!("{error:#}")));
                }
            })?;
        Ok(Self {
            observation,
            requests,
            events,
            shared,
            recovery,
            preflight_pending,
        })
    }
    pub fn shared_handle(&self) -> SharedHandle {
        self.shared.clone()
    }
    pub fn recovery(&self) -> Recovery {
        self.recovery.clone()
    }
    /// Whether the GUI-owned routing supervisor is still doing startup preflight.
    pub fn preflight_pending(&self) -> bool {
        self.preflight_pending.load(Ordering::Acquire)
    }
    pub fn desktop_observation_generation(&self) -> u64 { self.observation.generation() }
    pub fn desktop_observation_live(&self, tag: desktop_observation::BindingTag) -> bool { self.observation.live(tag) }
    pub fn take_desktop_observations(&self) -> Vec<desktop_observation::Delivery> { self.observation.take() }
    pub fn bind_desktop_observation(&self, request: u64, seat_identity: String) -> Result<desktop_observation::BindingTag, desktop_observation::BindRejection> {
        if seat_identity.is_empty() || seat_identity.len() > 128 || seat_identity.chars().any(char::is_control) { return Err(desktop_observation::BindRejection::MissingSeat); }
        let tag = self.observation.begin(request)?;
        if self.requests.send(Request::DesktopObservation(desktop_observation::Command::Bind { request, tag, seat_identity })).is_err() {
            self.observation.fence(desktop_observation::LossReason::Eof);
            return Err(desktop_observation::BindRejection::StaleGeneration);
        }
        Ok(tag)
    }
    pub fn unbind_desktop_observation(&self, tag: desktop_observation::BindingTag) {
        if self.observation.cancel(tag, desktop_observation::LossReason::Unbound)
            && self.requests.send(Request::DesktopObservation(desktop_observation::Command::Unbind { tag })).is_err() {
            self.observation.fence(desktop_observation::LossReason::Eof);
        }
    }
    pub fn request(&self, request: Request) {
        if matches!(&request, Request::DesktopObservation(_)) { return; }
        if matches!(&request, Request::Stop) {
            self.observation.fence(desktop_observation::LossReason::StopIntent);
            self.recovery.mark_stopping();
            self.shared.stop_requested.store(true, Ordering::Release);
            return;
        }
        let _ = self.requests.send(request);
    }
}

#[derive(Debug, Clone, Default)]
pub struct StartOptions {
    /// The GUI app-lifetime hotkey service owns emergency-stop registration.
    pub app_hotkeys: bool,
    /// Bounded diagnostic delay before background preflight.
    #[doc(hidden)]
    pub preflight_delay: Duration,
    #[doc(hidden)]
    pub emergency_check: Option<crate::app_lifecycle::EmergencyCheck>,
}

struct PreparedStart {
    executable: PathBuf,
    runtime: PathBuf,
    recovery: PreparedRecovery,
}

enum StartPath {
    Prepared {
        executable: PathBuf,
        runtime: PathBuf,
    },
    Background {
        runtime: PathBuf,
    },
}

fn validate_start_options(options: &StartOptions) -> Result<()> {
    ensure!(
        options.preflight_delay <= Duration::from_secs(15),
        "routing preflight delay cannot exceed 15 seconds"
    );
    Ok(())
}

fn start_cancelled(stop_requested: Option<&AtomicBool>) -> bool {
    stop_requested.is_some_and(|stop| stop.load(Ordering::Acquire))
}

fn wait_preflight_delay(delay: Duration, stop_requested: Option<&AtomicBool>) -> bool {
    let deadline = Instant::now() + delay;
    while Instant::now() < deadline {
        if start_cancelled(stop_requested) {
            return false;
        }
        thread::sleep(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(10)),
        );
    }
    !start_cancelled(stop_requested)
}

/// Authoritative read-only startup validation. The GUI invokes this from its
/// routing supervisor; the synchronous entry points retain the same checks.
fn preflight_start(
    profile: &Profile,
    runtime: &Path,
    options: &StartOptions,
    stop_requested: Option<&AtomicBool>,
) -> Result<Option<PreparedStart>> {
    if start_cancelled(stop_requested)
        || !wait_preflight_delay(options.preflight_delay, stop_requested)
    {
        return Ok(None);
    }
    if let Some(emergency_check) = &options.emergency_check {
        let result = emergency_check.ensure();
        if start_cancelled(stop_requested) {
            return Ok(None);
        }
        result?;
    }
    if profile.seats.is_empty() {
        if start_cancelled(stop_requested) {
            return Ok(None);
        }
        bail!("Add at least one user before starting routing");
    }
    if start_cancelled(stop_requested) {
        return Ok(None);
    }

    let devices = raw_input::devices();
    if start_cancelled(stop_requested) {
        return Ok(None);
    }
    let devices = devices?;
    let seats = profile.resolve(&devices);
    if start_cancelled(stop_requested) {
        return Ok(None);
    }
    let seats = seats?;
    let target_validation = mux_windows::targets::require_start_targets(
        &start_target_pids(profile),
        seats.iter().all(|seat| {
            let virtual_device =
                |device| device & mux_core::emulated::MASK == mux_core::emulated::BASE;
            virtual_device(seat.mouse) && seat.keyboard.is_none_or(virtual_device)
        }),
    );
    if start_cancelled(stop_requested) {
        return Ok(None);
    }
    target_validation?;
    if profile
        .seats
        .iter()
        .any(|seat| seat.target_pid == std::process::id())
    {
        if start_cancelled(stop_requested) {
            return Ok(None);
        }
        bail!("cannot attach to this control center");
    }
    if start_cancelled(stop_requested) {
        return Ok(None);
    }

    let executable = std::env::current_exe();
    if start_cancelled(stop_requested) {
        return Ok(None);
    }
    let executable = executable?;
    let runtime = runtime.canonicalize();
    if start_cancelled(stop_requested) {
        return Ok(None);
    }
    let runtime = runtime.context("runtime folder is missing; build the backend first")?;

    // Open process identities outside Recovery's shared mutex. The prepared
    // registry is installed only at the worker/profile-delivery boundary.
    let recovery = Recovery::prepare_profile(profile);
    if start_cancelled(stop_requested) {
        return Ok(None);
    }
    Ok(Some(PreparedStart {
        executable,
        runtime,
        recovery,
    }))
}

impl Drop for Controller {
    fn drop(&mut self) {
        self.request(Request::Stop);
    }
}
struct Worker(
    Child,
    Option<thread::JoinHandle<()>>,
    Option<transport::Transport>,
);
impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(transport) = &mut self.2 {
            transport.close();
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
        // A blocked pipe write is released by exact worker/job exit. Joining
        // before killing/waiting would recreate the supervisor deadlock.
        if let Some(transport) = &mut self.2 {
            transport.join_after_worker_exit();
        }
        // EOF follows child/job cleanup. Join before publishing a terminal event
        // so a buffered attachment attempt cannot arrive after quarantine clears.
        if let Some(reader) = self.1.take() {
            let _ = reader.join();
        }
    }
}

/// Required starting targets only. An automatic allowlist is potential scope,
/// not a request to attach every listed application.
pub(crate) fn start_target_pids(profile: &Profile) -> Vec<u32> {
    profile
        .seats
        .iter()
        .map(|seat| seat.target_pid)
        .filter(|pid| *pid != 0)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub(crate) fn virtual_only(profile: &Profile) -> bool {
    profile.seats.iter().all(|seat| {
        matches!(mux_core::emulated::parse(&seat.mouse), Ok(Some(_)))
            && seat
                .keyboard
                .as_ref()
                .is_none_or(|path| matches!(mux_core::emulated::parse(path), Ok(Some(_))))
    })
}

fn commit_terminal(
    event: &Event,
    worker_success: bool,
    commit: impl FnOnce() -> Result<()>,
) -> Result<()> {
    if matches!(event, Event::Stopped) {
        ensure!(
            worker_success,
            "routing worker exited unsuccessfully after reporting stop"
        );
        commit().context("commit acknowledged cleanup for all session targets")?;
    }
    Ok(())
}

fn stop_deadline(recovery: &Recovery) -> Instant {
    let affected = recovery
        .snapshot()
        .targets
        .iter()
        .filter(|target| target.affected)
        .count() as u64;
    // Each touched process has its own bounded queue fence and native retire
    // acknowledgement. Keep the supervisor alive for those real cleanup steps.
    Instant::now() + Duration::from_secs(affected.saturating_mul(6).saturating_add(5).max(30))
}

struct SupervisorControl {
    observation: desktop_observation::Inbox,
    recovery: Recovery,
    stop_requested: Arc<AtomicBool>,
    prepared_recovery: Option<PreparedRecovery>,
}

fn supervise(
    executable: PathBuf,
    runtime: PathBuf,
    profile: Profile,
    input: Receiver<Request>,
    output: &Sender<Event>,
    options: StartOptions,
    control: SupervisorControl,
) -> Result<()> {
    let SupervisorControl {
        observation,
        recovery,
        stop_requested,
        mut prepared_recovery,
    } = control;
    if stop_requested.load(Ordering::Acquire) {
        recovery.cancel_prelaunch();
        let _ = output.send(Event::Stopped);
        return Ok(());
    }
    let targets = mux_windows::targets::ProcessWatch::open(
        profile
            .seats
            .iter()
            .filter(|_| profile.desktop.is_none())
            .map(|s| s.target_pid),
    )?;
    if stop_requested.load(Ordering::Acquire) {
        drop(targets);
        recovery.cancel_prelaunch();
        let _ = output.send(Event::Stopped);
        return Ok(());
    }
    let mut emergency = if !options.app_hotkeys && (profile.concurrent || profile.desktop.is_some())
    {
        Some(crate::emergency_stop::Subscription::connect()?)
    } else {
        None
    };
    if stop_requested.load(Ordering::Acquire) {
        drop(emergency);
        drop(targets);
        recovery.cancel_prelaunch();
        let _ = output.send(Event::Stopped);
        return Ok(());
    }
    // Every target retains the same Pending record. Only the supervisor may
    // commit it, after all cleanup acknowledgements and successful worker exit.
    let commitment = mux_windows::lifecycle::CommitOwner::create()?;
    if stop_requested.load(Ordering::Acquire) {
        drop(emergency);
        drop(targets);
        recovery.cancel_prelaunch();
        let _ = output.send(Event::Stopped);
        return Ok(());
    }
    let mut worker = Worker(
        Command::new(executable)
            .env(desktop_observation::GENERATION_ENV, observation.generation().to_string())
            .arg("session-worker")
            .arg("--runtime")
            .arg(runtime)
            .arg("--controller-pid")
            .arg(std::process::id().to_string())
            .env(mux_windows::lifecycle::WORKER_ENV, commitment.name())
            .creation_flags(0x08000000) // CREATE_NO_WINDOW: helper never opens a console.
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("start routing worker")?,
        None,
        None,
    );
    let stdin = worker.0.stdin.take().context("worker stdin missing")?;
    // Attach before sending the profile: the worker cannot inject before it is owned by this job.
    let job = mux_windows::job::Job::own(&worker.0)?;
    let stdout = worker.0.stdout.take().context("worker stdout missing")?;
    let (messages, incoming) = mpsc::channel();
    let reader_recovery = recovery.clone();
    let reader_observation = observation.clone();
    worker.1 = Some(thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                break;
            };
            if let Some(json) = line.strip_prefix(PREFIX)
                && let Ok(event) = serde_json::from_str::<Event>(json)
            {
                if let Event::DesktopObservation(delivery) = event {
                    reader_observation.receive(delivery);
                    continue;
                }
                if matches!(&event, Event::Stopped | Event::Failed(_)) { reader_observation.fence(desktop_observation::LossReason::Eof); }
                if let Event::Attaching(pid) = &event {
                    reader_recovery.observe_attempt(*pid);
                }
                // Even if the supervisor is unwinding, drain through EOF:
                // later buffered Attaching records still affect quarantine.
                let _ = messages.send(event);
            }
            else if line.starts_with(PREFIX) { reader_observation.fence(desktop_observation::LossReason::Sequence); }
        }
        reader_observation.fence(desktop_observation::LossReason::Eof);
    }));
    let startup_deadline = Instant::now() + Duration::from_secs(45 * profile.seats.len() as u64);
    let serialized_profile = serde_json::to_string(&profile)?;
    worker.2 = Some(transport::Transport::start(stdin)?);
    if stop_requested.load(Ordering::Acquire) {
        worker.2.as_mut().unwrap().close();
        drop(worker);
        drop(job);
        drop(emergency);
        drop(targets);
        recovery.cancel_prelaunch();
        let _ = output.send(Event::Stopped);
        return Ok(());
    }
    if let Some(prepared) = prepared_recovery.take() {
        recovery.install_prepared(prepared);
    }
    if stop_requested.load(Ordering::Acquire) {
        worker.2.as_mut().unwrap().close();
        drop(worker);
        drop(job);
        drop(emergency);
        drop(targets);
        recovery.cancel_prelaunch();
        let _ = output.send(Event::Stopped);
        return Ok(());
    }
    worker
        .2
        .as_mut()
        .unwrap()
        .enqueue(serialized_profile, startup_deadline)?;
    worker.2.as_mut().unwrap().poll(Instant::now())?;
    let mut deadline = Some(startup_deadline);
    let mut agent_deadlines = std::collections::BTreeMap::new();
    let mut stopping = false;
    loop {
        let emergency_stop = if !stopping {
            match emergency.as_mut().map(|guard| guard.poll()).transpose() {
                Ok(requested) => requested.unwrap_or(false),
                Err(error) => {
                    let _ = output.send(Event::EmergencyUnavailable(format!(
                        "Emergency stop unavailable; stopping routing: {error:#}"
                    )));
                    true
                }
            }
        } else {
            false
        };
        if !stopping && (emergency_stop || stop_requested.load(Ordering::Acquire)) {
            observation.fence(desktop_observation::LossReason::StopIntent);
            recovery.mark_stopping();
            stopping = true;
            let due = stop_deadline(&recovery);
            deadline = Some(due);
            worker
                .2
                .as_mut()
                .unwrap()
                .stop(serde_json::to_string(&Request::Stop)?, due)?;
        }
        worker.2.as_mut().unwrap().poll(Instant::now())?;
        if let Some(pid) = targets.exited() {
            bail!(
                "target PID {pid} exited; routing stopped. Restart remaining target applications before retrying"
            );
        }
        for event in incoming.try_iter() {
            let terminal = matches!(event, Event::Stopped | Event::Failed(_));
            if let Event::Agent(state) = &event
                && state.acknowledged
                && state.client != 0
            {
                agent_deadlines.remove(&(state.client, state.sequence));
            }
            if matches!(event, Event::Attaching(_)) {
                for due in agent_deadlines.values_mut() {
                    *due = Instant::now() + Duration::from_secs(45);
                }
            }
            // A late reply to a previous command must not cancel the stop deadline.
            let telemetry = matches!(
                &event,
                Event::State(_) | Event::UserPauseStates(_) | Event::ScreenTargets(_)
            ) || matches!(&event, Event::AttachmentFinished { .. })
                || matches!(&event, Event::Agent(state) if state.client != 0 || !state.acknowledged)
                || matches!(&event, Event::MacroStudio(state) if !state.acknowledged)
                || matches!(&event, Event::Magnifier(state) if !state.acknowledged)
                || matches!(&event, Event::Annotation(state) if !state.acknowledged)
                || matches!(&event, Event::Output(state) if !state.acknowledged)
                || matches!(&event, Event::VirtualUsers(state) if !state.acknowledged)
                || matches!(&event, Event::Automation(state) if !state.acknowledged);
            if !stopping && !telemetry {
                deadline = if matches!(event, Event::Attaching(_)) {
                    deadline.map(|_| Instant::now() + Duration::from_secs(45))
                } else {
                    None
                };
            }
            if terminal {
                worker.2.as_mut().unwrap().close();
                let status = wait_for_worker_exit(&mut worker.0)?;
                drop(job);
                drop(emergency);
                drop(targets);
                drop(worker);
                commit_terminal(&event, status.success(), || commitment.commit())?;
                let _ = output.send(event);
                return Ok(());
            }
            let _ = output.send(event);
        }
        if let Some(status) = worker.0.try_wait()? {
            // Drain messages produced just before process exit.
            while let Ok(event) = incoming.recv_timeout(Duration::from_millis(50)) {
                let terminal = matches!(event, Event::Stopped | Event::Failed(_));
                if terminal {
                    worker.2.as_mut().unwrap().close();
                    drop(job);
                    drop(emergency);
                    drop(targets);
                    drop(worker);
                    commit_terminal(&event, status.success(), || commitment.commit())?;
                    let _ = output.send(event);
                    return Ok(());
                }
                let _ = output.send(event);
            }
            bail!("routing worker exited ({status}); restart target applications before retrying");
        }
        if deadline.is_some_and(|d| Instant::now() >= d)
            || (!stopping && agent_deadlines.values().any(|due| Instant::now() >= *due))
        {
            bail!(
                "native routing call timed out; worker terminated. Restart target applications before retrying"
            );
        }
        if !worker.2.as_ref().unwrap().ready() {
            // Keep the single pending frame instead of failing normal bursts or
            // pulling more requests into an unbounded transport outbox. Stop is
            // independently visible through stop_requested on the next tick.
            thread::sleep(Duration::from_millis(20));
            continue;
        }
        match input.recv_timeout(Duration::from_millis(20)) {
            Ok(request) => {
                if stopping {
                    continue;
                }
                stopping = matches!(request, Request::Stop);
                if stopping {
                    recovery.mark_stopping();
                }
                let due = if stopping {
                    stop_deadline(&recovery)
                } else if matches!(&request, Request::Agent(_)) {
                    Instant::now() + Duration::from_secs(45)
                } else {
                    Instant::now() + Duration::from_secs(10)
                };
                if let Request::Agent(command) = &request
                    && let Some(tag) = command.tag()
                {
                    agent_deadlines.insert(tag, due);
                } else if !matches!(&request, Request::DesktopObservation(_)) {
                    deadline = Some(due);
                }
                worker
                    .2
                    .as_mut()
                    .unwrap()
                    .enqueue(serde_json::to_string(&request)?, due)?;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) if !stopping => {
                recovery.mark_stopping();
                stopping = true;
                let due = stop_deadline(&recovery);
                deadline = Some(due);
                worker
                    .2
                    .as_mut()
                    .unwrap()
                    .stop(serde_json::to_string(&Request::Stop)?, due)?;
            }
            Err(_) => {
                if stopping {
                    thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }
}

fn wait_for_worker_exit(worker: &mut Child) -> Result<std::process::ExitStatus> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = worker.try_wait()? {
            return Ok(status);
        }
        ensure!(
            Instant::now() < deadline,
            "routing worker did not exit after terminal event"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

pub(crate) fn emit(event: Event) -> Result<()> {
    let json = serde_json::to_string(&event)?;
    if matches!(&event, Event::DesktopObservation(_)) && json.len() > desktop_observation::MAX_DELIVERY_JSON_BYTES {
        println!("{PREFIX}{}", serde_json::to_string(&Event::DesktopObservation(desktop_observation::Delivery::Lost { tag: None, reason: desktop_observation::LossReason::Overflow }))?);
    } else { println!("{PREFIX}{json}"); }
    std::io::stdout().flush()?;
    Ok(())
}
fn input_ack(result: Result<()>, automation: &crate::macro_studio::Runtime) -> Result<()> {
    match result {
        Ok(()) => emit(Event::InputSent),
        Err(error) if release_rejection(&error).is_some() => {
            emit(release_rejection(&error).unwrap())
        }
        Err(error)
            if error
                .downcast_ref::<crate::replication_queue::Overflow>()
                .is_some() =>
        {
            let mut state = automation.state(false);
            state.acknowledged = true;
            state.error = Some(format!("{error:#}"));
            emit(Event::Automation(state))
        }
        Err(error) => Err(error),
    }
}
fn emit_desktop_observations(router: &mut dyn Engine, signals: &WorkerIoSignals) -> Result<()> {
    let operation = if signals.cancel.load(Ordering::Acquire) {
        desktop_observation::Operation::Fence(if signals.ordinary_stop() { desktop_observation::LossReason::StopIntent } else { desktop_observation::LossReason::Eof })
    } else { desktop_observation::Operation::Drain };
    for delivery in router.desktop_observation(operation) { emit(Event::DesktopObservation(delivery))?; }
    Ok(())
}
/// Validation is a command rejection; only failures after validation enter the
/// existing effect-failure acknowledgement/worker-termination path.
fn dispatch_settings(validation: Result<()>, apply: impl FnOnce() -> Result<()>) -> Result<Event> {
    if let Err(error) = validation {
        return Ok(Event::CommandRejected(format!("{error:#}")));
    }
    apply()?;
    Ok(Event::InputSent)
}

fn correlate_settings(
    ticket: Option<SettingsTicket>,
    index: usize,
    settings: mux_core::desktop::CursorSettings,
    event: Event,
) -> Event {
    let Some(ticket) = ticket else {
        return event;
    };
    let error = match event {
        Event::InputSent => None,
        Event::CommandRejected(error) => Some(error),
        _ => unreachable!("only settings dispatch results are correlated"),
    };
    Event::SettingsAcknowledged(SettingsAcknowledgement {
        ticket,
        index,
        settings,
        error,
    })
}
fn validate_settings_ticket(ticket: Option<&SettingsTicket>, identity: &str) -> Result<()> {
    if let Some(ticket) = ticket {
        ensure!(
            ticket.request != 0 && ticket.identity == identity,
            "Cursor settings request no longer matches this user"
        );
    }
    Ok(())
}
fn dispatch_input_packet(
    packet: mux_core::emulated::Packet,
    releases: &mut Vec<(Instant, mux_core::emulated::Packet)>,
    apply: impl FnOnce(mux_core::emulated::Packet) -> Result<()>,
) -> Result<()> {
    if packet.kind == 1 {
        releases
            .retain(|(_, pending)| pending.device != packet.device || pending.data != packet.data);
    }
    apply(packet)
}
fn release_rejection(error: &anyhow::Error) -> Option<Event> {
    error
        .downcast_ref::<crate::router::ReleaseFailure>()
        .map(|_| Event::InputRejected(format!("{error:#}")))
}
/// Short publication lock only; no pipe operation or cleanup runs under it.
#[derive(Default)]
struct WorkerIoState {
    stop_requested: bool,
    cleanup: bool,
    disconnected: bool,
}

#[derive(Default)]
struct WorkerIoSignals {
    cancel: Arc<AtomicBool>,
    state: Mutex<WorkerIoState>,
}

impl WorkerIoSignals {
    fn request_stop(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.stop_requested = true;
        if !state.cleanup {
            self.cancel.store(true, Ordering::Release);
        }
    }

    fn ordinary_stop(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.stop_requested && !state.disconnected
    }

    fn step(&self, operation: impl FnOnce() -> Result<bool>) -> Result<bool> {
        if self.ordinary_stop() {
            return Ok(true);
        }
        match operation() {
            Err(error)
                if self.ordinary_stop()
                    && mux_windows::protoinput::is_cancelled_before_transfer(&error) =>
            {
                Ok(true)
            }
            result => result,
        }
    }

    fn disconnect(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.disconnected = true;
        self.cancel.store(true, Ordering::Release);
    }

    fn begin_cleanup(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.cleanup = true;
        // Ordinary Stop permits bounded release/retirement work. EOF remains an
        // unclean cancellation, including when it raced the queued Stop line.
        self.cancel.store(state.disconnected, Ordering::Release);
    }
}

fn worker_backend(
    runtime: &Path,
    cancel: Arc<AtomicBool>,
    profile: &Profile,
    seats: &[mux_core::ResolvedSeat],
) -> Result<Backend> {
    let requested = std::env::var_os("PROTOINPUTMULTIPLEX_CONTROL_BACKEND");
    let supervised = std::env::var_os(mux_windows::lifecycle::WORKER_ENV).is_some();
    backend_selection::construct(
        requested.as_deref(),
        supervised,
        profile,
        seats,
        || Backend::load(runtime),
        || Backend::load_rust_virtual(runtime, cancel),
    )
}

pub fn run(runtime: &Path, controller_pid: u32) -> Result<()> {
    let result = (|| -> Result<()> {
        let (tx, rx) = mpsc::channel();
        let io_signals = Arc::new(WorkerIoSignals::default());
        let reader_signals = io_signals.clone();
        thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                if line.as_ref().is_ok_and(|text| {
                    matches!(serde_json::from_str::<Request>(text), Ok(Request::Stop))
                }) {
                    reader_signals.request_stop();
                    let _ = emit(Event::DesktopObservation(desktop_observation::Delivery::Lost { tag: None, reason: desktop_observation::LossReason::StopIntent }));
                }
                if tx.send(line).is_err() {
                    break;
                }
            }
            reader_signals.disconnect();
            let _ = emit(Event::DesktopObservation(desktop_observation::Delivery::Lost { tag: None, reason: desktop_observation::LossReason::Eof }));
        });
        let mut profile: Profile = serde_json::from_str(&rx.recv().context("missing profile")??)?;
        profile.normalize_ids()?;
        for seat in &profile.seats {
            mux_windows::monitors::validate_settings(&seat.cursor)?;
            crate::magnifier_runtime::validate_settings(&seat.magnifier)?;
        }
        let seats = profile.resolve(&raw_input::devices()?)?;
        // Check all output destinations before any selected process is attached.
        let output_rules = crate::output_runtime::Rules::new(&profile, seats.clone())?;
        let mut virtual_users =
            crate::virtual_users::Runtime::new(&profile, seats.clone(), controller_pid)?;
        let backend = worker_backend(runtime, io_signals.cancel.clone(), &profile, &seats)?;
        let router: Box<dyn crate::routing_engine::Engine> = if profile.desktop.is_some() {
            ensure!(
                controller_pid != 0,
                "automatic desktop routing requires a controller PID"
            );
            let guard = mux_windows::overlay::DesktopGuard::cursor_thread()?;
            let backend = backend.desktop_session(seats.clone())?;
            Box::new(crate::desktop_router::DesktopRouter::new(
                backend,
                &profile,
                seats.clone(),
                controller_pid,
                guard,
            )?)
        } else {
            let guard = if profile.concurrent {
                Some(mux_windows::overlay::DesktopGuard::cursor_thread()?)
            } else {
                None
            };
            let windows = crate::router::Router::preflight(&profile, &seats)?;
            let session = backend.attach(&seats)?;
            Box::new(crate::router::Router::new(
                session,
                &profile,
                seats.clone(),
                windows,
                guard,
            )?)
        };
        let router = crate::annotation_runtime::Runtime::new(router, &profile, seats.clone())?;
        let mut router = crate::magnifier_runtime::Runtime::new(router, &profile, seats.clone())?;
        emit(Event::Configured(seats.len()))?;
        emit(Event::VirtualUsers(virtual_users.state()))?;
        emit(Event::Annotation(router.annotation_state()))?;
        emit(Event::Magnifier(router.magnifier_state()))?;
        let mut automation = crate::macro_studio::Runtime::new_with_ids(
            seats.clone(),
            profile.seats.iter().map(mux_core::Seat::identity).collect(),
        );
        automation.initialize_output(output_rules);
        automation.initialize_mapper_profile(&profile, router.as_mut())?;
        emit(Event::Output(automation.output_state()))?;
        if !profile.replication.edges.is_empty() {
            automation.command(
                crate::automation_runtime::Command::Graph {
                    edges: profile.replication.resolve(&profile.seats)?,
                    expected_revision: 0,
                },
                router.as_mut(),
            )?;
        }
        if !profile.forwarding.edges.is_empty() {
            automation.command(
                crate::automation_runtime::Command::Forwarding {
                    edges: profile.forwarding.resolve(&profile.seats)?,
                    expected_revision: 0,
                },
                router.as_mut(),
            )?;
        }
        emit(Event::MacroStudio(automation.studio_state(None)))?;
        emit(Event::Automation(automation.state(false)))?;
        let mut agents = crate::agent_runtime::Runtime::new();
        let mut releases: Vec<(Instant, mux_core::emulated::Packet)> = Vec::new();
        let mut last_tick = Instant::now() - Duration::from_secs(1);
        let mut screen_targets = Vec::new();
        loop {
            let background = io_signals.step(|| -> Result<bool> {
                router.poll_motion()?;
                let now = Instant::now();
                let mut remaining = Vec::new();
                for (due, packet) in releases.drain(..) {
                    if virtual_users.require_device(packet.device).is_err() {
                        continue;
                    }
                    if due <= now {
                        if let Err(error) = automation.packet(packet, router.as_mut(), true) {
                            if release_rejection(&error).is_some() {
                                // Retry only the saved UP; a failed release must not
                                // kill healthy users or turn into a new action.
                                remaining.push((now + Duration::from_millis(250), packet));
                                let mut state = automation.state(false);
                                state.error = Some(format!("{error:#}"));
                                emit(Event::Automation(state))?;
                            } else if error
                                .downcast_ref::<crate::replication_queue::Overflow>()
                                .is_some()
                            {
                                let mut state = automation.state(false);
                                state.error = Some(format!("{error:#}"));
                                emit(Event::Automation(state))?;
                            } else {
                                return Err(error);
                            }
                        }
                    } else {
                        remaining.push((due, packet));
                    }
                }
                releases = remaining;
                if let Err(error) = automation.tick(router.as_mut()) {
                    let cleanup = if let Some(scope) =
                        error.downcast_ref::<crate::macro_studio::AgentScopeViolation>()
                    {
                        automation.cancel_agent(scope.client, router.as_mut())
                    } else if error
                        .downcast_ref::<crate::replication_queue::Overflow>()
                        .is_none()
                    {
                        automation.cancel(router.as_mut())
                    } else {
                        Ok(())
                    };
                    let detail = match cleanup {
                        Ok(()) => format!("{error:#}"),
                        Err(cleanup) if release_rejection(&cleanup).is_some() => {
                            format!("{error:#}; cleanup remains pending: {cleanup:#}")
                        }
                        Err(cleanup) => return Err(cleanup),
                    };
                    let mut state = automation.state(false);
                    state.error = Some(detail.clone());
                    emit(Event::Automation(state))?;
                    let mut studio = automation.studio_state(None);
                    studio.error = Some(detail);
                    emit(Event::MacroStudio(studio))?;
                }
                if last_tick.elapsed() >= Duration::from_millis(50) {
                    let (stop, states) = router.tick()?;
                    if stop {
                        return Ok(true);
                    }
                    if let Some(event) = agents.poll_cleanup(now, &mut automation, router.as_mut())
                    {
                        emit(event)?;
                    }
                    if profile.concurrent || profile.desktop.is_some() {
                        emit(Event::VirtualUsers(virtual_users.state()))?;
                        emit(Event::Annotation(router.annotation_state()))?;
                        emit(Event::Magnifier(router.magnifier_state()))?;
                        emit(Event::MacroStudio(automation.studio_state(None)))?;
                        emit(Event::ScreenTargets(capture_targets(
                            &virtual_users,
                            &router,
                            &mut screen_targets,
                        )))?;
                        emit(Event::State(states))?;
                        emit(Event::Automation(automation.state(false)))?;
                    }
                    emit(Event::UserPauseStates(
                        automation.user_pause_states(&router),
                    ))?;
                    last_tick = Instant::now();
                }
                if let Some(event) =
                    agents.poll_capture(&virtual_users, &automation, &mut router, &profile)?
                {
                    emit(event)?;
                }
                if let Some(event) =
                    agents.poll_export(&virtual_users, &automation, &mut router, &profile)?
                {
                    emit(event)?;
                }
                if let Some(event) = agents.poll_annotation_export(
                    &virtual_users,
                    &automation,
                    &mut router,
                    &profile,
                )? {
                    emit(event)?;
                }
                Ok(false)
            });
            if background? {
                for delivery in router.desktop_observation(desktop_observation::Operation::Fence(desktop_observation::LossReason::StopIntent)) { emit(Event::DesktopObservation(delivery))?; }
                io_signals.begin_cleanup();
                agents.cancel_annotation_export();
                break;
            }
            emit_desktop_observations(router.as_mut(), &io_signals)?;
            // Wake for the oldest pending native publication, even when input is idle.
            let wait = router
                .motion_deadline()
                .map_or(Duration::from_millis(10), |due| {
                    due.saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(10))
                });
            let line = match rx.recv_timeout(wait) {
                Ok(line) => line?,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => bail!("controller transport closed before an acknowledged Stop"),
            };
            let dispatched = io_signals.step(|| -> Result<bool> {
                let request = serde_json::from_str::<Request>(&line)?;
                let user_check = match &request {
                    Request::Text { user, .. } | Request::MoveTo { user, .. } => {
                        virtual_users.require_active(*user).map(|_| ())
                    }
                    Request::Output(crate::output_runtime::Command::Configure { user, .. }) => {
                        virtual_users.require_active(*user).map(|_| ())
                    }
                    Request::Settings { index, .. } => {
                        virtual_users.require_active(*index).map(|_| ())
                    }
                    Request::Input(packet) => virtual_users.require_device(packet.device),
                    Request::Tap { device, .. } => virtual_users.require_device(*device),
                    Request::Batch(packets) => packets
                        .iter()
                        .try_for_each(|packet| virtual_users.require_device(packet.device)),
                    Request::Annotation(
                        crate::annotation_runtime::Command::Configure { user, .. }
                        | crate::annotation_runtime::Command::ClearUser { user }
                        | crate::annotation_runtime::Command::Export { user, .. },
                    ) => virtual_users.require_active(*user).map(|_| ()),
                    Request::Magnifier(
                        crate::magnifier_runtime::Command::Configure { user, .. }
                        | crate::magnifier_runtime::Command::Refresh { user }
                        | crate::magnifier_runtime::Command::Export { user, .. },
                    ) => virtual_users.require_active(*user).map(|_| ()),
                    _ => Ok(()),
                };
                if let Err(error) = user_check {
                    let error = Some(format!("{error:#}"));
                    match &request {
                        Request::Settings {
                            index,
                            settings,
                            gui: Some(ticket),
                        } => {
                            emit(Event::SettingsAcknowledged(SettingsAcknowledgement {
                                ticket: ticket.clone(),
                                index: *index,
                                settings: settings.clone(),
                                error,
                            }))?;
                        }
                        Request::Output(crate::output_runtime::Command::Configure {
                            user, ..
                        }) => {
                            let mut state = automation.output_state();
                            state.acknowledged = true;
                            state.user = Some(*user);
                            state.error = error;
                            emit(Event::Output(state))?;
                        }
                        Request::Annotation(_) => {
                            let mut state = router.annotation_state();
                            state.acknowledged = true;
                            state.error = error;
                            emit(Event::Annotation(state))?;
                        }
                        Request::Magnifier(_) => {
                            let mut state = router.magnifier_state();
                            state.acknowledged = true;
                            state.error = error;
                            emit(Event::Magnifier(state))?;
                        }
                        _ => {
                            let mut state = virtual_users.state();
                            state.acknowledged = true;
                            state.error = error;
                            emit(Event::VirtualUsers(state))?;
                        }
                    }
                    return Ok(false);
                }
                match request {
                    Request::DesktopObservation(command) => {
                        let rejection = if let desktop_observation::Command::Bind { seat_identity, .. } = &command {
                            let slots = virtual_users.state();
                            match slots.slots.iter().find(|slot| slot.active && slot.seat.identity() == *seat_identity) {
                                None => Some(desktop_observation::BindRejection::MissingSeat),
                                Some(slot) => {
                                    let mouse = matches!(mux_core::emulated::parse(&slot.seat.mouse), Ok(Some(device)) if device.kind == mux_core::DeviceKind::Mouse);
                                    let keyboard = slot.seat.keyboard.as_deref().is_none_or(|path| matches!(mux_core::emulated::parse(path), Ok(Some(device)) if device.kind == mux_core::DeviceKind::Keyboard));
                                    if !mouse || !keyboard { Some(desktop_observation::BindRejection::NotVirtual) } else { None }
                                }
                            }
                        } else { None };
                        if let Some(reason) = rejection {
                            if let desktop_observation::Command::Bind { request, tag, .. } = command { emit(Event::DesktopObservation(desktop_observation::Delivery::Rejected { request, tag, reason }))?; }
                        } else {
                            for delivery in router.desktop_observation(desktop_observation::Operation::Command(command)) { emit(Event::DesktopObservation(delivery))?; }
                        }
                    }
                    Request::SelectWindow(selection) => {
                        let result = crate::window_selection::apply(
                            &selection,
                            &automation,
                            router.as_mut(),
                        );
                        emit(Event::WindowSelected(
                            crate::window_selection::Acknowledgement {
                                selection,
                                error: result.err().map(|error| format!("{error:#}")),
                            },
                        ))?;
                    }
                    Request::Output(command) => {
                        let user = match &command {
                            crate::output_runtime::Command::Configure { user, .. } => Some(*user),
                            _ => None,
                        };
                        let result = (|| -> Result<crate::output_runtime::State> {
                            if let crate::output_runtime::Command::Configure { settings, .. } =
                                &command
                            {
                                ensure!(
                                    settings == &mux_core::output::Settings::default()
                                        || profile.desktop.is_some()
                                        || profile.concurrent,
                                    "Output conversion requires desktop cursor routing"
                                );
                            }
                            let state = automation.output_command(command, router.as_mut())?;
                            if let Some(user) = user {
                                virtual_users
                                    .output_settings(user, state.users[user].settings.clone());
                            }
                            Ok(state)
                        })();
                        let state = match result {
                            Ok(state) => state,
                            Err(error) => {
                                let mut state = automation.output_state();
                                state.acknowledged = true;
                                state.user = user;
                                state.error = Some(format!("{error:#}"));
                                state
                            }
                        };
                        emit(Event::Output(state))?;
                    }
                    Request::Agent(command) => {
                        if let Some(state) = agents.command(
                            command,
                            &mut virtual_users,
                            &mut automation,
                            &mut router,
                            &profile,
                            controller_pid,
                        )? {
                            emit(Event::Agent(state))?;
                        }
                    }
                    Request::InspectMapperMonitors { request_id } => {
                        let snapshot = automation
                            .mapper_monitor_snapshot(crate::mapper_runtime::Actor::Manual);
                        emit(Event::MapperMonitors(crate::mapper_runtime::MonitorReceipt {
                            request_id,
                            snapshot,
                        }))?;
                    }
                    Request::Capture { user, query } => {
                        if let Err(error) =
                            agents.standalone_capture(user, &virtual_users, &router, &profile, query)
                        {
                            emit(Event::Capture(crate::agent_runtime::CaptureState {
                                acknowledged: true,
                                frame: None,
                                error: Some(format!("{error:#}")),
                            }))?;
                        }
                    }
                    Request::VirtualUsers(command) => {
                        match virtual_users.command(command, &mut automation, router.as_mut()) {
                            Ok(state) => {
                                releases.retain(|(_, packet)| {
                                    virtual_users.require_device(packet.device).is_ok()
                                });
                                emit(Event::Output(automation.output_state()))?;
                                emit(Event::VirtualUsers(state))?;
                            }
                            Err(error) => {
                                let mut state = virtual_users.state();
                                state.acknowledged = true;
                                state.error = Some(format!("{error:#}"));
                                emit(Event::VirtualUsers(state))?;
                            }
                        }
                        emit(Event::Automation(automation.state(false)))?;
                        emit(Event::MacroStudio(automation.studio_state(None)))?;
                        emit(Event::Annotation(router.annotation_state()))?;
                        emit(Event::Magnifier(router.magnifier_state()))?;
                        let (_, states) = router.tick()?;
                        emit(Event::ScreenTargets(capture_targets(&virtual_users, &router, &mut screen_targets)))?;
                        emit(Event::State(states))?;
                        emit(Event::UserPauseStates(automation.user_pause_states(&router)))?;
                    }
                    Request::MacroStudio(command) => {
                        match automation.studio_command(command, router.as_mut()) {
                            Ok(state) => emit(Event::MacroStudio(state))?,
                            Err(error) => {
                                let mut state = automation.studio_state(None);
                                state.acknowledged = true;
                                state.error = Some(format!("{error:#}"));
                                emit(Event::MacroStudio(state))?;
                            }
                        }
                        emit(Event::Automation(automation.state(false)))?;
                    }
                    Request::Magnifier(command) => {
                        let configured_user = match &command {
                            crate::magnifier_runtime::Command::Configure { user, .. } => {
                                Some(*user)
                            }
                            _ => None,
                        };
                        match router.magnifier_command(command) {
                            Ok(state) => {
                                if let Some(user) = configured_user
                                    && let Some(actual) = state.users.get(user)
                                {
                                    virtual_users.magnifier_settings(user, actual.settings.clone());
                                }
                                emit(Event::Magnifier(state))?;
                            }
                            Err(error) => {
                                let mut state = router.magnifier_state();
                                state.acknowledged = true;
                                state.error = Some(format!("{error:#}"));
                                emit(Event::Magnifier(state))?;
                            }
                        }
                    }
                    Request::Annotation(
                        command @ crate::annotation_runtime::Command::Export { .. },
                    ) => {
                        if let Err(error) = agents.begin_annotation_export(
                            (0, 0),
                            command,
                            &virtual_users,
                            &mut router,
                        ) {
                            let mut state = router.annotation_state();
                            state.acknowledged = true;
                            state.error = Some(format!("{error:#}"));
                            emit(Event::Annotation(state))?;
                        }
                    }
                    Request::Annotation(command) => match router.annotation_command(command) {
                        Ok(state) => emit(Event::Annotation(state))?,
                        Err(error) => {
                            let mut state = router.annotation_state();
                            state.acknowledged = true;
                            state.error = Some(format!("{error:#}"));
                            emit(Event::Annotation(state))?;
                        }
                    },
                    Request::Automation(command) => {
                        let mutation = !matches!(
                            command,
                            crate::automation_runtime::Command::Inspect
                                | crate::automation_runtime::Command::CancelReplication { .. }
                                | crate::automation_runtime::Command::Forwarding { .. }
                        );
                        match automation.command(command, router.as_mut()) {
                            Ok(state) => {
                                if mutation {
                                    releases.clear();
                                }
                                emit(Event::Automation(state))?;
                            }
                            Err(error) => {
                                let mut state = automation.state(false);
                                state.acknowledged = true;
                                state.error = Some(format!("{error:#}"));
                                emit(Event::Automation(state))?;
                            }
                        }
                    }
                    Request::MacroFileLoad {
                        clip,
                        expected_macro_revision,
                    } => {
                        match automation.load_if_revision(
                            expected_macro_revision,
                            clip,
                            router.as_mut(),
                        ) {
                            Ok(state) => {
                                releases.clear();
                                emit(Event::Automation(state))?;
                            }
                            Err(error) => {
                                let mut state = automation.state(false);
                                state.acknowledged = true;
                                state.error = Some(format!("{error:#}"));
                                emit(Event::Automation(state))?;
                            }
                        }
                    }
                    Request::Text { user, text } => {
                        let result = automation.action(
                            mux_core::automation::Action::Text { user, text },
                            router.as_mut(),
                            true,
                        );
                        input_ack(result, &automation)?;
                    }
                    Request::Mode {
                        mode,
                        multi_keyboard,
                    } => {
                        if let Err(error) = router.validate_mode_change(multi_keyboard) {
                            emit(Event::CommandRejected(format!(
                                "Mode change rejected: {error:#}"
                            )))?;
                            return Ok(false);
                        }
                        let result = crate::window_selection::change_mode(
                            router.as_mut(),
                            mode,
                            multi_keyboard,
                            |router| {
                                automation.cancel(router)?;
                                automation.stop_recording();
                                Ok(())
                            },
                        );
                        if result.is_ok() {
                            agents.note_mode(mode, multi_keyboard);
                            releases.clear();
                        }
                        match result {
                            Ok(()) => emit(Event::ModeChanged(mode, multi_keyboard))?,
                            Err(error) => input_ack(Err(error), &automation)?,
                        }
                    }
                    Request::HostedMode {
                        ticket,
                        mode,
                        multi_keyboard,
                    } => {
                        if let Err(error) = router.validate_mode_change(multi_keyboard) {
                            emit(Event::HostedModeCompleted {
                                ticket,
                                mode,
                                multi_keyboard,
                                error: Some(format!("Mode change rejected: {error:#}")),
                                uncertain: false,
                            })?;
                            return Ok(false);
                        }
                        let result = crate::window_selection::change_mode(
                            router.as_mut(),
                            mode,
                            multi_keyboard,
                            |router| {
                                automation.cancel(router)?;
                                automation.stop_recording();
                                Ok(())
                            },
                        );
                        if result.is_ok() {
                            agents.note_mode(mode, multi_keyboard);
                            releases.clear();
                        }
                        let uncertain = result.is_err();
                        emit(Event::HostedModeCompleted {
                            ticket,
                            mode,
                            multi_keyboard,
                            error: result.err().map(|error| format!("{error:#}")),
                            uncertain,
                        })?;
                    }
                    Request::MoveTo { user, x, y } => {
                        let result = automation.action(
                            mux_core::automation::Action::MoveTo { user, x, y },
                            router.as_mut(),
                            true,
                        );
                        input_ack(result, &automation)?;
                    }
                    Request::UserPause { user, paused, gui } => {
                        let result = (|| -> Result<crate::routing_engine::UserPauseState> {
                            let slot = virtual_users.require_active(user)?;
                            validate_settings_ticket(gui.as_ref(), &slot.seat.identity())?;
                            automation.user_pause(user, paused, router.as_mut())
                        })();
                        let state = match result {
                            Ok(state) => state,
                            Err(error) => {
                                let mut state = automation.user_pause_state(user, &router);
                                state.error = Some(format!("{error:#}"));
                                state
                            }
                        };
                        if let Some(ticket) = gui {
                            emit(Event::UserPauseAcknowledged { ticket, state })?;
                        } else {
                            emit(Event::UserPause(state))?;
                        }
                    }
                    Request::Pause => {
                        let result = (|| -> Result<()> {
                            automation.cancel(router.as_mut())?;
                            automation.stop_recording();
                            router.pause(true)?;
                            releases.clear();
                            Ok(())
                        })();
                        match result {
                            Ok(()) => emit(Event::Paused)?,
                            Err(error) => input_ack(Err(error), &automation)?,
                        }
                    }
                    Request::Resume => {
                        router.pause(false)?;
                        emit(Event::Resumed)?;
                    }
                    Request::Stop => {
                        return Ok(true);
                    }
                    Request::Input(packet) => {
                        let result = dispatch_input_packet(packet, &mut releases, |packet| {
                            automation.packet(packet, router.as_mut(), true)
                        });
                        input_ack(result, &automation)?;
                    }
                    Request::Batch(packets) => {
                        ensure!(packets.len() <= 256, "input batch is too large");
                        for packet in &packets {
                            packet.validate()?;
                        }
                        let result = packets.into_iter().try_for_each(|packet| {
                            if packet.kind == 1 {
                                releases.retain(|(_, p)| {
                                    p.device != packet.device || p.data != packet.data
                                });
                            }
                            automation.packet(packet, router.as_mut(), true)
                        });
                        input_ack(result, &automation)?;
                    }
                    Request::ReleaseAll => {
                        let result = automation.cancel(router.as_mut());
                        if result.is_ok() {
                            releases.clear();
                        }
                        input_ack(result, &automation)?;
                    }
                    Request::Settings {
                        index,
                        settings,
                        gui,
                    } => {
                        let validation = (|| -> Result<()> {
                            validate_settings_ticket(
                                gui.as_ref(),
                                &virtual_users.require_active(index)?.seat.identity(),
                            )?;
                            mux_windows::monitors::validate_settings(&settings)?;
                            let resolved = virtual_users.resolved(index)?;
                            crate::router::validate_keyboard_settings(
                                &settings,
                                resolved,
                                profile.desktop.is_some() || profile.concurrent,
                            )?;
                            ensure!(
                                !settings.requires_extended_overlay()
                                    || profile.desktop.is_some()
                                    || profile.concurrent,
                                "Extended cursor appearance and idle hiding require desktop cursor routing"
                            );
                            let horizontal_emulation = settings.horizontal_wheel_emulation
                                != mux_core::desktop::HorizontalWheelEmulation::Never;
                            ensure!(
                                (settings.horizontal_wheel
                                    != mux_core::desktop::HorizontalWheelPolicy::Preserve
                                    && !horizontal_emulation)
                                    || profile.desktop.is_some()
                                    || profile.concurrent,
                                "Horizontal wheel delivery requires desktop cursor routing"
                            );
                            ensure!(
                                !horizontal_emulation
                                    || resolved.mouse & mux_core::emulated::MASK
                                        == mux_core::emulated::BASE,
                                "Horizontal wheel emulation currently requires a virtual mouse"
                            );
                            settings
                                .keyboard
                                .validate_application_route(profile.desktop.is_some())?;
                            settings.keyboard.validate_live_update(
                                virtual_users.require_active(index)?.seat.cursor.keyboard,
                            )?;
                            Ok(())
                        })();
                        let dispatch = dispatch_settings(validation, || -> Result<()> {
                            let interrupted = automation.interrupt_user(index, router.as_mut())?;
                            if virtual_users.require_active(index)?.seat.cursor.keyboard
                                != settings.keyboard
                            {
                                automation.keyboard_settings_change(index, router.as_mut())?;
                            }
                            automation.release_user_input_claims(index, router.as_mut())?;
                            router.settings(index, settings.clone())?;
                            virtual_users.settings(index, settings.clone());
                            if interrupted {
                                releases.clear();
                            } else {
                                let seat = virtual_users.resolved(index)?;
                                releases.retain(|(_, packet)| {
                                    packet.device != seat.mouse
                                        && Some(packet.device) != seat.keyboard
                                });
                            }
                            Ok(())
                        });
                        match dispatch {
                            Ok(event) => emit(correlate_settings(gui, index, settings, event))?,
                            Err(error) => {
                                if gui.is_some() {
                                    emit(correlate_settings(
                                        gui,
                                        index,
                                        settings,
                                        Event::CommandRejected(format!("{error:#}")),
                                    ))?;
                                }
                                input_ack(Err(error), &automation)?;
                            }
                        }
                    }
                    Request::Tap { device, key } => {
                        if !router.paused() && !router.key_held(device, key) {
                            let result = automation.packet(
                                mux_windows::keyboard::packet_code(device, key, false)?,
                                router.as_mut(),
                                true,
                            );
                            if result.is_ok() {
                                releases.push((
                                    Instant::now() + Duration::from_millis(150),
                                    mux_windows::keyboard::packet_code(device, key, true)?,
                                ));
                            }
                            input_ack(result, &automation)?;
                        } else {
                            emit(Event::InputSent)?;
                        }
                    }
                }
                Ok(false)
            });
            if dispatched? {
                for delivery in router.desktop_observation(desktop_observation::Operation::Fence(desktop_observation::LossReason::StopIntent)) { emit(Event::DesktopObservation(delivery))?; }
                io_signals.begin_cleanup();
                agents.cancel_annotation_export();
                break;
            }
            emit_desktop_observations(router.as_mut(), &io_signals)?;
        }
        router.shutdown().context(
            "Target cleanup was not acknowledged; restart the affected target applications",
        )?;
        drop(router);
        emit(Event::Stopped)
    })();
    if let Err(error) = &result {
        let _ = emit(Event::Failed(format!("{error:#}")));
    }
    result
}

#[cfg(test)]
mod worker_io_stop_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn queued_stop_precedes_due_publication_and_allows_release_cleanup() {
        let signals = WorkerIoSignals::default();
        signals.request_stop();
        let publications = Cell::new(0);
        assert!(
            signals
                .step(|| {
                    publications.set(publications.get() + 1);
                    bail!("due coalesced motion must not run after queued Stop")
                })
                .unwrap()
        );
        assert_eq!(publications.get(), 0);
        signals.begin_cleanup();
        assert!(!signals.cancel.load(Ordering::Acquire));
        signals.request_stop();
        assert!(
            !signals.cancel.load(Ordering::Acquire),
            "duplicate Stop cancelled release cleanup"
        );
    }

    #[test]
    fn stop_cannot_hide_an_unrelated_acknowledgement_failure() {
        let signals = WorkerIoSignals::default();
        let result = signals.step(|| {
            signals.request_stop();
            bail!("target acknowledgement timed out")
        });
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("acknowledgement timed out")
        );
    }

    #[test]
    fn eof_is_never_cleared_by_queued_or_duplicate_stop() {
        for disconnect_first in [true, false] {
            let signals = WorkerIoSignals::default();
            if disconnect_first {
                signals.disconnect();
            }
            signals.request_stop();
            if !disconnect_first {
                signals.disconnect();
            }
            signals.begin_cleanup();
            signals.request_stop();
            assert!(!signals.ordinary_stop());
            assert!(signals.cancel.load(Ordering::Acquire));
        }
    }
}

#[cfg(test)]
mod attachment_event_tests {
    use super::*;

    #[test]
    fn gui_settings_identity_rejection_is_correlated_before_effects() {
        use std::cell::Cell;
        let ticket = SettingsTicket {
            request: 9,
            identity: "original".into(),
        };
        let effects = Cell::new(0);
        let event = dispatch_settings(
            validate_settings_ticket(Some(&ticket), "replacement"),
            || {
                effects.set(effects.get() + 1);
                Ok(())
            },
        )
        .unwrap();
        let settings = mux_core::desktop::CursorSettings {
            size: 42.,
            ..Default::default()
        };
        let Event::SettingsAcknowledged(ack) =
            correlate_settings(Some(ticket.clone()), 0, settings.clone(), event)
        else {
            panic!("missing correlated rejection");
        };
        assert_eq!(effects.get(), 0);
        assert_eq!(ack.ticket, ticket);
        assert_eq!(ack.settings, settings);
        assert!(ack.error.unwrap().contains("no longer matches"));
        let accepted =
            dispatch_settings(validate_settings_ticket(Some(&ticket), "original"), || {
                effects.set(effects.get() + 1);
                Ok(())
            })
            .unwrap();
        let Event::SettingsAcknowledged(ack) =
            correlate_settings(Some(ticket), 0, settings, accepted)
        else {
            panic!("missing correlated success");
        };
        assert_eq!(effects.get(), 1);
        assert!(ack.error.is_none());
    }

    #[test]
    fn private_user_pause_ack_never_reaches_subscribers_but_current_states_do() {
        let mut bus = EventBus::default();
        let (sender, receiver) = mpsc::sync_channel(4);
        bus.subscribers.push(((1, 1), sender));
        let state = crate::routing_engine::UserPauseState {
            user: 0,
            supported: true,
            paused: true,
            ..Default::default()
        };
        bus.publish(&Event::UserPauseAcknowledged {
            ticket: SettingsTicket {
                request: 1,
                identity: "a".into(),
            },
            state: state.clone(),
        });
        assert!(bus.latest.is_empty());
        assert!(receiver.try_recv().is_err());
        bus.publish(&Event::UserPauseStates(vec![state.clone()]));
        assert!(
            matches!(receiver.try_recv().unwrap(),Event::UserPauseStates(states) if states.len()==1 && states[0].paused)
        );
        bus.publish(&Event::UserPause(state));
        assert!(matches!(receiver.try_recv().unwrap(),Event::UserPause(state) if state.paused));
        assert_eq!(bus.latest.len(), 1);
    }

    #[test]
    fn gui_settings_ack_is_not_cached_or_published_to_client_subscribers() {
        let mut bus = EventBus::default();
        let (sender, receiver) = mpsc::sync_channel(2);
        bus.subscribers.push(((1, 1), sender));
        bus.publish(&Event::SettingsAcknowledged(SettingsAcknowledgement {
            ticket: SettingsTicket {
                request: 1,
                identity: "private".into(),
            },
            index: 0,
            settings: Default::default(),
            error: None,
        }));
        assert!(bus.latest.is_empty());
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        bus.publish(&Event::InputSent);
        assert!(matches!(receiver.try_recv(), Ok(Event::InputSent)));
    }

    #[test]
    fn uncorrelated_settings_keep_the_existing_json_and_ack_contract() {
        let request: Request =
            serde_json::from_value(serde_json::json!({"Settings":{"index":0,"settings":{}}}))
                .unwrap();
        assert!(matches!(&request, Request::Settings { gui: None, .. }));
        assert!(
            serde_json::to_value(request).unwrap()["Settings"]
                .get("gui")
                .is_none()
        );
        assert!(matches!(
            correlate_settings(None, 0, Default::default(), Event::InputSent),
            Event::InputSent
        ));
        assert!(
            matches!(correlate_settings(None, 0, Default::default(), Event::CommandRejected("old error".into())), Event::CommandRejected(error) if error == "old error")
        );
    }

    #[test]
    fn nonreading_owned_child_times_out_and_worker_drop_joins_after_exit() {
        // This test-only child receives an explicit command, never stdin code.
        // Its finite sleep is a final bound even if the cleanup under test breaks.
        // No product fault hook or user process is involved.
        let result = (|| -> Result<(bool, bool, bool, Duration)> {
            let mut worker = Worker(
                Command::new("powershell.exe")
                    .args([
                        "-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
                        "[Console]::Out.WriteLine('MUX_PIPE_READY'); [Console]::Out.Flush(); Start-Sleep -Seconds 20",
                    ])
                    .creation_flags(0x08000000)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                    .context("start exact owned nonreading pipe child")?,
                None,
                None,
            );
            // Declared after Worker: error unwinding closes the job first, then
            // Worker kills/waits and joins; no assertion can bypass cleanup.
            let job = mux_windows::job::Job::own(&worker.0)?;
            let watch = mux_windows::targets::ProcessWatch::open([worker.0.id()])?;
            let stdout = worker.0.stdout.take().context("test child stdout")?;
            let (ready, readiness) = mpsc::channel();
            worker.1 = Some(thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    match line {
                        Ok(line) if line == "MUX_PIPE_READY" => {
                            let _ = ready.send(());
                        }
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
            }));
            readiness
                .recv_timeout(Duration::from_secs(5))
                .context("nonreading child readiness")?;
            let stdin = worker.0.stdin.take().context("test child stdin")?;
            worker.2 = Some(transport::Transport::start(stdin)?);
            // Valid JSON below the existing connected-frame ceiling, much larger
            // than an anonymous Windows pipe buffer. Child never reads stdin.
            let frame = serde_json::to_string(&Request::Text {
                user: 0,
                text: "x".repeat(8 * 1024 * 1024),
            })?;
            let due = Instant::now() + Duration::from_millis(250);
            worker.2.as_mut().unwrap().enqueue(frame, due)?;
            worker.2.as_mut().unwrap().poll(Instant::now())?;
            while Instant::now() < due {
                thread::sleep(Duration::from_millis(5));
            }
            let alive = worker.0.try_wait()?.is_none();
            let stalled = worker.2.as_ref().unwrap().has_unfinished_write();
            let expired = worker.2.as_mut().unwrap().poll(Instant::now()).is_err();
            let teardown = Instant::now();
            drop(job);
            drop(worker);
            Ok((
                alive && stalled,
                expired,
                watch.exited().is_some(),
                teardown.elapsed(),
            ))
        })();
        // Every Result path above has already closed the exact job and Worker.
        let (nonreading, expired, exited, teardown) = result.unwrap();
        assert!(
            nonreading,
            "owned child must remain live with an unfinished real pipe write"
        );
        assert!(
            expired,
            "transport watchdog must expire without waiting for the pipe write"
        );
        assert!(
            exited,
            "Worker Drop must wait for exact child exit before returning"
        );
        assert!(
            teardown < Duration::from_secs(5),
            "writer join waited for the sleeping child"
        );
    }

    #[test]
    fn stop_intent_bypasses_ordinary_request_backlog() {
        let (requests, backlog) = mpsc::channel();
        let shared = SharedHandle {
            observation: desktop_observation::Inbox::new().unwrap(),
            requests,
            bus: Arc::new(Mutex::new(EventBus::default())),
            recovery: Recovery::for_profile(&Profile::default()),
            stop_requested: Arc::new(AtomicBool::new(false)),
        };
        for _ in 0..512 {
            shared.request(Request::Resume);
        }
        shared.request(Request::Stop);
        assert!(shared.stop_requested.load(Ordering::Acquire));
        assert_eq!(backlog.try_iter().count(), 512);
        assert!(matches!(
            shared.recovery.snapshot().phase,
            crate::session_recovery::Phase::Stopping
        ));
    }
    #[test]
    fn observation_stop_fences_pending_before_worker_or_event_backlog() {
        let (controller, _, requests, _) = Controller::test_fixture();
        let tag = controller.bind_desktop_observation(1, "seat-a".into()).unwrap();
        let shared = controller.shared_handle();
        shared.request(Request::DesktopObservation(desktop_observation::Command::Unbind { tag }));
        controller.request(Request::DesktopObservation(desktop_observation::Command::Unbind { tag }));
        assert_eq!(requests.try_iter().count(), 1, "only the private Bind was admitted");
        shared.request(Request::Stop);
        assert!(!controller.desktop_observation_live(tag));
        assert_eq!(controller.take_desktop_observations(), vec![desktop_observation::Delivery::Lost { tag: None, reason: desktop_observation::LossReason::StopIntent }]);
        let observer = shared.subscribe(7, 1);
        let event = Event::DesktopObservation(desktop_observation::Delivery::Lost { tag: None, reason: desktop_observation::LossReason::Eof });
        assert!(!shared.bus.lock().unwrap().publish(&event));
        assert!(observer.try_recv().is_err());
        assert!(shared.bus.lock().unwrap().latest.is_empty());
    }

    #[test]
    fn session_commit_requires_stopped_and_successful_worker_exit() {
        use std::cell::Cell;
        let committed = Cell::new(0);
        for (event, successful, expected_ok, expected_commits) in [
            (Event::Failed("partial retirement".into()), true, true, 0),
            (Event::Failed("worker terminated".into()), false, true, 0),
            (Event::Stopped, false, false, 0),
            (Event::Stopped, true, true, 1),
        ] {
            let result = commit_terminal(&event, successful, || {
                committed.set(committed.get() + 1);
                Ok(())
            });
            assert_eq!(result.is_ok(), expected_ok);
            assert_eq!(committed.get(), expected_commits);
        }
        assert!(
            commit_terminal(&Event::Stopped, true, || {
                bail!("commit mapping unavailable")
            })
            .is_err()
        );
    }

    #[test]
    fn startup_scope_deduplicates_required_targets_without_expanding_allowlist() {
        let mut profile: Profile = serde_json::from_value(serde_json::json!({
            "version": 1,
            "seats": [
                {"name":"A", "mouse":"virtual://mouse/1", "keyboard":null, "target_pid":42},
                {"name":"B", "mouse":"virtual://mouse/2", "keyboard":null, "target_pid":42},
                {"name":"C", "mouse":"virtual://mouse/3", "keyboard":null}
            ]
        }))
        .unwrap();
        profile.desktop = Some(mux_core::routing::DesktopRouting {
            allowed_pids: vec![7, 42, 99],
            ..Default::default()
        });
        assert_eq!(start_target_pids(&profile), vec![42]);
        profile.seats[0].target_pid = 0;
        profile.seats[1].target_pid = 0;
        assert!(start_target_pids(&profile).is_empty());
    }

    #[test]
    fn settings_preflight_rejection_preserves_session_for_the_next_saved_release() {
        use mux_core::keyboard::{ApplicationFocus, KeyboardSettings};
        use std::cell::Cell;

        let running = KeyboardSettings {
            application_focus: ApplicationFocus::NativeControls,
            ..Default::default()
        };
        let rejected = KeyboardSettings {
            application_focus: ApplicationFocus::Compatibility,
            ..running
        };
        let effects = Cell::new(0);
        let mut live_settings = running;
        let keyboard = mux_core::emulated::device(mux_core::DeviceKind::Keyboard, 1)
            .unwrap()
            .handle as u32;
        let original_up = mux_core::emulated::Packet::key_scanned(keyboard, 0x11, 0x1d, 0, true);
        assert_eq!(original_up.flags, 1);
        assert_eq!(original_up.data, 0x11);
        assert_eq!(original_up.x, 0x1d);
        assert!(original_up.validate().is_ok());
        let mut pending_ups = vec![(Instant::now() + Duration::from_secs(10), original_up)];
        let mut observed_rejection = None;
        let later_up_processed = Cell::new(0);

        enum QueuedAction {
            InvalidSettings,
            OriginalUp,
        }
        for action in [QueuedAction::InvalidSettings, QueuedAction::OriginalUp] {
            match action {
                QueuedAction::InvalidSettings => {
                    let validation = rejected.validate_live_update(running);
                    match dispatch_settings(validation, || {
                        effects.set(effects.get() + 1);
                        pending_ups.clear();
                        live_settings = rejected;
                        Ok(())
                    })
                    .unwrap()
                    {
                        Event::CommandRejected(error) => {
                            observed_rejection = Some(error);
                            continue;
                        }
                        Event::InputSent => panic!("invalid settings were applied"),
                        _ => panic!("settings preflight returned an unrelated event"),
                    }
                }
                QueuedAction::OriginalUp => {
                    dispatch_input_packet(original_up, &mut pending_ups, |packet| {
                        assert_eq!(packet.device, keyboard);
                        assert_eq!(packet.kind, 1);
                        assert_eq!(packet.flags, 1);
                        assert_eq!(packet.data, 0x11);
                        assert_eq!(packet.x, 0x1d);
                        assert!(packet.validate().is_ok());
                        later_up_processed.set(later_up_processed.get() + 1);
                        Ok(())
                    })
                    .unwrap();
                }
            }
        }

        assert_eq!(
            observed_rejection.as_deref(),
            Some("Application focus cannot change during routing; stop and restart the session")
        );
        assert_eq!(effects.get(), 0, "preflight rejection ran settings effects");
        assert_eq!(
            live_settings, running,
            "preflight rejection changed live policy"
        );
        assert_eq!(
            later_up_processed.get(),
            1,
            "the queued original UP was not processed"
        );
        assert!(
            pending_ups.is_empty(),
            "the original UP did not clear its saved hold"
        );

        let mut applied = running;
        assert!(matches!(
            dispatch_settings(Ok(()), || {
                effects.set(effects.get() + 1);
                applied = rejected;
                Ok(())
            })
            .unwrap(),
            Event::InputSent
        ));
        assert_eq!(effects.get(), 1);
        assert_eq!(applied, rejected, "valid preflight did not apply settings");

        let effect_error = anyhow::anyhow!("effectful settings failure");
        match dispatch_settings(Ok(()), || Err(effect_error)) {
            Err(error) => assert_eq!(error.to_string(), "effectful settings failure"),
            Ok(_) => panic!("effect failure was converted into a successful ACK"),
        }
    }

    #[test]
    fn background_preflight_delay_is_cancellable_and_bounded() {
        let stop = Arc::new(AtomicBool::new(false));
        let (started, ready) = mpsc::channel();
        let (finished, completion) = mpsc::channel();
        let worker_stop = stop.clone();
        let worker = thread::spawn(move || {
            let _ = started.send(());
            let result = wait_preflight_delay(Duration::from_secs(6), Some(&worker_stop));
            let _ = finished.send(result);
        });
        ready.recv_timeout(Duration::from_secs(1)).unwrap();
        let cancel_at = Instant::now();
        stop.store(true, Ordering::Release);
        assert!(!completion.recv_timeout(Duration::from_secs(1)).unwrap());
        assert!(cancel_at.elapsed() < Duration::from_secs(1));
        worker.join().unwrap();

        assert!(
            validate_start_options(&StartOptions {
                preflight_delay: Duration::from_secs(15),
                ..StartOptions::default()
            })
            .is_ok()
        );
        assert!(
            validate_start_options(&StartOptions {
                preflight_delay: Duration::from_millis(15_001),
                ..StartOptions::default()
            })
            .is_err()
        );
    }

    #[test]
    fn release_rejection_is_typed_nonterminal_and_not_replayed() {
        let error = anyhow::Error::new(crate::router::ReleaseFailure {
            failures: vec![crate::router::ReleaseFailureDetail {
                user: 0,
                device: 1,
                identity: 65,
                detail: "original input window disappeared".into(),
            }],
        })
        .context("Changing cursor settings");
        let rejection = release_rejection(&error).unwrap();
        assert!(
            matches!(&rejection, Event::InputRejected(detail) if detail.contains("Changing cursor settings") && detail.contains("original input window"))
        );
        assert!(release_rejection(&anyhow::anyhow!("unrelated native failure")).is_none());
        let mut bus = EventBus::default();
        let (tx, rx) = mpsc::sync_channel(16);
        bus.subscribers.push(((1, 1), tx));
        bus.publish(&Event::Configured(2));
        bus.publish(&rejection);
        assert!(matches!(
            rx.try_iter().last(),
            Some(Event::InputRejected(_))
        ));
        assert_eq!(bus.latest.len(), 1);
        assert!(matches!(bus.latest[0], Event::Configured(2)));
    }

    #[test]
    fn command_prevalidation_rejection_is_ephemeral_and_distinct_from_release_failure() {
        let rejected = dispatch_settings(Err(anyhow::anyhow!("invalid live policy")), || {
            panic!("invalid prevalidation must not apply settings")
        })
        .unwrap();
        assert!(matches!(
            &rejected,
            Event::CommandRejected(error) if error == "invalid live policy"
        ));

        let mut bus = EventBus::default();
        let (tx, rx) = mpsc::sync_channel(16);
        bus.subscribers.push(((1, 1), tx));
        bus.publish(&Event::Configured(2));
        bus.publish(&rejected);
        assert!(matches!(
            rx.try_iter().last(),
            Some(Event::CommandRejected(error)) if error == "invalid live policy"
        ));
        assert_eq!(bus.latest.len(), 1);
        assert!(matches!(bus.latest[0], Event::Configured(2)));
    }

    #[test]
    fn completion_expires_only_matching_progress_and_is_not_replayed() {
        let mut bus = EventBus::default();
        let (tx, rx) = mpsc::sync_channel(16);
        bus.subscribers.push(((1, 1), tx));
        bus.publish(&Event::Configured(2));
        bus.publish(&Event::Attaching(42));
        bus.publish(&Event::AttachmentFinished {
            pid: 7,
            error: None,
        });
        assert!(
            bus.latest
                .iter()
                .any(|event| matches!(event, Event::Attaching(42)))
        );
        bus.publish(&Event::AttachmentFinished {
            pid: 42,
            error: Some("owned fixture refused attachment".into()),
        });
        assert_eq!(bus.latest.len(), 1);
        assert!(matches!(bus.latest[0], Event::Configured(2)));
        assert!(matches!(
            rx.try_iter().last(),
            Some(Event::AttachmentFinished {
                pid: 42,
                error: Some(_)
            })
        ));
    }
}
