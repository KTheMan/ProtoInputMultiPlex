//! Serial owner of preferences storage, startup registration and lifecycle teardown.
//! The GUI only submits immutable jobs and polls; it never joins this worker.
use crate::{
    app_lifecycle::{CommitDecision, CommitFailure},
    app_preferences::{self, Preferences, StartupValue},
};
use anyhow::{Context, Result, ensure};
use eframe::egui;
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Configure,
    RetryShortcuts,
    RetryEmergency,
    Shutdown,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    Local,
    Connected {
        client: u64,
        sequence: u64,
        authority: (u64, u64, u64),
    },
    Setup {
        client: u64,
        sequence: u64,
        epoch: u64,
    },
}
impl Origin {
    pub fn local(&self) -> bool {
        matches!(self, Self::Local)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Committed,
    Cancelled,
    Failed,
    Unconfirmed,
    Retried,
    ShutdownComplete,
}

struct Job {
    ticket: u64,
    revision: u64,
    recover: bool,
    kind: Kind,
    origin: Origin,
    candidate: Option<Preferences>,
    decision: Arc<CommitDecision>,
    repaint_context: Option<egui::Context>,
}
#[derive(Clone)]
struct Pending {
    ticket: u64,
    kind: Kind,
    origin: Origin,
}
pub struct Completion {
    pub ticket: u64,
    pub revision: u64,
    pub kind: Kind,
    pub origin: Origin,
    pub committed: Option<Preferences>,
    pub outcome: Outcome,
    pub error: Option<String>,
}
#[derive(Debug)]
struct Failure {
    message: String,
    uncertain: bool,
    cancelled: bool,
}
impl Failure {
    fn before(error: impl std::fmt::Display) -> Self {
        Self {
            message: error.to_string(),
            uncertain: false,
            cancelled: false,
        }
    }
    fn lifecycle(error: crate::app_lifecycle::OperationFailure) -> Self {
        Self {
            message: format!("{:#}", error.error),
            uncertain: error.uncertain,
            cancelled: false,
        }
    }
    fn unknown(error: impl std::fmt::Display) -> Self {
        Self {
            message: error.to_string(),
            uncertain: true,
            cancelled: false,
        }
    }
}
trait Provider: Send + 'static {
    type Stage;
    fn original(&mut self) -> Result<Option<Vec<u8>>>;
    fn save(&mut self, candidate: &Preferences) -> Result<()>;
    fn restore(&mut self, original: Option<&[u8]>) -> Result<()>;
    fn startup_snapshot(&mut self) -> Result<Option<StartupValue>>;
    fn startup_value(&mut self) -> Result<StartupValue>;
    fn startup_write(&mut self, value: Option<&StartupValue>) -> Result<()>;
    fn prepare(
        &mut self,
        candidate: &Preferences,
    ) -> std::result::Result<Self::Stage, crate::app_lifecycle::OperationFailure>;
    fn rollback(&mut self, stage: Self::Stage) -> Result<()>;
    fn commit(
        &mut self,
        stage: Self::Stage,
        decision: Arc<CommitDecision>,
    ) -> std::result::Result<(), CommitFailure>;
    fn retry(
        &mut self,
        kind: Kind,
    ) -> std::result::Result<(), crate::app_lifecycle::OperationFailure>;
    fn shutdown(&mut self) -> Result<()>;
}
struct WindowsProvider {
    path: PathBuf,
    executable: PathBuf,
    registration: Box<dyn app_preferences::StartupRegistration>,
    lifecycle: crate::app_lifecycle::Service,
}
impl Provider for WindowsProvider {
    type Stage = crate::app_lifecycle::Prepared;
    fn original(&mut self) -> Result<Option<Vec<u8>>> {
        app_preferences::Store::original(&self.path)
    }
    fn save(&mut self, candidate: &Preferences) -> Result<()> {
        app_preferences::Store::save(&self.path, candidate)
    }
    fn restore(&mut self, original: Option<&[u8]>) -> Result<()> {
        app_preferences::Store::restore_original(&self.path, original)
    }
    fn startup_snapshot(&mut self) -> Result<Option<StartupValue>> {
        self.registration.snapshot()
    }
    fn startup_value(&mut self) -> Result<StartupValue> {
        app_preferences::startup_command_value(&self.executable, &self.path)
    }
    fn startup_write(&mut self, value: Option<&StartupValue>) -> Result<()> {
        self.registration.write(value)
    }
    fn prepare(
        &mut self,
        candidate: &Preferences,
    ) -> std::result::Result<Self::Stage, crate::app_lifecycle::OperationFailure> {
        self.lifecycle.handle().prepare_preferences(candidate)
    }
    fn rollback(&mut self, stage: Self::Stage) -> Result<()> {
        stage.rollback()
    }
    fn commit(
        &mut self,
        stage: Self::Stage,
        decision: Arc<CommitDecision>,
    ) -> std::result::Result<(), CommitFailure> {
        stage.commit(decision)
    }
    fn retry(
        &mut self,
        kind: Kind,
    ) -> std::result::Result<(), crate::app_lifecycle::OperationFailure> {
        match kind {
            Kind::RetryShortcuts => self.lifecycle.handle().retry_shortcuts(),
            Kind::RetryEmergency => self.lifecycle.handle().retry_emergency(),
            _ => Err(crate::app_lifecycle::OperationFailure {
                error: anyhow::anyhow!("not a retry"),
                uncertain: false,
            }),
        }
    }
    fn shutdown(&mut self) -> Result<()> {
        self.lifecycle.shutdown()
    }
}

