//! Single-owner, nonblocking GUI lifecycle for the authenticated Agent Broker.
use crate::{gui_bridge, session};
use anyhow::{Context, Result, anyhow, ensure};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Stopped,
    Starting,
    Active,
    Stopping,
    StartFailed(String),
    CleanupFailed(String),
    WorkerFailed(String),
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub generation: u64,
    pub session_epoch: u64,
    pub phase: Phase,
    pub descriptor_path: Option<PathBuf>,
    pub worker_alive: bool,
}

#[derive(Clone, Debug)]
pub enum CompletionKind {
    Started(PathBuf),
    StartFailed(String),
    Stopped,
    CleanupFailed(String),
    WorkerFailed(String),
}

#[derive(Clone, Debug)]
pub struct Completion {
    pub generation: u64,
    pub session_epoch: u64,
    pub kind: CompletionKind,
}

pub(super) fn enable_completion_matches(
    generation: u64,
    session_epoch: u64,
    pending: Option<(u64, u64)>,
    broker_current: bool,
    current_epoch: u64,
    has_session: bool,
    stopping: bool,
) -> bool {
    broker_current
        && current_epoch == session_epoch
        && has_session
        && !stopping
        && pending == Some((generation, session_epoch))
}

// The GUI's one coalesced Start intent cannot construct a controller until
// the sole actor has acknowledged all old descriptor/resource cleanup.
pub(super) fn restart_ready(
    requested: bool,
    session_terminal: bool,
    recovery_ready: bool,
    phase: &Phase,
) -> bool {
    requested && session_terminal && recovery_ready && matches!(phase, Phase::Stopped)
}

pub(super) fn quit_ready(session_terminal: bool, phase: &Phase) -> bool {
    session_terminal && matches!(phase, Phase::Stopped)
}

pub(super) fn fail_pending_enable(
    page: &mut crate::agent_ui::AgentPage,
    pending: &mut Option<(u64, u64, crate::agent_runtime::Command)>,
    generation: u64,
    session_epoch: u64,
    error: String,
) -> bool {
    if pending
        .as_ref()
        .is_some_and(|(pending_generation, pending_epoch, _)| {
            *pending_generation == generation && *pending_epoch == session_epoch
        })
    {
        pending.take();
        page.request_failed(error);
        true
    } else {
        false
    }
}

pub(super) struct GuiSession {
    pub epoch: u64,
    pub live: bool,
    pub stopping: bool,
}

pub(super) enum GuiTransition {
    Configure(PathBuf, crate::agent_runtime::Command),
    Retire,
    Status(String),
    Stopped,
    Unchanged,
}

// This is the production GUI completion transition. It owns the deferred
// Configure until the actor either starts that exact request or abandons it.
pub(super) fn apply_gui_completion<B: Backend>(
    page: &mut crate::agent_ui::AgentPage,
    pending: &mut Option<(u64, u64, crate::agent_runtime::Command)>,
    completion: Completion,
    service: &Service<B>,
    session: GuiSession,
) -> GuiTransition {
    let generation = completion.generation;
    let epoch = completion.session_epoch;
    let pending_identity = pending
        .as_ref()
        .map(|(generation, epoch, _)| (*generation, *epoch));
    match completion.kind {
        CompletionKind::Started(path) => {
            if enable_completion_matches(
                generation,
                epoch,
                pending_identity,
                service.is_current(generation, epoch),
                session.epoch,
                session.live,
                session.stopping,
            ) && let Some((_, _, command)) = pending.take()
            {
                GuiTransition::Configure(path, command)
            } else {
                GuiTransition::Retire
            }
        }
        CompletionKind::StartFailed(error) => {
            if fail_pending_enable(page, pending, generation, epoch, error.clone()) {
                GuiTransition::Status(format!("Agent bridge could not start: {error}"))
            } else {
                GuiTransition::Unchanged
            }
        }
        CompletionKind::CleanupFailed(error) => {
            let status = format!("Agent Access cleanup is incomplete: {error}");
            let retained = service.retains_start(generation, epoch)
                && session.epoch == epoch
                && session.live
                && !session.stopping
                && pending_identity == Some((generation, epoch));
            if !retained {
                fail_pending_enable(page, pending, generation, epoch, status.clone());
            }
            GuiTransition::Status(status)
        }
        CompletionKind::WorkerFailed(error) => {
            let status = format!("Agent Access lifecycle worker failed: {error}");
            fail_pending_enable(page, pending, generation, epoch, status.clone());
            GuiTransition::Status(status)
        }
        CompletionKind::Stopped => GuiTransition::Stopped,
    }
}

#[derive(Clone)]
pub struct StartRequest {
    pub session_epoch: u64,
    session: Option<session::SharedHandle>,
    app: Option<gui_bridge::AppHandle>,
    setup: Option<crate::gui_setup::Handle>,
}

pub(super) trait Backend: Send + 'static {
    type Running: Send + 'static;

    #[cfg(test)]
    fn before_take(&mut self) {}

    fn start(
        &mut self,
        request: StartRequest,
        descriptor_path: PathBuf,
        generation: u64,
        current_generation: Arc<AtomicU64>,
        stop: Arc<AtomicBool>,
    ) -> Result<Self::Running>;

    fn stop(&mut self, running: Self::Running) -> Result<()>;
    fn retry_descriptor_cleanup(&mut self, path: &Path) -> Result<()>;
}

pub(super) struct ProductionBackend;

impl Backend for ProductionBackend {
    type Running = gui_bridge::Broker;

    fn start(
        &mut self,
        request: StartRequest,
        descriptor_path: PathBuf,
        generation: u64,
        current_generation: Arc<AtomicU64>,
        stop: Arc<AtomicBool>,
    ) -> Result<Self::Running> {
        if let Some(setup) = request.setup {
            gui_bridge::Broker::start_setup_cancellable(
                setup,
                descriptor_path,
                current_generation,
                generation,
                stop,
            )
        } else {
            gui_bridge::Broker::start_with_app_cancellable(
                request.session.context("missing session handle")?,
                request.app.context("missing app handle")?,
                descriptor_path,
                current_generation,
                generation,
                stop,
            )
        }
    }