/// Attempt every compensation even when an earlier compensation fails.
fn compensate<P: Provider>(
    provider: &mut P,
    original: Option<&[u8]>,
    restore_file: bool,
    startup: Option<&Option<StartupValue>>,
    stage: Option<P::Stage>,
) -> Vec<String> {
    let mut errors = Vec::new();
    if restore_file && let Err(error) = provider.restore(original) {
        errors.push(format!("file rollback failed: {error:#}"));
    }
    if let Some(startup) = startup
        && let Err(error) = provider.startup_write(startup.as_ref())
    {
        errors.push(format!("startup rollback failed: {error:#}"));
    }
    if let Some(stage) = stage
        && let Err(error) = provider.rollback(stage)
    {
        errors.push(format!("shortcut rollback failed: {error:#}"));
    }
    errors
}
fn failed<P: Provider>(
    provider: &mut P,
    error: impl std::fmt::Display,
    cancelled: bool,
    original: Option<&[u8]>,
    restore_file: bool,
    startup: Option<&Option<StartupValue>>,
    stage: Option<P::Stage>,
) -> Failure {
    let errors = compensate(provider, original, restore_file, startup, stage);
    Failure {
        message: if errors.is_empty() {
            error.to_string()
        } else {
            format!("{error}; {}", errors.join("; "))
        },
        uncertain: !errors.is_empty(),
        cancelled: cancelled && errors.is_empty(),
    }
}
fn transact<P: Provider>(
    provider: &mut P,
    original: &Preferences,
    candidate: &Preferences,
    local: bool,
    decision: Arc<CommitDecision>,
    reconcile: bool,
) -> std::result::Result<(), Failure> {
    candidate.validate().map_err(Failure::before)?;
    if !local {
        app_preferences::reject_mcp_login_change(original, candidate).map_err(Failure::before)?;
    }
    if decision.cancelled() {
        return Err(Failure {
            message: "Application preference request cancelled before staging.".into(),
            uncertain: false,
            cancelled: true,
        });
    }
    let original_file = provider.original().map_err(Failure::before)?;
    let startup_changed =
        local && (reconcile || candidate.launch_at_login != original.launch_at_login);
    let old_startup = if startup_changed {
        Some(provider.startup_snapshot().map_err(Failure::before)?)
    } else {
        None
    };
    let startup_value = if startup_changed && candidate.launch_at_login {
        Some(provider.startup_value().map_err(Failure::before)?)
    } else {
        None
    };
    let stage = provider.prepare(candidate).map_err(Failure::lifecycle)?;
    if startup_changed && let Err(error) = provider.startup_write(startup_value.as_ref()) {
        return Err(failed(
            provider,
            format!("Update startup registration: {error:#}"),
            false,
            original_file.as_deref(),
            false,
            old_startup.as_ref(),
            Some(stage),
        ));
    }
    if decision.cancelled() {
        return Err(failed(
            provider,
            "Application preference request cancelled before save.",
            true,
            original_file.as_deref(),
            false,
            old_startup.as_ref(),
            Some(stage),
        ));
    }
    // Store::save is atomic and preserves an invalid existing destination on
    // failure. Do not overwrite that file with an older snapshot on save error.
    if let Err(error) = provider.save(candidate) {
        return Err(failed(
            provider,
            format!("Save application preferences: {error:#}"),
            false,
            original_file.as_deref(),
            false,
            old_startup.as_ref(),
            Some(stage),
        ));
    }
    if decision.cancelled() {
        return Err(failed(
            provider,
            "Application preference request cancelled before activation.",
            true,
            original_file.as_deref(),
            true,
            old_startup.as_ref(),
            Some(stage),
        ));
    }
    if let Err(error) = provider.commit(stage, decision.clone()) {
        // A lost claimed ACK is not proof activation failed: never undo its file
        // beneath potentially active shortcuts. Surface concrete uncertainty.
        if !error.rollback_files {
            return Err(Failure::unknown(format!(
                "Activation outcome unconfirmed: {:#}",
                error.error
            )));
        }
        let mut failure = failed(
            provider,
            format!("Activate application preferences: {:#}", error.error),
            decision.cancelled(),
            original_file.as_deref(),
            true,
            old_startup.as_ref(),
            None,
        );
        failure.uncertain |= error.uncertain;
        failure.cancelled &= !failure.uncertain;
        return Err(failure);
    }
    Ok(())
}

impl Completion {
    pub fn permits_waiting_quit(&self) -> bool {
        !matches!(self.outcome, Outcome::Failed | Outcome::Unconfirmed)
    }
}