    fn stop(&mut self, running: Self::Running) -> Result<()> {
        running.shutdown()
    }

    fn retry_descriptor_cleanup(&mut self, path: &Path) -> Result<()> {
        gui_bridge::remove_descriptor_file(path)
    }
}

#[derive(Clone)]
enum Desired {
    Start {
        generation: u64,
        request: StartRequest,
        stop: Arc<AtomicBool>,
    },
    Stop {
        generation: u64,
        session_epoch: u64,
    },
}

struct State {
    generation: u64,
    session_epoch: u64,
    desired: Option<Desired>,
    phase: Phase,
    descriptor_path: Option<PathBuf>,
    completion: Option<Completion>,
    active_stop: Option<Arc<AtomicBool>>,
    // Remains set while a requested Start survives old-descriptor cleanup.
    // Separate from desired, which the actor temporarily takes while working.
    latest_start: Option<(u64, u64)>,
    worker_alive: bool,
    shutdown: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    current_generation: Arc<AtomicU64>,
}

pub(super) struct Service<B: Backend = ProductionBackend> {
    shared: Arc<Shared>,
    _backend: std::marker::PhantomData<B>,
}

impl Service<ProductionBackend> {
    pub fn start(guards: Arc<crate::gui::RuntimeGuards>) -> Result<Self> {
        Self::start_with_backend_guards(ProductionBackend, Some(guards))
    }
}

impl<B: Backend> Service<B> {
    #[cfg(test)]
    fn start_with_backend(backend: B) -> Result<Self> {
        Self::start_with_backend_guards(backend, None)
    }

    fn start_with_backend_guards(
        backend: B,
        guards: Option<Arc<crate::gui::RuntimeGuards>>,
    ) -> Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                generation: 0,
                session_epoch: 0,
                desired: None,
                phase: Phase::Stopped,
                descriptor_path: None,
                completion: None,
                active_stop: None,
                latest_start: None,
                worker_alive: true,
                shutdown: false,
            }),
            wake: Condvar::new(),
            current_generation: Arc::new(AtomicU64::new(0)),
        });
        let worker_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("gui-agent-broker-lifecycle".into())
            .spawn(move || {
                let _guards = guards;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    worker_loop(worker_shared.clone(), backend)
                }));
                if let Err(payload) = result {
                    mark_worker_failed(&worker_shared, panic_message(payload));
                }
            })
            .context("start Agent Broker lifecycle worker")?;
        Ok(Self {
            shared,
            _backend: std::marker::PhantomData,
        })
    }

    pub fn request_start(
        &self,
        session_epoch: u64,
        session: session::SharedHandle,
        app: gui_bridge::AppHandle,
    ) -> Result<u64> {
        self.enqueue_start(StartRequest {
            session_epoch,
            session: Some(session),
            app: Some(app),
            setup: None,
        })
    }

    pub fn request_setup_start(
        &self,
        session_epoch: u64,
        setup: crate::gui_setup::Handle,
    ) -> Result<u64> {
        self.enqueue_start(StartRequest {
            session_epoch,
            session: None,
            app: None,
            setup: Some(setup),
        })
    }

    fn enqueue_start(&self, request: StartRequest) -> Result<u64> {
        let session_epoch = request.session_epoch;
        let mut state = lock(&self.shared.state);
        ensure!(
            state.worker_alive && !state.shutdown,
            "Agent Broker worker is unavailable"
        );
        if matches!(state.phase, Phase::Active | Phase::Starting)
            && state.session_epoch == session_epoch
        {
            return Ok(state.generation);
        }
        state.generation = state
            .generation
            .checked_add(1)
            .context("Agent Broker generation exhausted")?;
        let generation = state.generation;
        let stop = Arc::new(AtomicBool::new(false));
        if let Some(previous) = &state.active_stop {
            previous.store(true, Ordering::Release);
        }
        state.session_epoch = session_epoch;
        state.desired = Some(Desired::Start {
            generation,
            request,
            stop: Arc::clone(&stop),
        });
        state.phase = Phase::Starting;
        state.completion = None;
        state.active_stop = Some(stop);
        state.latest_start = Some((generation, session_epoch));
        self.shared
            .current_generation
            .store(generation, Ordering::Release);
        self.shared.wake.notify_one();
        Ok(generation)
    }

    pub fn request_stop(&self, session_epoch: u64) -> u64 {
        Self::stop_locked(&self.shared, &mut lock(&self.shared.state), session_epoch)
    }

    // Automatic GUI polling can initiate retirement once. Failed cleanup stays
    // owned by the actor's existing retry/backoff; a dead actor stays truthful.
    pub fn request_automatic_stop(&self, session_epoch: u64) -> bool {
        let mut state = lock(&self.shared.state);
        if !matches!(state.phase, Phase::Active | Phase::Starting) {
            return false;
        }
        Self::stop_locked(&self.shared, &mut state, session_epoch);
        true
    }

    fn stop_locked(shared: &Shared, state: &mut State, session_epoch: u64) -> u64 {
        let replacing_start = state.latest_start.is_some();
        state.latest_start = None;
        if !state.worker_alive || state.shutdown {
            return state.generation;
        }
        if state.desired.is_none() && matches!(state.phase, Phase::Stopped) {
            state.session_epoch = session_epoch;
            return state.generation;
        }
        if let Some(active_stop) = &state.active_stop {
            active_stop.store(true, Ordering::Release);
        }
        if matches!(state.phase, Phase::Stopping)
            && state.session_epoch == session_epoch
            && !replacing_start
        {
            return state.generation;
        }
        state.generation = state.generation.saturating_add(1);
        let generation = state.generation;
        state.session_epoch = session_epoch;
        state.desired = Some(Desired::Stop {
            generation,
            session_epoch,
        });
        state.phase = Phase::Stopping;
        state.completion = None;
        shared
            .current_generation
            .store(generation, Ordering::Release);
        shared.wake.notify_one();
        generation
    }

    pub fn status(&self) -> Snapshot {
        let state = lock(&self.shared.state);
        Snapshot {
            generation: state.generation,
            session_epoch: state.session_epoch,
            phase: state.phase.clone(),
            descriptor_path: state.descriptor_path.clone(),
            worker_alive: state.worker_alive,
        }
    }

    pub fn take_completion(&self) -> Option<Completion> {
        lock(&self.shared.state).completion.take()
    }

    pub fn is_current(&self, generation: u64, session_epoch: u64) -> bool {
        let state = lock(&self.shared.state);
        state.generation == generation
            && state.session_epoch == session_epoch
            && matches!(state.phase, Phase::Starting | Phase::Active)
            && !state.shutdown
    }

    fn retains_start(&self, generation: u64, session_epoch: u64) -> bool {
        let state = lock(&self.shared.state);
        !state.shutdown
            && state.worker_alive
            && state.latest_start == Some((generation, session_epoch))
            && state.generation == generation
            && state.session_epoch == session_epoch
    }
}