#[derive(Serialize)]
pub struct Status {
    pub ticket: u64,
    pub revision: u64,
    pub kind: Option<Kind>,
    pub origin: Option<&'static str>,
    pub pending: bool,
    pub retiring: bool,
    pub shutdown_complete: bool,
    pub degraded: bool,
    pub result: Option<Outcome>,
    pub error: Option<String>,
    pub artificial_delay_ms: u64,
}
pub struct Service {
    jobs: SyncSender<Job>,
    replies: Receiver<Completion>,
    pending: Option<Pending>,
    ticket: u64,
    revision: u64,
    last_kind: Option<Kind>,
    last_origin: Option<&'static str>,
    retiring: bool,
    shutdown_complete: bool,
    degraded: bool,
    outcome: Option<Outcome>,
    error: Option<String>,
    delay: Duration,
    repaint_context: Option<egui::Context>,
}
impl Service {
    pub fn start(
        path: PathBuf,
        registration: Box<dyn app_preferences::StartupRegistration>,
        lifecycle: crate::app_lifecycle::Service,
        initial: Preferences,
        delay: Duration,
        guards: Arc<crate::gui::RuntimeGuards>,
    ) -> Result<Self> {
        ensure!(
            delay <= Duration::from_secs(15),
            "Preference delay exceeds 15 seconds"
        );
        Self::start_with_guards(
            WindowsProvider {
                path,
                executable: std::env::current_exe()?,
                registration,
                lifecycle,
            },
            initial,
            delay,
            Some(guards),
        )
    }
    #[cfg(test)]
    fn start_with(provider: impl Provider, actual: Preferences, delay: Duration) -> Result<Self> {
        Self::start_with_guards(provider, actual, delay, None)
    }
    fn start_with_guards(
        mut provider: impl Provider,
        mut actual: Preferences,
        delay: Duration,
        guards: Option<Arc<crate::gui::RuntimeGuards>>,
    ) -> Result<Self> {
        let (jobs, requests) = mpsc::sync_channel::<Job>(1);
        let (responses, replies) = mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("gui-app-preferences".into())
            .spawn(move || {
                let _guards = guards;
                let mut revision = 0u64;
                let mut degraded = false;
                let mut retiring = false;
                while let Ok(job) = requests.recv() {
                    let mut committed = None;
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        if retiring {
                            return Err(Failure::unknown("Application service shutdown was already attempted; its outcome must not be replayed."));
                        }
                        if !delay.is_zero() { std::thread::sleep(delay); }
                        // Cleanup/retries never depend on an old view revision.
                        // Explicit local recovery reestablishes a full candidate.
                        if job.kind == Kind::Configure && job.revision != revision && !job.recover {
                            return Err(Failure::before("Application preferences revision changed."));
                        }
                        match job.kind {
                            Kind::Configure => {
                                if degraded && !job.origin.local() {
                                    return Err(Failure::before("Apply application preferences locally to recover the uncertain state."));
                                }
                                let candidate = job.candidate.as_ref().context("missing preference candidate").map_err(Failure::before)?;
                                transact(&mut provider, &actual, candidate, job.origin.local(), job.decision.clone(), degraded || job.recover)?;
                                actual = candidate.clone();
                                revision = revision.checked_add(1).expect("preferences revision exhausted");
                                committed = Some(actual.clone());
                                degraded = false;
                                Ok(Outcome::Committed)
                            }
                            Kind::RetryShortcuts | Kind::RetryEmergency => {
                                provider.retry(job.kind).map_err(Failure::lifecycle)?;
                                Ok(Outcome::Retried)
                            }
                            Kind::Shutdown => {
                                retiring = true;
                                provider.shutdown().map_err(Failure::unknown)?;
                                Ok(Outcome::ShutdownComplete)
                            }
                        }
                    }));
                    let (outcome,error) = match result {
                        Ok(Ok(outcome)) => (outcome,None),
                        Ok(Err(failure)) => {
                            degraded |= failure.uncertain;
                            let outcome = if failure.uncertain { Outcome::Unconfirmed } else if failure.cancelled { Outcome::Cancelled } else { Outcome::Failed };
                            (outcome,Some(failure.message))
                        }
                        Err(_) => {
                            degraded = true;
                            (Outcome::Unconfirmed,Some("Application preferences worker panicked; final state is unconfirmed.".into()))
                        }
                    };
                    let completion = Completion {
                        ticket: job.ticket,
                        revision,
                        kind: job.kind,
                        origin: job.origin,
                        committed,
                        outcome,
                        error,
                    };
                    if responses.try_send(completion).is_err() {
                        break;
                    }
                    // A minimized root may be waiting for a repaint. Publish
                    // the result first so the awakened GUI can consume it.
                    if let Some(ctx) = job.repaint_context {
                        ctx.request_repaint_of(egui::ViewportId::ROOT);
                    }
                }
                // Provider destruction and lifecycle joins stay on this owner,
                // before the runtime leases are released.
                drop(provider);
                drop(_guards);
            })?;
        drop(worker);
        Ok(Self {
            jobs,
            replies,
            pending: None,
            ticket: 0,
            revision: 0,
            last_kind: None,
            last_origin: None,
            retiring: false,
            shutdown_complete: false,
            degraded: false,
            outcome: None,
            error: None,
            delay,
            repaint_context: None,
        })
    }
    pub fn attach_repaint_context(&mut self, ctx: &egui::Context) {
        self.repaint_context = Some(ctx.clone());
    }
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn retiring(&self) -> bool {
        self.retiring
    }
    pub fn shutdown_complete(&self) -> bool {
        self.shutdown_complete
    }
    pub fn degraded(&self) -> bool {
        self.degraded
    }
    pub fn status(&self) -> Status {
        Status {
            ticket: self.ticket,
            revision: self.revision,
            kind: self.last_kind,
            origin: self.last_origin,
            pending: self.pending(),
            retiring: self.retiring,
            shutdown_complete: self.shutdown_complete,
            degraded: self.degraded,
            result: self.outcome,
            error: self.error.clone(),
            artificial_delay_ms: self.delay.as_millis() as u64,
        }
    }
    pub fn submit(
        &mut self,
        kind: Kind,
        candidate: Option<Preferences>,
        origin: Origin,
        decision: Arc<CommitDecision>,
    ) -> Result<u64> {
        ensure!(
            !self.pending(),
            "An application preference operation is already pending."
        );
        ensure!(!self.retiring, "Application services are retiring.");
        ensure!(
            !self.degraded || kind != Kind::Configure || origin.local(),
            "Apply application preferences locally to recover the uncertain state."
        );
        let ticket = self
            .ticket
            .checked_add(1)
            .context("preferences ticket exhausted")?;
        self.jobs
            .try_send(Job {
                ticket,
                revision: self.revision,
                recover: self.degraded && origin.local(),
                kind,
                origin: origin.clone(),
                candidate,
                decision,
                repaint_context: self.repaint_context.clone(),
            })
            .map_err(|_| anyhow::anyhow!("Application preferences worker unavailable."))?;
        self.ticket = ticket;
        self.last_kind = Some(kind);
        self.last_origin = Some(match origin {
            Origin::Local => "local",
            Origin::Connected { .. } => "connected",
            Origin::Setup { .. } => "setup",
        });
        self.outcome = None;
        self.error = None;
        self.pending = Some(Pending {
            ticket,
            kind,
            origin,
        });
        if kind == Kind::Shutdown {
            self.retiring = true;
        }
        Ok(ticket)
    }
    pub fn poll(&mut self) -> Option<Completion> {
        let pending = self.pending.as_ref()?;
        let completion = match self.replies.try_recv() {
            Ok(completion) => completion,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Completion {
                ticket: pending.ticket,
                revision: self.revision,
                kind: pending.kind,
                origin: pending.origin.clone(),
                committed: None,
                outcome: Outcome::Unconfirmed,
                error: Some(
                    "Application preferences worker stopped; final state is unconfirmed.".into(),
                ),
            },
        };
        let pending = self.pending.take().expect("pending request");
        let completion = if completion.ticket != pending.ticket
            || completion.kind != pending.kind
            || completion.origin != pending.origin
        {
            Completion{ticket:pending.ticket,revision:self.revision,kind:pending.kind,origin:pending.origin,committed:None,outcome:Outcome::Unconfirmed,error:Some("Application preference response identity mismatch; final state is unconfirmed.".into())}
        } else {
            completion
        };
        self.revision = completion.revision;
        self.outcome = Some(completion.outcome);
        self.error = completion.error.clone();
        self.degraded = if completion.outcome == Outcome::Committed {
            false
        } else {
            self.degraded || completion.outcome == Outcome::Unconfirmed
        };
        self.shutdown_complete = completion.outcome == Outcome::ShutdownComplete;
        Some(completion)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_lifecycle::OperationFailure;
    use std::{collections::BTreeSet, sync::Mutex, time::Instant};
    #[derive(Clone)]
    struct Model {
        disk: Option<Vec<u8>>,
        registration: Option<StartupValue>,
        active: Preferences,
        calls: Vec<&'static str>,
        failures: BTreeSet<&'static str>,
    }
    struct Fake {
        state: Arc<Mutex<Model>>,
        gate: Option<(&'static str, mpsc::Sender<()>, Receiver<()>)>,
    }
    impl Fake {
        fn call(&mut self, name: &'static str) -> Result<()> {
            self.state.lock().unwrap().calls.push(name);
            if let Some((at, entered, release)) = &self.gate
                && *at == name
            {
                entered.send(()).unwrap();
                release.recv().unwrap();
            }
            ensure!(
                !self.state.lock().unwrap().failures.contains(name),
                "owned {name} failure"
            );
            Ok(())
        }
    }
    impl Provider for Fake {
        type Stage = Preferences;
        fn original(&mut self) -> Result<Option<Vec<u8>>> {
            self.call("original")?;
            Ok(self.state.lock().unwrap().disk.clone())
        }
        fn save(&mut self, candidate: &Preferences) -> Result<()> {
            self.call("save")?;
            self.state.lock().unwrap().disk = Some(app_preferences::encode(candidate)?);
            Ok(())
        }
        fn restore(&mut self, original: Option<&[u8]>) -> Result<()> {
            self.call("restore")?;
            self.state.lock().unwrap().disk = original.map(<[u8]>::to_vec);
            Ok(())
        }
        fn startup_snapshot(&mut self) -> Result<Option<StartupValue>> {
            self.call("snapshot")?;
            Ok(self.state.lock().unwrap().registration.clone())
        }
        fn startup_value(&mut self) -> Result<StartupValue> {
            self.call("value")?;
            Ok(StartupValue {
                kind: 1,
                data: vec![1, 2, 0],
            })
        }
        fn startup_write(&mut self, value: Option<&StartupValue>) -> Result<()> {
            self.call(if value.is_some_and(|value| value.kind == 99) {
                "registry_restore"
            } else {
                "registry"
            })?;
            self.state.lock().unwrap().registration = value.cloned();
            Ok(())
        }
        fn prepare(
            &mut self,
            candidate: &Preferences,
        ) -> std::result::Result<Self::Stage, OperationFailure> {
            self.call("prepare").map_err(|error| OperationFailure {
                error,
                uncertain: false,
            })?;
            Ok(candidate.clone())
        }
        fn rollback(&mut self, _: Self::Stage) -> Result<()> {
            self.call("rollback")
        }
        fn commit(
            &mut self,
            candidate: Self::Stage,
            decision: Arc<CommitDecision>,
        ) -> std::result::Result<(), CommitFailure> {
            if let Err(error) = self.call("before_claim") {
                let rollback = self.rollback(candidate);
                return Err(CommitFailure {
                    uncertain: rollback.is_err(),
                    rollback_files: true,
                    error: match rollback {
                        Ok(()) => error,
                        Err(rollback) => {
                            error.context(format!("shortcut rollback failed: {rollback:#}"))
                        }
                    },
                });
            }
            if !decision.claim() {
                self.rollback(candidate).map_err(|error| CommitFailure {
                    error,
                    uncertain: true,
                    rollback_files: true,
                })?;
                return Err(CommitFailure {
                    error: anyhow::anyhow!("cancelled before native claim"),
                    uncertain: false,
                    rollback_files: true,
                });
            }
            self.call("claimed").map_err(|error| CommitFailure {
                error,
                uncertain: true,
                rollback_files: false,
            })?;
            if self.state.lock().unwrap().failures.contains("panic") {
                panic!("owned provider panic");
            }
            self.call("commit").map_err(|error| CommitFailure {
                error,
                uncertain: false,
                rollback_files: true,
            })?;
            self.state.lock().unwrap().active = candidate;
            Ok(())
        }
        fn retry(&mut self, kind: Kind) -> std::result::Result<(), OperationFailure> {
            self.call(if kind == Kind::RetryShortcuts {
                "retry_shortcuts"
            } else {
                "retry_emergency"
            })
            .map_err(|error| OperationFailure {
                error,
                uncertain: false,
            })
        }
        fn shutdown(&mut self) -> Result<()> {
            self.call("shutdown")
        }
    }
    fn fixture(
        at: Option<&'static str>,
    ) -> (Fake, Arc<Mutex<Model>>, Receiver<()>, mpsc::Sender<()>) {
        let initial = Preferences::default();
        let state = Arc::new(Mutex::new(Model {
            disk: Some(format!(" \n{}\n", serde_json::to_string(&initial).unwrap()).into_bytes()),
            registration: Some(StartupValue {
                kind: 99,
                data: vec![9, 0, 8, 0],
            }),
            active: initial,
            calls: Vec::new(),
            failures: BTreeSet::new(),
        }));
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        (
            Fake {
                state: state.clone(),
                gate: at.map(|at| (at, entered_tx, release_rx)),
            },
            state,
            entered,
            release,
        )
    }
    fn candidate() -> Preferences {
        Preferences {
            launch_at_login: true,
            close_to_tray: false,
            zoom_percent: 110,
            ..Default::default()
        }
    }
    fn connected(sequence: u64) -> Origin {
        Origin::Connected {
            client: 7,
            sequence,
            authority: (11, 12, 13),
        }
    }
    fn wait(service: &mut Service) -> Completion {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(done) = service.poll() {
                return done;
            }
            assert!(Instant::now() < deadline, "owned worker made no progress");
            std::thread::yield_now();
        }
    }
    fn started(entered: &Receiver<()>) {
        entered.recv_timeout(Duration::from_secs(3)).unwrap();
    }
    fn setup_policy_transaction(at: &'static str, revoke: bool) {
        use crate::gui_setup::{PreparedCommand, channel};
        let (provider, state, entered, release) = fixture(Some(at));
        let initial = Preferences {
            appearance_mode: app_preferences::AppearanceMode::Light,
            zoom_percent: 125,
            show_app_launcher: false,
            launch_minimized: true,
            minimize_to_tray: true,
            close_to_tray: true,
            launch_at_login: true,
            ..Default::default()
        };
        initial.validate().unwrap();
        {
            let mut model = state.lock().unwrap();
            model.active = initial.clone();
            model.disk =
                Some(format!(" \n{}\n", serde_json::to_string(&initial).unwrap()).into_bytes());
        }
        let original = state.lock().unwrap().clone();
        let policy = app_preferences::ProfilePersistence {
            load_profile_at_startup: false,
            save_profile_on_exit: true,
        };
        let desired = initial.with_profile_persistence(policy);
        let (handle, requests) = channel();
        let epoch = handle.enable();
        handle.admit(17).unwrap();
        let pending_reply = handle
            .submit(
                17,
                21,
                epoch,
                PreparedCommand::ConfigurePersistence {
                    expected_revision: 4,
                    policy,
                },
            )
            .unwrap();
        let request = requests.try_recv().unwrap();
        let origin = Origin::Setup {
            client: request.client,
            sequence: request.sequence,
            epoch: request.epoch,
        };
        assert!(!origin.local());
        let mut service = Service::start_with(provider, initial.clone(), Duration::ZERO).unwrap();
        let ticket = service
            .submit(
                Kind::Configure,
                Some(desired.clone()),
                origin.clone(),
                request.decision.clone(),
            )
            .unwrap();
        started(&entered);
        assert_eq!(service.status().origin, Some("setup"));
        assert!(service.poll().is_none());
        // The transaction has staged bytes by both gates; cancellation must
        // compensate the exact original formatting, not merely equivalent JSON.
        assert_ne!(state.lock().unwrap().disk, original.disk);
        if revoke {
            handle.revoke();
            assert!(!handle.permits(request.client, request.epoch));
            assert_eq!(request.decision.cancelled(), at == "before_claim");
            // Revoke has already cancelled an unclaimed decision; a second
            // cancellation also cannot steal an existing commit claim.
            assert!(!request.decision.cancel());
        }
        release.send(()).unwrap();
        let done = wait(&mut service);
        let cancelled = revoke && at == "before_claim";
        assert_eq!(done.ticket, ticket);
        assert_eq!(done.origin, origin);
        assert_eq!(service.status().origin, Some("setup"));
        assert_eq!(
            done.outcome,
            if cancelled {
                Outcome::Cancelled
            } else {
                Outcome::Committed
            }
        );
        assert!(!service.pending());
        assert!(!service.degraded());
        let model = state.lock().unwrap();
        assert_eq!(model.registration, original.registration);
        for registry_call in ["snapshot", "value", "registry", "registry_restore"] {
            assert!(!model.calls.contains(&registry_call));
        }
        if cancelled {
            assert!(done.committed.is_none());
            assert_eq!(model.active, initial);
            assert_eq!(model.disk, original.disk);
            assert!(model.calls.contains(&"rollback"));
            assert!(model.calls.contains(&"restore"));
            assert!(!model.calls.contains(&"commit"));
        } else {
            assert_eq!(done.committed, Some(desired.clone()));
            assert_eq!(model.active, desired);
            assert_eq!(
                app_preferences::decode(model.disk.as_ref().unwrap()).unwrap(),
                desired
            );
            let mut unchanged = model.active.clone();
            unchanged.load_profile_at_startup = initial.load_profile_at_startup;
            unchanged.save_profile_on_exit = initial.save_profile_on_exit;
            assert_eq!(unchanged, initial);
            assert!(!model.calls.contains(&"restore"));
            assert!(!model.calls.contains(&"rollback"));
        }
        drop(model);
        // Abandoning the response after a claim cannot revoke the acknowledged
        // global commit; before claim it cannot resurrect the compensated work.
        drop(pending_reply);
        assert_eq!(request.decision.cancelled(), cancelled);
        assert_eq!(request.decision.claimed(), !cancelled);
    }
    #[test]
    fn setup_policy_commits_through_serial_owner_without_changing_other_preferences_or_registration()
     {
        setup_policy_transaction("before_claim", false);
    }
    #[test]
    fn setup_policy_revoke_before_claim_restores_exact_bytes_without_registry_side_effects() {
        setup_policy_transaction("before_claim", true);
    }
    #[test]
    fn setup_policy_revoke_after_claim_keeps_global_actual_and_setup_origin() {
        setup_policy_transaction("claimed", true);
    }
    #[test]
    fn blocked_whole_transaction_keeps_captured_candidate_and_one_outstanding_owner() {
        for phase in [
            "original",
            "snapshot",
            "prepare",
            "registry",
            "save",
            "before_claim",
            "claimed",
        ] {
            let (provider, state, entered, release) = fixture(Some(phase));
            let mut service =
                Service::start_with(provider, Preferences::default(), Duration::ZERO).unwrap();
            let mut draft = candidate();
            let submitted = draft.clone();
            let ticket = service
                .submit(
                    Kind::Configure,
                    Some(draft.clone()),
                    Origin::Local,
                    Arc::default(),
                )
                .unwrap();
            started(&entered);
            draft.zoom_percent = 120;
            assert!(service.poll().is_none());
            assert!(service.pending());
            assert!(
                service
                    .submit(Kind::Configure, Some(draft), Origin::Local, Arc::default())
                    .is_err()
            );
            release.send(()).unwrap();
            let done = wait(&mut service);
            assert_eq!(done.ticket, ticket);
            assert_eq!(done.committed, Some(submitted.clone()));
            assert_eq!(done.outcome, Outcome::Committed);
            let actual = state.lock().unwrap();
            assert_eq!(actual.active, submitted);
            assert_eq!(
                app_preferences::decode(actual.disk.as_ref().unwrap()).unwrap(),
                submitted
            );
        }
    }
    #[test]
    fn quit_revoke_before_claim_compensates_exact_bytes_then_allows_normal_shutdown() {
        let (provider, state, entered, release) = fixture(Some("before_claim"));
        let original = state.lock().unwrap().disk.clone();
        let mut service =
            Service::start_with(provider, Preferences::default(), Duration::ZERO).unwrap();
        let gate = Arc::new(CommitDecision::default());
        let mut desired = candidate();
        desired.launch_at_login = false;
        let origin = connected(81);
        service
            .submit(Kind::Configure, Some(desired), origin.clone(), gate.clone())
            .unwrap();
        started(&entered);
        // Same decision that GUI Quit/revoke/disconnect cancels, not an alive check.
        assert!(gate.cancel());
        assert!(service.poll().is_none());
        release.send(()).unwrap();
        let done = wait(&mut service);
        assert_eq!(done.origin, origin);
        assert_eq!(done.outcome, Outcome::Cancelled);
        assert!(done.permits_waiting_quit());
        assert!(done.committed.is_none());
        assert!(!service.degraded());
        assert_eq!(state.lock().unwrap().disk, original);
        assert_eq!(state.lock().unwrap().active, Preferences::default());
        service
            .submit(Kind::Shutdown, None, Origin::Local, Arc::default())
            .unwrap();
        let done = wait(&mut service);
        assert_eq!(done.outcome, Outcome::ShutdownComplete);
        assert!(service.shutdown_complete());
        assert_eq!(
            state
                .lock()
                .unwrap()
                .calls
                .iter()
                .filter(|&&call| call == "shutdown")
                .count(),
            1
        );
    }
    #[test]
    fn disconnect_after_actual_claim_cannot_discard_global_commit_or_mix_response_origin() {
        let (provider, state, entered, release) = fixture(Some("claimed"));
        let mut service =
            Service::start_with(provider, Preferences::default(), Duration::ZERO).unwrap();
        let gate = Arc::new(CommitDecision::default());
        let mut desired = candidate();
        desired.launch_at_login = false;
        let origin = connected(93);
        service
            .submit(
                Kind::Configure,
                Some(desired.clone()),
                origin.clone(),
                gate.clone(),
            )
            .unwrap();
        started(&entered);
        assert!(!gate.cancel());
        release.send(()).unwrap();
        let done = wait(&mut service);
        assert_eq!(done.origin, origin);
        assert_eq!(done.outcome, Outcome::Committed);
        assert_eq!(done.committed, Some(desired.clone()));
        assert_eq!(state.lock().unwrap().active, desired);
    }
    #[test]
    fn failures_attempt_all_compensations_and_do_not_turn_rollback_loss_into_cancellation() {
        for broken in [false, true] {
            let (provider, state, _, _) = fixture(None);
            let original = state.lock().unwrap().clone();
            state.lock().unwrap().failures.insert("before_claim");
            if broken {
                state
                    .lock()
                    .unwrap()
                    .failures
                    .extend(["restore", "registry_restore", "rollback"]);
            }
            let mut service =
                Service::start_with(provider, Preferences::default(), Duration::ZERO).unwrap();
            service
                .submit(
                    Kind::Configure,
                    Some(candidate()),
                    Origin::Local,
                    Arc::default(),
                )
                .unwrap();
            let done = wait(&mut service);
            assert_eq!(
                done.outcome,
                if broken {
                    Outcome::Unconfirmed
                } else {
                    Outcome::Failed
                }
            );
            assert!(!done.permits_waiting_quit());
            let state = state.lock().unwrap();
            for call in ["restore", "registry_restore", "rollback"] {
                assert!(state.calls.contains(&call));
            }
            if broken {
                let error = done.error.unwrap();
                for what in ["file rollback", "startup rollback", "shortcut rollback"] {
                    assert!(error.contains(what));
                }
            } else {
                assert_eq!(state.disk, original.disk);
                assert_eq!(state.registration, original.registration);
            }
        }
    }
    #[test]
    fn atomic_save_rejection_preserves_newly_damaged_destination_instead_of_restoring_old_bytes() {
        let (provider, state, entered, release) = fixture(Some("prepare"));
        let original_registry = state.lock().unwrap().registration.clone();
        let mut service =
            Service::start_with(provider, Preferences::default(), Duration::ZERO).unwrap();
        service
            .submit(
                Kind::Configure,
                Some(candidate()),
                Origin::Local,
                Arc::default(),
            )
            .unwrap();
        started(&entered);
        // Another writer damaged the destination after original() but before
        // save() validation. Its refused bytes must remain for explicit recovery.
        state.lock().unwrap().disk = Some(b"invalid owned document".to_vec());
        state.lock().unwrap().failures.insert("save");
        release.send(()).unwrap();
        let done = wait(&mut service);
        assert_eq!(done.outcome, Outcome::Failed);
        let state = state.lock().unwrap();
        assert_eq!(state.disk, Some(b"invalid owned document".to_vec()));
        assert!(!state.calls.contains(&"restore"));
        assert!(state.calls.contains(&"rollback"));
        assert_eq!(state.registration, original_registry);
    }

    #[test]
    fn ordinary_prepare_conflict_and_retry_failure_allow_explicit_corrected_remote_apply() {
        let (provider, state, _, _) = fixture(None);
        state.lock().unwrap().failures.insert("prepare");
        let mut service =
            Service::start_with(provider, Preferences::default(), Duration::ZERO).unwrap();
        let mut desired = candidate();
        desired.launch_at_login = false;
        service
            .submit(
                Kind::Configure,
                Some(desired.clone()),
                connected(1),
                Arc::default(),
            )
            .unwrap();
        assert_eq!(wait(&mut service).outcome, Outcome::Failed);
        assert!(!service.degraded());
        state.lock().unwrap().failures.clear();
        state.lock().unwrap().failures.insert("retry_shortcuts");
        service
            .submit(Kind::RetryShortcuts, None, Origin::Local, Arc::default())
            .unwrap();
        assert_eq!(wait(&mut service).outcome, Outcome::Failed);
        assert!(!service.degraded());
        state.lock().unwrap().failures.clear();
        service
            .submit(Kind::Configure, Some(desired), connected(2), Arc::default())
            .unwrap();
        assert_eq!(wait(&mut service).outcome, Outcome::Committed);
    }
    #[test]
    fn lost_claimed_ack_and_panic_are_unconfirmed_not_blind_file_rollback_or_success() {
        for failure in ["claimed", "panic"] {
            let (provider, state, _, _) = fixture(None);
            state.lock().unwrap().failures.insert(failure);
            let mut service =
                Service::start_with(provider, Preferences::default(), Duration::ZERO).unwrap();
            service
                .submit(
                    Kind::Configure,
                    Some(candidate()),
                    Origin::Local,
                    Arc::default(),
                )
                .unwrap();
            let done = wait(&mut service);
            assert_eq!(done.outcome, Outcome::Unconfirmed);
            assert!(done.committed.is_none());
            assert!(service.degraded());
            assert!(!state.lock().unwrap().calls.contains(&"restore"));
            let mut remote = candidate();
            remote.launch_at_login = false;
            assert!(
                service
                    .submit(Kind::Configure, Some(remote), connected(9), Arc::default())
                    .is_err()
            );
        }
    }
    #[test]
    fn retry_and_shutdown_are_nonblocking_owned_once_and_drop_never_joins_blocked_provider() {
        for (kind, phase) in [
            (Kind::RetryShortcuts, "retry_shortcuts"),
            (Kind::RetryEmergency, "retry_emergency"),
            (Kind::Shutdown, "shutdown"),
        ] {
            let (provider, state, entered, release) = fixture(Some(phase));
            let mut service =
                Service::start_with(provider, Preferences::default(), Duration::ZERO).unwrap();
            service
                .submit(kind, None, Origin::Local, Arc::default())
                .unwrap();
            started(&entered);
            assert!(service.poll().is_none());
            assert!(
                service
                    .submit(kind, None, Origin::Local, Arc::default())
                    .is_err()
            );
            let (dropped_tx, dropped_rx) = mpsc::channel();
            let owner = std::thread::spawn(move || {
                drop(service);
                dropped_tx.send(()).unwrap();
            });
            dropped_rx
                .recv_timeout(Duration::from_secs(3))
                .expect("GUI owner joined worker");
            owner.join().unwrap();
            release.send(()).unwrap();
            assert_eq!(
                state
                    .lock()
                    .unwrap()
                    .calls
                    .iter()
                    .filter(|&&call| call == phase)
                    .count(),
                1
            );
        }
    }
    #[test]
    fn shutdown_wakes_the_root_after_the_result_is_available() {
        for fail in [false, true] {
            let (provider, state, entered, release) = fixture(Some("shutdown"));
            if fail {
                state.lock().unwrap().failures.insert("shutdown");
            }
            let mut service =
                Service::start_with(provider, Preferences::default(), Duration::ZERO).unwrap();
            let ctx = egui::Context::default();
            let (wakeup, notified) = mpsc::channel();
            ctx.set_request_repaint_callback(move |request| {
                let _ = wakeup.send(request);
            });
            service.attach_repaint_context(&ctx);
            let ticket = service
                .submit(Kind::Shutdown, None, Origin::Local, Arc::default())
                .unwrap();
            started(&entered);
            assert!(matches!(notified.try_recv(), Err(TryRecvError::Empty)));
            assert!(service.poll().is_none());

            release.send(()).unwrap();
            let wakeup = notified.recv_timeout(Duration::from_secs(3)).unwrap();
            assert_eq!(wakeup.viewport_id, egui::ViewportId::ROOT);
            assert_eq!(wakeup.delay, Duration::ZERO);
            let done = service.poll().expect("result must precede the wakeup");
            assert_eq!(done.ticket, ticket);
            assert_eq!(
                done.outcome,
                if fail {
                    Outcome::Unconfirmed
                } else {
                    Outcome::ShutdownComplete
                }
            );
            assert_eq!(service.shutdown_complete(), !fail);
        }
    }

    #[test]
    fn failed_shutdown_is_retained_and_never_replayed_as_clean_success() {
        let (provider, state, _, _) = fixture(None);
        state.lock().unwrap().failures.insert("shutdown");
        let mut service =
            Service::start_with(provider, Preferences::default(), Duration::ZERO).unwrap();
        service
            .submit(Kind::Shutdown, None, Origin::Local, Arc::default())
            .unwrap();
        let done = wait(&mut service);
        assert_eq!(done.outcome, Outcome::Unconfirmed);
        assert!(service.retiring());
        assert!(!service.shutdown_complete());
        assert!(
            service
                .submit(Kind::Shutdown, None, Origin::Local, Arc::default())
                .is_err()
        );
        assert_eq!(
            state
                .lock()
                .unwrap()
                .calls
                .iter()
                .filter(|&&call| call == "shutdown")
                .count(),
            1
        );
    }
    #[test]
    fn explicit_local_recovery_and_shutdown_survive_an_uncertain_view_revision() {
        let (provider, _, _, _) = fixture(None);
        let mut service =
            Service::start_with(provider, Preferences::default(), Duration::ZERO).unwrap();
        service
            .submit(
                Kind::Configure,
                Some(candidate()),
                Origin::Local,
                Arc::default(),
            )
            .unwrap();
        assert_eq!(wait(&mut service).outcome, Outcome::Committed);
        // Simulate a private completion that could not be safely projected into
        // the GUI. Explicit local Apply reconciles all state; cleanup is never
        // prevented by a stale view revision.
        service.revision = 0;
        service.degraded = true;
        let next = Preferences {
            zoom_percent: 120,
            ..candidate()
        };
        service
            .submit(
                Kind::Configure,
                Some(next.clone()),
                Origin::Local,
                Arc::default(),
            )
            .unwrap();
        let done = wait(&mut service);
        assert_eq!(done.committed, Some(next));
        assert_eq!(done.revision, 2);
        assert!(!service.degraded());
        service.revision = 0;
        service
            .submit(Kind::Shutdown, None, Origin::Local, Arc::default())
            .unwrap();
        assert_eq!(wait(&mut service).outcome, Outcome::ShutdownComplete);
    }
    #[test]
    fn mismatched_or_disconnected_private_reply_is_unconfirmed_and_never_adopted() {
        for wrong_identity in [false, true] {
            let (jobs, _) = mpsc::sync_channel(1);
            let (reply, replies) = mpsc::sync_channel(1);
            let original = connected(7);
            let mut service = Service {
                jobs,
                replies,
                pending: Some(Pending {
                    ticket: 1,
                    kind: Kind::Configure,
                    origin: original.clone(),
                }),
                ticket: 1,
                revision: 0,
                last_kind: Some(Kind::Configure),
                last_origin: Some("connected"),
                retiring: false,
                shutdown_complete: false,
                degraded: false,
                outcome: None,
                error: None,
                delay: Duration::ZERO,
                repaint_context: None,
            };
            if wrong_identity {
                reply
                    .send(Completion {
                        ticket: 2,
                        revision: 1,
                        kind: Kind::Configure,
                        origin: connected(99),
                        committed: Some(candidate()),
                        outcome: Outcome::Committed,
                        error: None,
                    })
                    .unwrap();
            } else {
                drop(reply);
            }
            let done = service.poll().unwrap();
            assert_eq!(done.origin, original);
            assert_eq!(done.outcome, Outcome::Unconfirmed);
            assert!(done.committed.is_none());
            assert!(service.degraded());
            assert!(!service.shutdown_complete());
            assert!(!service.pending());
            assert!(
                service
                    .submit(
                        Kind::Configure,
                        Some(candidate()),
                        connected(8),
                        Arc::default()
                    )
                    .is_err()
            );
        }
    }

    #[test]
    fn candidate_validation_and_login_permissions_are_unchanged_before_any_side_effect() {
        let (provider, state, _, _) = fixture(None);
        let mut service =
            Service::start_with(provider, Preferences::default(), Duration::ZERO).unwrap();
        service
            .submit(
                Kind::Configure,
                Some(candidate()),
                connected(1),
                Arc::default(),
            )
            .unwrap();
        assert_eq!(wait(&mut service).outcome, Outcome::Failed);
        assert!(state.lock().unwrap().calls.is_empty());
        let bad = Preferences {
            zoom_percent: 51,
            ..Default::default()
        };
        service
            .submit(Kind::Configure, Some(bad), Origin::Local, Arc::default())
            .unwrap();
        assert_eq!(wait(&mut service).outcome, Outcome::Failed);
        assert!(state.lock().unwrap().calls.is_empty());
    }
}