impl<B: Backend> Drop for Service<B> {
    fn drop(&mut self) {
        let mut state = lock(&self.shared.state);
        state.shutdown = true;
        state.latest_start = None;
        state.generation = state.generation.saturating_add(1);
        self.shared
            .current_generation
            .store(state.generation, Ordering::Release);
        if let Some(active_stop) = &state.active_stop {
            active_stop.store(true, Ordering::Release);
        }
        state.desired = Some(Desired::Stop {
            generation: state.generation,
            session_epoch: state.session_epoch,
        });
        state.phase = Phase::Stopping;
        self.shared.wake.notify_one();
    }
}

struct Owned<B: Backend> {
    running: B::Running,
    path: PathBuf,
    stop: Arc<AtomicBool>,
    generation: u64,
    session_epoch: u64,
}

fn worker_loop<B: Backend>(shared: Arc<Shared>, mut backend: B) {
    let mut owned: Option<Owned<B>> = None;
    let mut retry_delay = std::time::Duration::from_millis(100);
    loop {
        let (desired, shutdown, retry_path) = loop {
            let mut state = lock(&shared.state);
            while state.desired.is_none() && !state.shutdown {
                if state.descriptor_path.is_some() && matches!(state.phase, Phase::CleanupFailed(_))
                {
                    let waited = shared
                        .wake
                        .wait_timeout(state, retry_delay)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state = waited.0;
                    if state.desired.is_none() && !state.shutdown {
                        state.desired = Some(Desired::Stop {
                            generation: state.generation,
                            session_epoch: state.session_epoch,
                        });
                        state.phase = Phase::Stopping;
                    }
                } else {
                    state = shared
                        .wake
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
            }

            // If control Drop has left a descriptor cleanup obligation, retain the
            // sole actor and retry with bounded exponential backoff. A new request
            // wakes this wait immediately, so live GUI retries are still responsive.
            if state.shutdown && state.desired.is_none() && state.descriptor_path.is_some() {
                let waited = shared
                    .wake
                    .wait_timeout(state, retry_delay)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state = waited.0;
                if state.desired.is_none() {
                    state.desired = Some(Desired::Stop {
                        generation: state.generation,
                        session_epoch: state.session_epoch,
                    });
                }
            }
            drop(state);

            #[cfg(test)]
            backend.before_take();

            let mut state = lock(&shared.state);
            if state.desired.is_none() && !state.shutdown {
                continue;
            }
            let desired = state.desired.take();
            let shutdown = state.shutdown;
            let retry_path = if desired.is_some() {
                state.descriptor_path.clone()
            } else {
                None
            };
            break (desired, shutdown, retry_path);
        };

        if owned.is_none()
            && let Some(path) = retry_path
        {
            if let Err(error) = backend_call(|| backend.retry_descriptor_cleanup(&path)) {
                let (generation, session_epoch) = current_ticket(&shared);
                finish_cleanup_failure(
                    &shared,
                    generation,
                    session_epoch,
                    path,
                    format!("{error:#}"),
                );
                retry_delay = retry_delay
                    .saturating_mul(2)
                    .min(std::time::Duration::from_secs(10));
                if !shutdown && let Some(pending) = desired.clone() {
                    let (pending_generation, pending_epoch) = desired_ticket(&pending);
                    let mut state = lock(&shared.state);
                    if state.desired.is_none()
                        && state.generation == pending_generation
                        && state.session_epoch == pending_epoch
                    {
                        state.desired = Some(pending);
                    }
                }
                if !shutdown {
                    let state = lock(&shared.state);
                    drop(
                        shared
                            .wake
                            .wait_timeout(state, retry_delay)
                            .unwrap_or_else(std::sync::PoisonError::into_inner),
                    );
                }
                continue;
            }
            retry_delay = std::time::Duration::from_millis(100);
            clear_descriptor_after_cleanup(&shared, &path);
            if let Some(desired) = &desired {
                let (generation, session_epoch) = desired_ticket(desired);
                let phase = match desired {
                    Desired::Start { .. } => Phase::Starting,
                    Desired::Stop { .. } => Phase::Stopping,
                };
                set_phase_if_current(&shared, generation, session_epoch, phase);
            }
        }

        match desired {
            Some(Desired::Start {
                generation,
                request,
                stop,
            }) => {
                let session_epoch = request.session_epoch;
                if let Some(existing) = owned.take() {
                    set_phase_if_current(&shared, generation, session_epoch, Phase::Stopping);
                    existing.stop.store(true, Ordering::Release);
                    if let Err(error) = backend_call(|| backend.stop(existing.running)) {
                        finish_cleanup_failure(
                            &shared,
                            generation,
                            session_epoch,
                            existing.path,
                            format!("{error:#}"),
                        );
                        defer_cleanup_retry(
                            &shared,
                            (!shutdown).then_some(Desired::Start {
                                generation,
                                request,
                                stop,
                            }),
                            shutdown,
                            &mut retry_delay,
                        );
                        continue;
                    }
                    clear_descriptor_after_cleanup(&shared, &existing.path);
                    set_phase_if_current(&shared, generation, session_epoch, Phase::Starting);
                    retry_delay = std::time::Duration::from_millis(100);
                }
                if !is_current(&shared, generation, session_epoch) {
                    continue;
                }
                let path = match descriptor_path() {
                    Ok(path) => path,
                    Err(error) => {
                        finish_start_failure(
                            &shared,
                            generation,
                            session_epoch,
                            format!("{error:#}"),
                        );
                        continue;
                    }
                };
                set_pending_path(&shared, generation, session_epoch, path.clone());
                match backend_call(|| {
                    backend.start(
                        request,
                        path.clone(),
                        generation,
                        Arc::clone(&shared.current_generation),
                        Arc::clone(&stop),
                    )
                }) {
                    Ok(running)
                        if is_current(&shared, generation, session_epoch)
                            && !stop.load(Ordering::Acquire) =>
                    {
                        set_active(
                            &shared,
                            generation,
                            session_epoch,
                            path.clone(),
                            Arc::clone(&stop),
                            Completion {
                                generation,
                                session_epoch,
                                kind: CompletionKind::Started(path.clone()),
                            },
                        );
                        owned = Some(Owned {
                            running,
                            path,
                            stop,
                            generation,
                            session_epoch,
                        });
                    }
                    Ok(running) => {
                        stop.store(true, Ordering::Release);
                        match backend_call(|| backend.stop(running)) {
                            Ok(()) => {
                                clear_descriptor_after_cleanup(&shared, &path);
                                finish_stopped(&shared, generation, session_epoch);
                            }
                            Err(error) => finish_cleanup_failure(
                                &shared,
                                generation,
                                session_epoch,
                                path,
                                format!("{error:#}"),
                            ),
                        }
                    }
                    Err(error) if is_current(&shared, generation, session_epoch) => {
                        // A start operation failed terminally. Only cleanup is
                        // retried; this ticket must not keep a GUI Configure alive.
                        cancel_start_if_current(&shared, generation, session_epoch);
                        if let Err(cleanup_error) =
                            backend_call(|| backend.retry_descriptor_cleanup(&path))
                        {
                            finish_cleanup_failure(
                                &shared,
                                generation,
                                session_epoch,
                                path,
                                format!(
                                    "start failed: {error:#}; cleanup failed: {cleanup_error:#}"
                                ),
                            );
                        } else {
                            clear_descriptor_after_cleanup(&shared, &path);
                            finish_start_failure(
                                &shared,
                                generation,
                                session_epoch,
                                format!("{error:#}"),
                            );
                        }
                    }
                    Err(_) => {
                        if backend_call(|| backend.retry_descriptor_cleanup(&path)).is_ok() {
                            clear_descriptor_after_cleanup(&shared, &path);
                        } else {
                            let (current_generation, current_epoch) = current_ticket(&shared);
                            finish_cleanup_failure(
                                &shared,
                                current_generation,
                                current_epoch,
                                path,
                                "superseded Agent Broker start left a descriptor to clean".into(),
                            );
                        }
                    }
                }
            }
            Some(Desired::Stop {
                generation,
                session_epoch,
            }) => {
                if let Some(existing) = owned.take() {
                    existing.stop.store(true, Ordering::Release);
                    if let Err(error) = backend_call(|| backend.stop(existing.running)) {
                        finish_cleanup_failure(
                            &shared,
                            generation,
                            session_epoch,
                            existing.path,
                            format!("{error:#}"),
                        );
                        defer_cleanup_retry(
                            &shared,
                            (!shutdown).then_some(Desired::Stop {
                                generation,
                                session_epoch,
                            }),
                            shutdown,
                            &mut retry_delay,
                        );
                        continue;
                    }
                    clear_descriptor_after_cleanup(&shared, &existing.path);
                    retry_delay = std::time::Duration::from_millis(100);
                }
                finish_stopped(&shared, generation, session_epoch);
            }
            None if shutdown => {
                if let Some(existing) = owned.take() {
                    existing.stop.store(true, Ordering::Release);
                    if let Err(error) = backend_call(|| backend.stop(existing.running)) {
                        finish_cleanup_failure(
                            &shared,
                            existing.generation,
                            existing.session_epoch,
                            existing.path,
                            format!("{error:#}"),
                        );
                        defer_cleanup_retry(&shared, None, true, &mut retry_delay);
                        continue;
                    }
                    clear_descriptor_after_cleanup(&shared, &existing.path);
                }
                if lock(&shared.state).descriptor_path.is_some() {
                    retry_delay = retry_delay
                        .saturating_mul(2)
                        .min(std::time::Duration::from_secs(10));
                    continue;
                }
                let (generation, session_epoch) = current_ticket(&shared);
                finish_stopped(&shared, generation, session_epoch);
                break;
            }
            None => {}
        }
    }
    lock(&shared.state).worker_alive = false;
}

fn backend_call<T>(operation: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)).map_err(|payload| {
        anyhow!(
            "Agent Broker lifecycle operation panicked: {}",
            panic_message(payload)
        )
    })?
}

fn descriptor_path() -> Result<PathBuf> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    Ok(std::env::temp_dir().join(format!(
        "protoinputmultiplex-agent-{}-{nonce}.json",
        std::process::id()
    )))
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn current_ticket(shared: &Shared) -> (u64, u64) {
    let state = lock(&shared.state);
    (state.generation, state.session_epoch)
}

fn desired_ticket(desired: &Desired) -> (u64, u64) {
    match desired {
        Desired::Start {
            generation,
            request,
            ..
        } => (*generation, request.session_epoch),
        Desired::Stop {
            generation,
            session_epoch,
        } => (*generation, *session_epoch),
    }
}

fn defer_cleanup_retry(
    shared: &Shared,
    pending: Option<Desired>,
    shutdown: bool,
    retry_delay: &mut std::time::Duration,
) {
    *retry_delay = retry_delay
        .saturating_mul(2)
        .min(std::time::Duration::from_secs(10));
    if !shutdown {
        if let Some(pending) = pending {
            let (generation, session_epoch) = desired_ticket(&pending);
            let mut state = lock(&shared.state);
            if state.desired.is_none()
                && state.generation == generation
                && state.session_epoch == session_epoch
            {
                state.desired = Some(pending);
            }
        }
        let state = lock(&shared.state);
        drop(
            shared
                .wake
                .wait_timeout(state, *retry_delay)
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
    }
}

fn is_current(shared: &Shared, generation: u64, session_epoch: u64) -> bool {
    let state = lock(&shared.state);
    !state.shutdown
        && state.generation == generation
        && state.session_epoch == session_epoch
        && matches!(state.phase, Phase::Starting | Phase::Active)
}

fn set_phase_if_current(shared: &Shared, generation: u64, session_epoch: u64, phase: Phase) {
    let mut state = lock(&shared.state);
    if state.generation == generation && state.session_epoch == session_epoch {
        state.phase = phase;
    }
}

fn set_active(
    shared: &Shared,
    generation: u64,
    session_epoch: u64,
    path: PathBuf,
    stop: Arc<AtomicBool>,
    completion: Completion,
) {
    let mut state = lock(&shared.state);
    if state.generation == generation && state.session_epoch == session_epoch {
        state.phase = Phase::Active;
        state.descriptor_path = Some(path);
        state.active_stop = Some(stop);
        state.completion = Some(completion);
    }
}

fn set_pending_path(shared: &Shared, generation: u64, session_epoch: u64, path: PathBuf) {
    let mut state = lock(&shared.state);
    if state.generation == generation && state.session_epoch == session_epoch {
        state.descriptor_path = Some(path);
    }
}

fn finish_start_failure(shared: &Shared, generation: u64, session_epoch: u64, error: String) {
    let mut state = lock(&shared.state);
    if state.generation == generation && state.session_epoch == session_epoch {
        state.latest_start = None;
        state.phase = Phase::StartFailed(error.clone());
        state.completion = Some(Completion {
            generation,
            session_epoch,
            kind: CompletionKind::StartFailed(error),
        });
    }
}

fn cancel_start_if_current(shared: &Shared, generation: u64, session_epoch: u64) {
    let mut state = lock(&shared.state);
    if state.latest_start == Some((generation, session_epoch)) {
        state.latest_start = None;
    }
}

fn finish_cleanup_failure(
    shared: &Shared,
    generation: u64,
    session_epoch: u64,
    path: PathBuf,
    error: String,
) {
    let mut state = lock(&shared.state);
    if state.generation == generation && state.session_epoch == session_epoch {
        state.phase = Phase::CleanupFailed(error.clone());
        state.descriptor_path = Some(path);
        state.active_stop = None;
        state.completion = Some(Completion {
            generation,
            session_epoch,
            kind: CompletionKind::CleanupFailed(error),
        });
    }
}

fn clear_descriptor_after_cleanup(shared: &Shared, path: &Path) {
    let mut state = lock(&shared.state);
    if state.descriptor_path.as_deref() == Some(path) {
        state.descriptor_path = None;
        state.active_stop = None;
    }
}

fn finish_stopped(shared: &Shared, generation: u64, session_epoch: u64) {
    let mut state = lock(&shared.state);
    if state.generation == generation && state.session_epoch == session_epoch {
        state.latest_start = None;
        state.phase = Phase::Stopped;
        state.descriptor_path = None;
        state.active_stop = None;
        state.completion = Some(Completion {
            generation,
            session_epoch,
            kind: CompletionKind::Stopped,
        });
    }
}

fn mark_worker_failed(shared: &Shared, error: String) {
    let mut state = lock(&shared.state);
    state.worker_alive = false;
    state.latest_start = None;
    state.phase = Phase::WorkerFailed(error.clone());
    state.completion = Some(Completion {
        generation: state.generation,
        session_epoch: state.session_epoch,
        kind: CompletionKind::WorkerFailed(error),
    });
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "Agent Broker worker panicked".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{Condvar, mpsc},
        time::{Duration, Instant},
    };

    #[derive(Default)]
    struct Gate {
        open: Mutex<bool>,
        changed: Condvar,
    }

    impl Gate {
        fn wait(&self) {
            let mut open = lock(&self.open);
            while !*open {
                open = self
                    .changed
                    .wait(open)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }

        fn release(&self) {
            *lock(&self.open) = true;
            self.changed.notify_all();
        }
    }

    struct FakeBackend {
        events: mpsc::Sender<&'static str>,
        start_gate: Arc<Gate>,
        cleanup_gate: Arc<Gate>,
        descriptor_gate: Arc<Gate>,
        coalesce_hook: Option<Arc<CoalesceHook>>,
        fail_stop_once: Arc<AtomicBool>,
        fail_descriptor_cleanup_once: Arc<AtomicBool>,
        cleanup_attempts: Arc<std::sync::atomic::AtomicUsize>,
    }

    struct CoalesceHook {
        pause_next_take: AtomicBool,
        first_start: AtomicBool,
        gate: Arc<Gate>,
        events: mpsc::Sender<&'static str>,
    }

    impl Backend for FakeBackend {
        type Running = ();

        fn before_take(&mut self) {
            if let Some(hook) = &self.coalesce_hook
                && hook.pause_next_take.swap(false, Ordering::AcqRel)
            {
                hook.events.send("command-take-paused").unwrap();
                hook.gate.wait();
            }
        }

        fn start(
            &mut self,
            _request: StartRequest,
            _descriptor_path: PathBuf,
            _generation: u64,
            _current_generation: Arc<AtomicU64>,
            _stop: Arc<AtomicBool>,
        ) -> Result<Self::Running> {
            self.events.send("start-entered").unwrap();
            self.start_gate.wait();
            if let Some(hook) = &self.coalesce_hook
                && hook.first_start.swap(false, Ordering::AcqRel)
            {
                hook.pause_next_take.store(true, Ordering::Release);
            }
            Ok(())
        }

        fn stop(&mut self, _running: Self::Running) -> Result<()> {
            self.events.send("cleanup-entered").unwrap();
            self.cleanup_gate.wait();
            if self.fail_stop_once.swap(false, Ordering::AcqRel) {
                anyhow::bail!("injected broker shutdown failure");
            }
            Ok(())
        }

        fn retry_descriptor_cleanup(&mut self, _path: &Path) -> Result<()> {
            self.events.send("descriptor-cleanup-entered").unwrap();
            self.descriptor_gate.wait();
            self.cleanup_attempts.fetch_add(1, Ordering::Relaxed);
            if self
                .fail_descriptor_cleanup_once
                .swap(false, Ordering::AcqRel)
            {
                anyhow::bail!("injected descriptor cleanup failure");
            }
            Ok(())
        }
    }

    fn fake_service() -> (
        Service<FakeBackend>,
        mpsc::Receiver<&'static str>,
        Arc<Gate>,
        Arc<Gate>,
    ) {
        let (events_tx, events_rx) = mpsc::channel();
        let start_gate = Arc::new(Gate::default());
        let cleanup_gate = Arc::new(Gate::default());
        let descriptor_gate = Arc::new(Gate::default());
        descriptor_gate.release();
        let service = Service::start_with_backend(FakeBackend {
            events: events_tx,
            start_gate: Arc::clone(&start_gate),
            cleanup_gate: Arc::clone(&cleanup_gate),
            descriptor_gate,
            coalesce_hook: None,
            fail_stop_once: Arc::new(AtomicBool::new(false)),
            fail_descriptor_cleanup_once: Arc::new(AtomicBool::new(false)),
            cleanup_attempts: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
        .unwrap();
        (service, events_rx, start_gate, cleanup_gate)
    }

    fn await_phase<B: Backend>(service: &Service<B>, expected: fn(&Phase) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if expected(&service.status().phase) {
                return;
            }
            thread::yield_now();
        }
        panic!(
            "Agent Broker phase did not reach expected state: {:?}",
            service.status().phase
        );
    }

    fn await_shared(shared: &Shared, expected: fn(&Phase) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if expected(&lock(&shared.state).phase) {
                return;
            }
            thread::yield_now();
        }
        panic!(
            "Agent Broker phase did not reach expected state: {:?}",
            lock(&shared.state).phase
        );
    }

    fn await_worker_exit(shared: &Shared) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let state = lock(&shared.state);
            if !state.worker_alive && matches!(state.phase, Phase::Stopped) {
                return;
            }
            drop(state);
            thread::yield_now();
        }
        let state = lock(&shared.state);
        panic!(
            "Agent Broker worker did not stop cleanly: alive={}, phase={:?}",
            state.worker_alive, state.phase
        );
    }

    fn fake_start(service: &Service<FakeBackend>, epoch: u64) -> u64 {
        service
            .enqueue_start(StartRequest {
                session_epoch: epoch,
                session: None,
                app: None,
                setup: None,
            })
            .unwrap()
    }

    fn enable_command() -> crate::agent_runtime::Command {
        crate::agent_runtime::Command::Configure {
            enabled: true,
            user_ids: vec!["latest-user".into()],
            allowed_pids: vec![42],
            capture: false,
            desktop_capture: false,
            session_control: false,
            app_preferences: false,
        }
    }

    #[test]
    fn enable_commit_requires_current_broker_session_and_pending_generation() {
        assert!(enable_completion_matches(
            3,
            6,
            Some((3, 6)),
            true,
            6,
            true,
            false
        ));
        assert!(!enable_completion_matches(
            3,
            6,
            Some((2, 6)),
            true,
            6,
            true,
            false
        ));
        assert!(!enable_completion_matches(
            3,
            6,
            Some((3, 6)),
            false,
            6,
            true,
            false
        ));
        assert!(!enable_completion_matches(
            3,
            6,
            Some((3, 6)),
            true,
            7,
            true,
            false
        ));
        assert!(!enable_completion_matches(
            3,
            6,
            Some((3, 6)),
            true,
            6,
            false,
            false
        ));
        assert!(!enable_completion_matches(
            3,
            6,
            Some((3, 6)),
            true,
            6,
            true,
            true
        ));
    }

    #[test]
    fn ordinary_quit_waits_for_both_session_and_broker_terminal_state() {
        assert!(!quit_ready(false, &Phase::Stopped));
        assert!(!quit_ready(true, &Phase::Stopping));
        assert!(!quit_ready(
            true,
            &Phase::CleanupFailed("still owned".into())
        ));
        assert!(quit_ready(true, &Phase::Stopped));
    }

    #[test]
    fn failed_start_completes_only_its_matching_gui_request() {
        let command = || crate::agent_runtime::Command::Configure {
            enabled: true,
            user_ids: Vec::new(),
            allowed_pids: Vec::new(),
            capture: false,
            desktop_capture: false,
            session_control: false,
            app_preferences: false,
        };
        let mut page = crate::agent_ui::AgentPage::default();
        let mut pending = Some((5, 8, command()));

        assert!(fail_pending_enable(
            &mut page,
            &mut pending,
            5,
            8,
            "descriptor creation failed".into(),
        ));
        assert!(pending.is_none());
        assert_eq!(
            page.snapshot().error.as_deref(),
            Some("descriptor creation failed")
        );
        assert!(!page.snapshot().pending);

        let mut newer_pending = Some((6, 9, command()));
        assert!(!fail_pending_enable(
            &mut page,
            &mut newer_pending,
            5,
            8,
            "stale startup failure".into(),
        ));
        assert!(newer_pending.is_some());
        assert_eq!(
            page.snapshot().error.as_deref(),
            Some("descriptor creation failed")
        );
    }

    #[test]
    fn blocked_start_is_cancelled_without_publishing_started_completion() {
        let (service, events, start_gate, cleanup_gate) = fake_service();
        let generation = fake_start(&service, 4);
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "start-entered"
        );

        let stop_generation = service.request_stop(5);
        assert!(stop_generation > generation);
        assert!(!service.is_current(generation, 4));
        assert_eq!(service.status().phase, Phase::Stopping);

        start_gate.release();
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "cleanup-entered"
        );
        assert_eq!(service.status().phase, Phase::Stopping);
        assert!(service.status().descriptor_path.is_some());
        cleanup_gate.release();
        await_phase(&service, |phase| matches!(phase, Phase::Stopped));
        assert!(restart_ready(true, true, true, &service.status().phase));
        assert!(!restart_ready(false, true, true, &service.status().phase));
        assert!(!restart_ready(true, false, true, &service.status().phase));
        assert!(!restart_ready(true, true, false, &service.status().phase));
        assert!(service.status().descriptor_path.is_none());
        assert!(service.take_completion().is_some_and(|completion| {
            completion.generation == stop_generation
                && matches!(completion.kind, CompletionKind::Stopped)
        }));
    }

    #[test]
    fn blocked_cleanup_keeps_gui_side_requests_nonblocking_and_owned() {
        let (service, events, start_gate, cleanup_gate) = fake_service();
        let generation = fake_start(&service, 8);
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "start-entered"
        );
        start_gate.release();
        await_phase(&service, |phase| matches!(phase, Phase::Active));
        assert!(service.take_completion().is_some_and(|completion| {
            completion.generation == generation
                && matches!(completion.kind, CompletionKind::Started(_))
        }));

        let stop_generation = service.request_stop(9);
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "cleanup-entered"
        );
        let before_release = service.status();
        assert!(!restart_ready(true, true, true, &before_release.phase));
        assert!(!restart_ready(
            true,
            true,
            true,
            &Phase::CleanupFailed("retained".into())
        ));
        assert_eq!(before_release.phase, Phase::Stopping);
        assert!(before_release.worker_alive);
        assert!(!service.is_current(generation, 8));

        cleanup_gate.release();
        await_phase(&service, |phase| matches!(phase, Phase::Stopped));
        assert!(service.status().descriptor_path.is_none());
        assert!(service.take_completion().is_some_and(|completion| {
            completion.generation == stop_generation
                && matches!(completion.kind, CompletionKind::Stopped)
        }));
    }

    #[test]
    fn repeated_automatic_retirement_preserves_owned_cleanup_failure_and_retry() {
        let (events_tx, events) = mpsc::channel();
        let start_gate = Arc::new(Gate::default());
        start_gate.release();
        let cleanup_gate = Arc::new(Gate::default());
        cleanup_gate.release();
        let descriptor_gate = Arc::new(Gate::default());
        let service = Service::start_with_backend(FakeBackend {
            events: events_tx,
            start_gate,
            cleanup_gate,
            descriptor_gate: Arc::clone(&descriptor_gate),
            coalesce_hook: None,
            fail_stop_once: Arc::new(AtomicBool::new(true)),
            fail_descriptor_cleanup_once: Arc::new(AtomicBool::new(false)),
            cleanup_attempts: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
        .unwrap();
        fake_start(&service, 70);
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "start-entered"
        );
        await_phase(&service, |phase| matches!(phase, Phase::Active));
        service.take_completion();
        assert!(service.request_automatic_stop(70));
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "cleanup-entered"
        );
        await_phase(&service, |phase| matches!(phase, Phase::CleanupFailed(_)));
        let failed_generation = service.status().generation;
        for _ in 0..64 {
            assert!(!service.request_automatic_stop(70));
            assert_eq!(service.status().generation, failed_generation);
        }
        assert!(
            service
                .take_completion()
                .is_some_and(|completion| completion.generation == failed_generation
                    && matches!(completion.kind, CompletionKind::CleanupFailed(_)))
        );
        // The sole owner's existing backoff/retry still proceeds. Block its
        // deletion so repeated GUI polls cannot erase or replace that ownership.
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "descriptor-cleanup-entered"
        );
        assert!(service.status().descriptor_path.is_some());
        for _ in 0..64 {
            assert!(!service.request_automatic_stop(70));
            assert_eq!(service.status().generation, failed_generation);
        }
        descriptor_gate.release();
        await_phase(&service, |phase| matches!(phase, Phase::Stopped));
        assert_eq!(service.status().generation, failed_generation);
        assert!(service.status().descriptor_path.is_none());
    }

    #[test]
    fn coalesced_start_survives_cleanup_failure_through_gui_configure() {
        let (events_tx, events) = mpsc::channel();
        let start_gate = Arc::new(Gate::default());
        start_gate.release();
        let cleanup_gate = Arc::new(Gate::default());
        let descriptor_gate = Arc::new(Gate::default());
        let take_gate = Arc::new(Gate::default());
        let hook = Arc::new(CoalesceHook {
            pause_next_take: AtomicBool::new(false),
            first_start: AtomicBool::new(true),
            gate: Arc::clone(&take_gate),
            events: events_tx.clone(),
        });
        let service = Service::start_with_backend(FakeBackend {
            events: events_tx,
            start_gate,
            cleanup_gate: Arc::clone(&cleanup_gate),
            descriptor_gate: Arc::clone(&descriptor_gate),
            coalesce_hook: Some(hook),
            fail_stop_once: Arc::new(AtomicBool::new(true)),
            fail_descriptor_cleanup_once: Arc::new(AtomicBool::new(false)),
            cleanup_attempts: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
        .unwrap();

        let first_generation = fake_start(&service, 10);
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "start-entered"
        );
        await_phase(&service, |phase| matches!(phase, Phase::Active));
        assert!(service.take_completion().is_some_and(|completion| {
            completion.generation == first_generation
                && matches!(completion.kind, CompletionKind::Started(_))
        }));
        let superseded_stop = service.request_stop(11);
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "command-take-paused"
        );

        let latest_generation = service
            .enqueue_start(StartRequest {
                session_epoch: 12,
                session: None,
                app: None,
                setup: None,
            })
            .unwrap();
        assert!(latest_generation > superseded_stop);
        let mut page = crate::agent_ui::AgentPage::default();
        let mut pending = Some((latest_generation, 12, enable_command()));
        let gui_session = || GuiSession {
            epoch: 12,
            live: true,
            stopping: false,
        };
        take_gate.release();

        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "cleanup-entered"
        );
        assert_eq!(service.status().phase, Phase::Stopping);
        assert!(
            events.try_recv().is_err(),
            "new broker started before old cleanup"
        );
        cleanup_gate.release();

        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "descriptor-cleanup-entered"
        );
        {
            let snapshot = service.status();
            assert!(matches!(snapshot.phase, Phase::CleanupFailed(_)));
            assert!(snapshot.descriptor_path.is_some());
        }
        let failure = service.take_completion().unwrap();
        assert_eq!(failure.generation, latest_generation);
        assert_eq!(failure.session_epoch, 12);
        assert!(matches!(failure.kind, CompletionKind::CleanupFailed(_)));
        assert!(matches!(
            apply_gui_completion(&mut page, &mut pending, failure, &service, gui_session()),
            GuiTransition::Status(_)
        ));
        assert!(pending.as_ref().is_some_and(|(generation, epoch, _)| {
            *generation == latest_generation && *epoch == 12
        }));
        assert!(page.snapshot().error.is_none());
        assert!(
            events.try_recv().is_err(),
            "new broker started before retry cleanup"
        );
        descriptor_gate.release();
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "start-entered"
        );
        await_phase(&service, |phase| matches!(phase, Phase::Active));
        let started = service.take_completion().unwrap();
        assert_eq!(started.generation, latest_generation);
        assert_eq!(started.session_epoch, 12);
        let GuiTransition::Configure(path, command) = apply_gui_completion(
            &mut page,
            &mut pending,
            started.clone(),
            &service,
            gui_session(),
        ) else {
            panic!("current retained Start did not produce its GUI Configure");
        };
        assert_eq!(service.status().descriptor_path.as_ref(), Some(&path));
        assert!(matches!(command, crate::agent_runtime::Command::Configure {
            enabled: true, user_ids, allowed_pids, ..
        } if user_ids == vec!["latest-user"] && allowed_pids == vec![42]));
        assert!(pending.is_none());
        // The actual GUI transition consumes the command exactly once.
        assert!(matches!(
            apply_gui_completion(&mut page, &mut pending, started, &service, gui_session()),
            GuiTransition::Retire
        ));
        assert!(service.take_completion().is_none());
        service.request_stop(12);
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "cleanup-entered"
        );
        await_phase(&service, |phase| matches!(phase, Phase::Stopped));
    }

    #[test]
    fn same_epoch_stop_cancels_start_while_it_retires_the_old_broker() {
        let (service, events, start_gate, cleanup_gate) = fake_service();
        fake_start(&service, 30);
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "start-entered"
        );
        start_gate.release();
        await_phase(&service, |phase| matches!(phase, Phase::Active));
        service.take_completion();

        let replacement = fake_start(&service, 31);
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "cleanup-entered"
        );
        assert_eq!(service.status().phase, Phase::Stopping);
        let stop_generation = service.request_stop(31);
        assert!(stop_generation > replacement);
        assert!(!service.retains_start(replacement, 31));
        assert!(!service.is_current(replacement, 31));
        cleanup_gate.release();
        await_phase(&service, |phase| matches!(phase, Phase::Stopped));
        assert!(service.status().descriptor_path.is_none());
        assert!(service.take_completion().is_some_and(|completion| {
            completion.generation == stop_generation
                && completion.session_epoch == 31
                && matches!(completion.kind, CompletionKind::Stopped)
        }));
        assert!(events.try_recv().is_err(), "canceled replacement started");
    }

    #[test]
    fn service_drop_retains_and_retries_failed_descriptor_cleanup() {
        let (events_tx, events) = mpsc::channel();
        let start_gate = Arc::new(Gate::default());
        start_gate.release();
        let cleanup_gate = Arc::new(Gate::default());
        cleanup_gate.release();
        let descriptor_gate = Arc::new(Gate::default());
        let cleanup_attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let service = Service::start_with_backend(FakeBackend {
            events: events_tx,
            start_gate,
            cleanup_gate,
            descriptor_gate: Arc::clone(&descriptor_gate),
            coalesce_hook: None,
            fail_stop_once: Arc::new(AtomicBool::new(true)),
            fail_descriptor_cleanup_once: Arc::new(AtomicBool::new(true)),
            cleanup_attempts: Arc::clone(&cleanup_attempts),
        })
        .unwrap();
        fake_start(&service, 20);
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "start-entered"
        );
        await_phase(&service, |phase| matches!(phase, Phase::Active));
        let shared = Arc::clone(&service.shared);

        drop(service);
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "cleanup-entered"
        );
        await_shared(&shared, |phase| matches!(phase, Phase::CleanupFailed(_)));
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "descriptor-cleanup-entered"
        );
        {
            let state = lock(&shared.state);
            assert!(state.descriptor_path.is_some());
            assert!(!matches!(state.phase, Phase::Stopped));
        }

        descriptor_gate.release();
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "descriptor-cleanup-entered"
        );
        await_shared(&shared, |phase| matches!(phase, Phase::Stopped));
        await_worker_exit(&shared);
        let state = lock(&shared.state);
        assert!(state.descriptor_path.is_none());
        assert!(!state.worker_alive);
        assert!(cleanup_attempts.load(Ordering::Relaxed) >= 2);
    }
}
