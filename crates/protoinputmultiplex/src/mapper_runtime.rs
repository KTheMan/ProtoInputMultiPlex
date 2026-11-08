//! Shared worker contract for the typed Input Mapper. Delivery stays supervised.
use crate::routing_engine::ProgramOrigin;
use anyhow::{Context, Result, bail, ensure};
use mux_core::mapper::{
    self, Compiled, Definition, Disposition, Emission, NUMERIC_LIMIT, OutputChannel, Source, Value,
};
use mux_core::{ResolvedSeat, automation::Action, emulated, routing::Window};
use mux_windows::targets::ProcessWatch;
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

/// Configuration changes are checked against the acknowledged worker revision.
/// Applying never arms a graph; Arm is a separate explicit command.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Inspect,
    Apply {
        definition: Definition,
        expected_revision: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gui: Option<u64>,
    },
    Arm {
        expected_revision: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gui: Option<u64>,
    },
    Disarm {
        expected_revision: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gui: Option<u64>,
    },
}

/// No actor values, input payload history, credentials or native handles are
/// exposed in telemetry. Counters describe accepted graph work, not proof of
/// application consumption of each native message.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub acknowledged: bool,
    pub definition: Definition,
    pub revision: u64,
    /// Monotonic state ordering, independent of the definition and run counters.
    pub sequence: u64,
    /// Internal GUI reply identity; never an authorization credential. Ordinary
    /// inspections and externally requested commands leave this absent.
    pub reply_to: Option<u64>,
    pub run: u64,
    pub armed: bool,
    pub cleanup_pending: bool,
    pub processed_inputs: u64,
    pub emitted_outputs: u64,
    pub rejected_inputs: u64,
    pub actor_contexts: usize,
    pub pending_timers: usize,
    pub timer_pulses: u64,
    pub pending_delays: usize,
    pub delayed_events: u64,
    pub pending_observations: usize,
    pub observation_completions: u64,
    pub dropped_observations: u64,
    pub capture_worker_busy: bool,
    pub error: Option<String>,
}

/// Samples from exactly one actor. This is separate from shared Mapper telemetry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorSnapshot {
    pub revision: u64,
    pub run: u64,
    pub samples: Vec<mapper::MonitorSample>,
    #[serde(default)]
    pub traces: Vec<mapper::ScopeTrace>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorReceipt {
    pub request_id: u64,
    pub snapshot: MonitorSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Actor {
    Manual,
    Agent(u64),
}
impl Actor {
    fn from_origin(origin: ProgramOrigin) -> Option<Self> {
        match origin {
            ProgramOrigin::Manual(_) => Some(Self::Manual),
            ProgramOrigin::Agent(client, _) => Some(Self::Agent(client)),
            _ => None,
        }
    }
}
/// A logical mapper namespace. Native token/recipient receipts belong to the
/// backend; this saved identity survives a gate closing from another source.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SinkOwner {
    pub origin: ProgramOrigin,
    pub run: u64,
    pub sink: usize,
    pub user: usize,
    pub source_users: Vec<usize>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutputScope {
    pub sink: usize,
    pub user: usize,
    pub channel: OutputChannel,
    pub source_users: Vec<usize>,
}
#[derive(Debug, Clone)]
pub(crate) struct PlannedEffect {
    pub owner: SinkOwner,
    pub effect: Quantized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CaptureAuthority {
    pub window: Window,
    pub generation: u64,
}

/// All callbacks run synchronously on the existing supervised worker. Planning
/// callbacks must not emit input. `original` owns the raw disposition ledger:
/// its saved DOWN policy overrides the current policy for repeats and UPs,
/// including while this coordinator is disarmed. `deliver` retains successful
/// DOWN receipts before another fallible effect, including pulse UP failure.
pub(crate) trait Backend {
    fn validate_definition(&mut self, sources: &[Source], outputs: &[OutputScope]) -> Result<()>;
    fn validate_arm(&mut self, sources: &[Source], users: &[usize]) -> Result<()>;
    fn authorize(
        &mut self,
        origin: ProgramOrigin,
        source_user: usize,
        outputs: &[OutputScope],
    ) -> Result<()>;
    fn capture_authority(
        &mut self,
        _origin: ProgramOrigin,
        _source_user: usize,
        _capture_user: usize,
        _source_users: &[usize],
    ) -> Result<CaptureAuthority> {
        bail!("Mapper selected-window capture is unavailable")
    }
    fn pointer(&mut self, user: usize) -> Result<(i32, i32)>;
    /// Pure recipient liveness check; a missing required recipient retires the
    /// generation before later source activity can reuse its latched state.
    fn health(&mut self, _outputs: &[OutputScope]) -> Result<()> {
        Ok(())
    }
    fn reserve(
        &mut self,
        original: Option<(ProgramOrigin, &Action)>,
        effects: &[PlannedEffect],
    ) -> Result<()>;
    fn timer_windows(&mut self, outputs: &[OutputScope]) -> Result<Vec<Window>>;
    fn watch_timer_targets(&self, windows: &[Window]) -> Result<Option<Rc<ProcessWatch>>>;
    /// Default production policy checks the retained process object. A fake
    /// backend can model an exited object without launching a native target.
    fn check_scheduled_lifetime(&self, watch: Option<&ProcessWatch>) -> Result<()> {
        ensure!(
            watch.is_none_or(|watch| watch.exited().is_none()),
            "scheduled target process exited"
        );
        Ok(())
    }
    fn original(
        &mut self,
        origin: ProgramOrigin,
        action: &Action,
        disposition: Disposition,
    ) -> Result<()>;
    fn check_timer_effect(
        &self,
        owner: &SinkOwner,
        effect: &Quantized,
        expected: Window,
    ) -> Result<()>;
    fn deliver(
        &mut self,
        owner: &SinkOwner,
        effect: &Quantized,
        expected: Option<Window>,
    ) -> Result<()>;
    /// Release only this generation/actor's mapper claims, never raw consumed
    /// markers or ordinary/manual/other-actor claims. Failed receipts stay saved.
    fn release(&mut self, run: u64, actor: Option<Actor>) -> Result<()>;
    fn pending(&self, run: u64, actor: Option<Actor>) -> usize;
}

#[derive(Clone)]
struct ContextState {
    signals: mapper::State,
    quantizer: Quantizer,
    pointers: BTreeMap<usize, [f64; 2]>,
    owners: BTreeMap<usize, SinkOwner>,
    keys: BTreeSet<(usize, u16, i32, u32)>,
    timers: BTreeMap<usize, TimerState>,
    delays: BTreeMap<u64, DelayedRun>,
    observations: BTreeMap<u64, ObservationRun>,
}
#[derive(Clone)]
struct TimerRun {
    origin: ProgramOrigin,
    source_user: usize,
    windows: Vec<Window>,
    watch: Option<Rc<ProcessWatch>>,
    interval: Duration,
    due: Instant,
    remaining: u32,
}
#[derive(Clone)]
enum TimerState {
    Running(TimerRun),
    Exhausted,
}
/// Authority and recipient receipts for one core-owned typed delayed value.
/// The value itself stays private in mapper::State, never in worker telemetry.
#[derive(Clone)]
struct DelayedRun {
    node_index: usize,
    origin: ProgramOrigin,
    source_user: usize,
    scopes: Vec<OutputScope>,
    windows: Vec<Window>,
    watch: Option<Rc<ProcessWatch>>,
    due: Instant,
}
#[derive(Clone)]
struct ObservationRun {
    ticket: u64,
    node_index: usize,
    origin: ProgramOrigin,
    source_user: usize,
    capture_user: usize,
    source_users: Vec<usize>,
    authority: CaptureAuthority,
    scopes: Vec<OutputScope>,
    windows: Vec<Window>,
    watch: Option<Rc<ProcessWatch>>,
    accepted_at: Instant,
}
struct ObservationTask {
    actor: Actor,
    id: u64,
    ticket: u64,
    run: u64,
    deadline: Instant,
    task: Box<dyn crate::mapper_screen::Task>,
    ready: Option<Result<mapper::ObservationResult>>,
}

#[derive(Default)]
struct ScheduleChanges {
    timers: Vec<usize>,
    delays: Vec<u64>,
    observations: Vec<u64>,
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DueWork {
    Timer(usize),
    Delay(u64),
}
const MAX_RAW_KEYS_PER_ACTOR: usize = 4096;
const MAX_PENDING_TIMERS: usize = 64;
struct Prepared {
    disposition: Disposition,
    context: Option<ContextState>,
    effects: Vec<PlannedEffect>,
    schedules: ScheduleChanges,
}
pub(crate) struct Runtime {
    state: State,
    compiled: Compiled,
    identities: Vec<String>,
    seats: Vec<ResolvedSeat>,
    active: Vec<bool>,
    actors: BTreeMap<Actor, ContextState>,
    global_cleanup: bool,
    actor_cleanup: BTreeSet<Actor>,
    last_cleanup: Option<Instant>,
    last_health: Option<Instant>,
    run_started: Instant,
    queries: BTreeMap<usize, Arc<crate::screen_analysis::PreparedQuery>>,
    observation_ticket: Cell<u64>,
    observation_task: Option<ObservationTask>,
    capture_provider: Box<dyn crate::mapper_screen::Provider>,
}
impl Runtime {
    pub(crate) fn new(identities: Vec<String>, seats: Vec<ResolvedSeat>) -> Result<Self> {
        ensure!(
            identities.len() == seats.len(),
            "Mapper user identities do not match seats"
        );
        ensure!(
            identities.iter().all(|id| !id.is_empty())
                && identities.iter().collect::<BTreeSet<_>>().len() == identities.len(),
            "Mapper user identities must be nonempty and unique"
        );
        let state = State::default();
        let compiled = state.definition.compile()?;
        Ok(Self {
            active: vec![true; seats.len()],
            state,
            compiled,
            identities,
            seats,
            actors: BTreeMap::new(),
            global_cleanup: false,
            actor_cleanup: BTreeSet::new(),
            last_cleanup: None,
            last_health: None,
            run_started: Instant::now(),
            queries: BTreeMap::new(),
            observation_ticket: Cell::new(0),
            observation_task: None,
            capture_provider: Box::new(crate::mapper_screen::NativeProvider),
        })
    }
    pub(crate) fn snapshot(&self) -> State {
        let mut state = self.state.clone();
        state.acknowledged = false;
        state.reply_to = None;
        state.actor_contexts = self.actors.len();
        state.capture_worker_busy = self.observation_task.is_some() || self.capture_provider.busy();
        state.pending_timers = self
            .actors
            .values()
            .flat_map(|actor| actor.timers.values())
            .filter(|timer| matches!(timer, TimerState::Running(_)))
            .count();
        state.pending_delays = self.actors.values().map(|actor| actor.delays.len()).sum();
        state.pending_observations = self
            .actors
            .values()
            .map(|actor| actor.observations.len())
            .sum();
        state
    }
    fn commit_context(&mut self, actor: Actor, context: ContextState) {
        let before = self
            .actors
            .get(&actor)
            .map_or(0, |old| old.signals.dropped_observations());
        let dropped = context
            .signals
            .dropped_observations()
            .saturating_sub(before);
        self.state.dropped_observations = self.state.dropped_observations.saturating_add(dropped);
        self.actors.insert(actor, context);
    }
    pub(crate) fn monitor_snapshot(&self, actor: Actor) -> MonitorSnapshot {
        let context = self.actors.get(&actor).filter(|_| self.state.armed);
        MonitorSnapshot {
            revision: self.state.revision,
            run: self.state.run,
            samples: context.map_or_else(Vec::new, |context| context.signals.monitor_samples()),
            traces: context.map_or_else(Vec::new, |context| context.signals.scope_traces()),
        }
    }

    fn run_elapsed_us(&self, now: Instant) -> u64 {
        u64::try_from(now.saturating_duration_since(self.run_started).as_micros())
            .unwrap_or(u64::MAX)
    }

    pub(crate) fn is_armed(&self) -> bool {
        self.state.armed
    }
    pub(crate) fn sequence(&self) -> u64 {
        self.state.sequence
    }
    pub(crate) fn prepare_user(&mut self, identity: &str) -> Result<()> {
        ensure!(
            !self.state.armed && !self.state.cleanup_pending,
            "Disarm and finish mapper cleanup before adding a user"
        );
        ensure!(
            !identity.is_empty() && !self.identities.iter().any(|id| id == identity),
            "Mapper user identity must be unique"
        );
        self.state
            .sequence
            .checked_add(1)
            .context("Mapper state sequence exhausted")?;
        self.identities.try_reserve(1)?;
        self.seats.try_reserve(1)?;
        self.active.try_reserve(1)?;
        Ok(())
    }
    /// Current configuration only. The backend's raw DOWN ledger remains
    /// authoritative for repeats/UPs from an earlier disposition or run.
    pub(crate) fn source_disposition(&self, action: &Action) -> Disposition {
        if !self.state.armed || action.user() >= self.seats.len() || !self.active[action.user()] {
            return Disposition::Passthrough;
        }
        self.action_source(action)
            .ok()
            .flatten()
            .and_then(|source| self.compiled.source_disposition(&source))
            .unwrap_or_default()
    }
    fn advance(&mut self) -> Result<()> {
        self.state.sequence = self
            .state
            .sequence
            .checked_add(1)
            .context("Mapper state sequence exhausted")?;
        Ok(())
    }
    pub(crate) fn command(&mut self, command: Command, backend: &mut dyn Backend) -> Result<State> {
        let gui = match &command {
            Command::Inspect => None,
            Command::Apply { gui, .. } | Command::Arm { gui, .. } | Command::Disarm { gui, .. } => {
                *gui
            }
        };
        if matches!(command, Command::Inspect) {
            let mut reply = self.snapshot();
            reply.acknowledged = true;
            return Ok(reply);
        }
        // Reserve ordering before any authority/native mutation. Domain errors
        // receive their own correlated acknowledgement without lending authority.
        let sequence = self
            .state
            .sequence
            .checked_add(1)
            .context("Mapper state sequence exhausted")?;
        let result = self.execute_command(command, backend);
        self.state.sequence = self.state.sequence.max(sequence);
        self.state.error = result.err().map(|error| format!("{error:#}"));
        let mut reply = self.snapshot();
        reply.acknowledged = true;
        reply.reply_to = gui;
        Ok(reply)
    }
    fn execute_command(&mut self, command: Command, backend: &mut dyn Backend) -> Result<()> {
        match command {
            Command::Inspect => Ok(()),
            Command::Apply {
                definition,
                expected_revision,
                ..
            } => {
                ensure!(
                    expected_revision == self.state.revision,
                    "Stale mapper revision; inspect before Apply"
                );
                ensure!(
                    self.observation_task.is_none() && !self.capture_provider.busy(),
                    "Wait for the previous Mapper capture worker to finish before Apply"
                );
                let compiled = definition.compile()?;
                let queries = crate::mapper_screen::prepare(&compiled)?;
                self.validate_users(&compiled)?;
                backend.validate_definition(&compiled.sources(), &self.all_scopes(&compiled)?)?;
                ensure!(!self.state.cleanup_pending, "Mapper cleanup is pending");
                let revision = self
                    .state
                    .revision
                    .checked_add(1)
                    .context("Mapper revision exhausted")?;
                self.retire(backend)?;
                self.compiled = compiled;
                self.queries = queries;
                self.state.definition = definition;
                self.state.revision = revision;
                Ok(())
            }
            Command::Arm {
                expected_revision, ..
            } => {
                ensure!(
                    expected_revision == self.state.revision,
                    "Stale mapper revision; inspect before Arm"
                );
                ensure!(!self.state.armed, "Mapper is already armed");
                ensure!(
                    self.observation_task.is_none() && !self.capture_provider.busy(),
                    "Wait for the previous Mapper capture worker to finish before Arm"
                );
                ensure!(!self.state.cleanup_pending, "Mapper cleanup is pending");
                ensure!(
                    !self.compiled.sources().is_empty(),
                    "Mapper has no input sources"
                );
                let users = self.validate_users(&self.compiled)?;
                backend.validate_definition(
                    &self.compiled.sources(),
                    &self.all_scopes(&self.compiled)?,
                )?;
                backend.validate_arm(&self.compiled.sources(), &users)?;
                let run = self
                    .state
                    .run
                    .checked_add(1)
                    .context("Mapper run counter exhausted")?;
                self.actors.clear();
                self.state.run = run;
                self.run_started = Instant::now();
                self.state.armed = true;
                Ok(())
            }
            Command::Disarm {
                expected_revision, ..
            } => {
                ensure!(
                    expected_revision == self.state.revision,
                    "Stale mapper revision; inspect before Disarm"
                );
                self.retire(backend)
            }
        }
    }
    fn user(&self, identity: &str) -> Result<usize> {
        let user = self
            .identities
            .iter()
            .position(|id| id == identity)
            .context("Mapper references an unknown user identity")?;
        ensure!(self.active[user], "Mapper references a retired user");
        Ok(user)
    }
    fn validate_users(&self, compiled: &Compiled) -> Result<Vec<usize>> {
        let mut users = BTreeSet::new();
        for source in compiled.sources() {
            let user = self.user(source.user())?;
            self.validate_device(user, matches!(source, Source::Key { .. }))?;
            users.insert(user);
        }
        for node in &compiled.definition().nodes {
            if let mapper::NodeKind::Sink { user, channel } = &node.kind {
                let user = self.user(user)?;
                self.validate_device(
                    user,
                    matches!(
                        channel,
                        OutputChannel::KeyHold { .. } | OutputChannel::KeyPulse { .. }
                    ),
                )?;
                users.insert(user);
            }
        }
        for observation in compiled.observations() {
            let user = self.user(&observation.capture_user)?;
            self.validate_device(user, false)?;
            users.insert(user);
        }
        Ok(users.into_iter().collect())
    }
    fn validate_device(&self, user: usize, keyboard: bool) -> Result<()> {
        let mouse = self.seats[user].mouse;
        ensure!(
            mouse & emulated::MASK == emulated::BASE && mouse & 1 == 0 && mouse & 0xFFFFFF >= 2,
            "Mapper requires virtual users"
        );
        if keyboard {
            let keyboard = self.seats[user]
                .keyboard
                .context("Mapper requires that user's virtual keyboard")?;
            ensure!(
                keyboard & emulated::MASK == emulated::BASE
                    && keyboard & 1 == 1
                    && keyboard & 0xFFFFFF >= 3,
                "Mapper requires virtual keyboards"
            );
        }
        Ok(())
    }
    fn scopes(&self, source: &Source) -> Result<Vec<OutputScope>> {
        self.scopes_for(&self.compiled, source)
    }
    fn scopes_for(&self, compiled: &Compiled, source: &Source) -> Result<Vec<OutputScope>> {
        self.output_scopes(compiled, compiled.affected_outputs(source))
    }
    fn output_scopes(
        &self,
        compiled: &Compiled,
        sinks: Vec<mapper::SinkDescriptor>,
    ) -> Result<Vec<OutputScope>> {
        sinks
            .into_iter()
            .map(|sink| {
                let index = compiled
                    .definition()
                    .nodes
                    .iter()
                    .position(|node| node.id == sink.sink)
                    .context("Mapper sink identity is missing")?;
                Ok(OutputScope {
                    sink: index,
                    user: self.user(&sink.user)?,
                    channel: sink.channel,
                    source_users: sink
                        .source_users
                        .iter()
                        .map(|user| self.user(user))
                        .collect::<Result<_>>()?,
                })
            })
            .collect()
    }
    fn all_scopes(&self, compiled: &Compiled) -> Result<Vec<OutputScope>> {
        let mut scopes = BTreeMap::new();
        for source in compiled.sources() {
            for scope in self.scopes_for(compiled, &source)? {
                scopes.insert(scope.sink, scope);
            }
        }
        Ok(scopes.into_values().collect())
    }
    pub(crate) fn action(
        &mut self,
        origin: ProgramOrigin,
        action: Action,
        backend: &mut dyn Backend,
    ) -> Result<()> {
        self.action_at(origin, action, Instant::now(), backend)
    }

    fn action_at(
        &mut self,
        origin: ProgramOrigin,
        action: Action,
        now: Instant,
        backend: &mut dyn Backend,
    ) -> Result<()> {
        let actor = Actor::from_origin(origin);
        if !self.state.armed || actor.is_none() {
            backend.reserve(Some((origin, &action)), &[])?;
            return backend.original(origin, &action, Disposition::Passthrough);
        }
        let actor = actor.context("Missing mapper actor")?;
        let prepared = self
            .prepare(actor, origin, &action, now, backend)
            .and_then(|prepared| {
                backend.reserve(Some((origin, &action)), &prepared.effects)?;
                self.state
                    .sequence
                    .checked_add(1)
                    .context("Mapper state sequence exhausted")?;
                Ok(prepared)
            });
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.state.rejected_inputs = self.state.rejected_inputs.saturating_add(1);
                let reason = format!("{error:#}");
                // Losing a queued Hold UP cannot leave its earlier delayed DOWN
                // eligible to fire. Saved owners also cover a DOWN already sent.
                let deferred_claims = (self.compiled.delays().next().is_some()
                    || !self.compiled.observations().is_empty())
                    && self.actors.get(&actor).is_some_and(|context| {
                        !context.delays.is_empty()
                            || !context.observations.is_empty()
                            || !context.owners.is_empty()
                    });
                if deferred_claims {
                    let cleanup = self.retire(backend);
                    self.state.error = Some(reason.clone());
                    return match cleanup {
                        Ok(()) => Err(error),
                        Err(cleanup) => {
                            Err(anyhow::anyhow!("{reason}; mapper cleanup: {cleanup:#}"))
                        }
                    };
                }
                self.state.error = Some(reason);
                self.advance()?;
                return Err(error);
            }
        };
        let delivery = backend
            .original(origin, &action, prepared.disposition)
            .and_then(|()| {
                for effect in &prepared.effects {
                    backend.deliver(&effect.owner, &effect.effect, None)?;
                }
                Ok(())
            });
        if let Err(error) = delivery {
            self.state.rejected_inputs = self.state.rejected_inputs.saturating_add(1);
            let reason = format!("{error:#}");
            let cleanup = self.retire(backend);
            self.state.error = Some(reason.clone());
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup) => Err(anyhow::anyhow!("{reason}; mapper cleanup: {cleanup:#}")),
            };
        }
        if let Some(mut context) = prepared.context {
            if let Err(error) =
                self.anchor_schedules(&mut context, &prepared.schedules, Instant::now().max(now))
            {
                self.state.rejected_inputs = self.state.rejected_inputs.saturating_add(1);
                let reason = format!("Mapper schedule commit failed: {error:#}");
                let cleanup = self.retire(backend);
                self.state.error = Some(reason.clone());
                return match cleanup {
                    Ok(()) => Err(anyhow::anyhow!(reason)),
                    Err(cleanup) => Err(anyhow::anyhow!("{reason}; mapper cleanup: {cleanup:#}")),
                };
            }
            self.commit_context(actor, context);
            self.state.processed_inputs = self.state.processed_inputs.saturating_add(1);
            self.state.emitted_outputs = self
                .state
                .emitted_outputs
                .saturating_add(prepared.effects.len() as u64);
            self.advance()?;
        }
        Ok(())
    }
    fn prepare(
        &self,
        actor: Actor,
        origin: ProgramOrigin,
        action: &Action,
        now: Instant,
        backend: &mut dyn Backend,
    ) -> Result<Prepared> {
        let user = action.user();
        ensure!(
            user < self.seats.len() && self.active[user],
            "Unknown active mapper source user"
        );
        ensure!(
            matches!(origin, ProgramOrigin::Manual(u) | ProgramOrigin::Agent(_, u) if u == user),
            "Mapper origin and source user disagree"
        );
        let Some(source) = self.action_source(action)? else {
            return Ok(Prepared {
                disposition: Disposition::Passthrough,
                context: None,
                effects: vec![],
                schedules: ScheduleChanges::default(),
            });
        };
        let Some(disposition) = self.compiled.source_disposition(&source) else {
            return Ok(Prepared {
                disposition: Disposition::Passthrough,
                context: None,
                effects: vec![],
                schedules: ScheduleChanges::default(),
            });
        };
        let scopes = self.scopes(&source)?;
        backend.authorize(origin, user, &scopes)?;
        ensure!(
            !self.actor_cleanup.contains(&actor),
            "Mapper actor cleanup is pending"
        );
        ensure!(
            self.actors.contains_key(&actor)
                || self.actors.len() + self.actor_cleanup.len() < mapper::MAX_ACTORS,
            "Mapper actor context limit reached"
        );
        let mut context = self
            .actors
            .get(&actor)
            .cloned()
            .unwrap_or_else(|| ContextState {
                signals: mapper::State::new(&self.compiled),
                quantizer: Quantizer::default(),
                pointers: BTreeMap::new(),
                owners: BTreeMap::new(),
                keys: BTreeSet::new(),
                timers: BTreeMap::new(),
                delays: BTreeMap::new(),
                observations: BTreeMap::new(),
            });
        let input = self.runtime_input(action, &mut context, backend)?;
        let emissions =
            context
                .signals
                .evaluate_at(&self.compiled, input, self.run_elapsed_us(now))?;
        let schedules =
            self.reconcile_schedules(actor, origin, user, &mut context, now, backend)?;
        let effects = self.prepare_effects(origin, &mut context, &scopes, emissions)?;
        Ok(Prepared {
            disposition,
            context: Some(context),
            effects,
            schedules,
        })
    }

    /// Reconcile every scheduled wave, including Delay→Timer and Timer→Delay.
    /// All changes stay in the private actor clone until delivery and anchoring.
    fn reconcile_schedules(
        &self,
        actor: Actor,
        origin: ProgramOrigin,
        user: usize,
        context: &mut ContextState,
        now: Instant,
        backend: &mut dyn Backend,
    ) -> Result<ScheduleChanges> {
        let mut changes = ScheduleChanges::default();
        context
            .timers
            .retain(|index, _| context.signals.timer_enabled(*index));
        for (index, interval_ms, count) in self.compiled.timers() {
            if context.signals.timer_enabled(index) && !context.timers.contains_key(&index) {
                let pending = self
                    .actors
                    .iter()
                    .filter(|(owner, _)| **owner != actor)
                    .flat_map(|(_, other)| other.timers.values())
                    .chain(context.timers.values())
                    .filter(|timer| matches!(timer, TimerState::Running(_)))
                    .count();
                ensure!(
                    pending < MAX_PENDING_TIMERS,
                    "Mapper pending timer capacity reached"
                );
                let scopes =
                    self.output_scopes(&self.compiled, self.compiled.timer_outputs(index))?;
                backend.authorize(origin, user, &scopes)?;
                let windows = backend.timer_windows(&scopes)?;
                ensure!(
                    windows.len() == scopes.len(),
                    "Scheduled mapper recipient count changed"
                );
                let watch = backend.watch_timer_targets(&windows)?;
                let interval = Duration::from_millis(u64::from(interval_ms));
                context.timers.insert(
                    index,
                    TimerState::Running(TimerRun {
                        origin,
                        source_user: user,
                        windows,
                        watch,
                        interval,
                        due: now + interval,
                        remaining: count,
                    }),
                );
                changes.timers.push(index);
            }
        }
        let pending = context.signals.pending_delays();
        context
            .delays
            .retain(|id, _| pending.iter().any(|job| job.id == *id));
        let session_pending: usize = self
            .actors
            .iter()
            .filter(|(owner, _)| **owner != actor)
            .map(|(_, other)| other.delays.len())
            .sum();
        ensure!(
            session_pending + pending.len() <= mapper::MAX_PENDING_DELAYS_SESSION,
            "Mapper pending delay session capacity reached"
        );
        for job in pending {
            if context.delays.contains_key(&job.id) {
                continue;
            }
            ensure!(
                job.due_elapsed_us.is_none(),
                "Delayed mapper job is missing its authority receipt"
            );
            let (_, delay_ms) = self
                .compiled
                .delays()
                .find(|(index, _)| *index == job.node_index)
                .context("Delayed mapper node is no longer compiled")?;
            let scopes =
                self.output_scopes(&self.compiled, self.compiled.delay_outputs(job.node_index))?;
            backend.authorize(origin, user, &scopes)?;
            let windows = backend.timer_windows(&scopes)?;
            ensure!(
                windows.len() == scopes.len(),
                "Scheduled mapper recipient count changed"
            );
            let watch = backend.watch_timer_targets(&windows)?;
            let interval = Duration::from_millis(u64::from(delay_ms));
            context.delays.insert(
                job.id,
                DelayedRun {
                    node_index: job.node_index,
                    origin,
                    source_user: user,
                    scopes,
                    windows,
                    watch,
                    due: now + interval,
                },
            );
            changes.delays.push(job.id);
        }
        let pending = context.signals.pending_observations();
        context
            .observations
            .retain(|id, _| pending.iter().any(|job| job.id == *id));
        let session_pending: usize = self
            .actors
            .iter()
            .filter(|(owner, _)| **owner != actor)
            .map(|(_, other)| other.observations.len())
            .sum();
        let mut dropped = Vec::new();
        for job in pending {
            if context.observations.contains_key(&job.id) {
                continue;
            }
            if session_pending + context.observations.len()
                >= mapper::MAX_PENDING_OBSERVATIONS_SESSION
            {
                dropped.push(job.id);
                continue;
            }
            let descriptor = self
                .compiled
                .observations()
                .into_iter()
                .find(|descriptor| descriptor.node_index == job.node_index)
                .context("Observation node is no longer compiled")?;
            let capture_user = self.user(&descriptor.capture_user)?;
            let source_users = descriptor
                .source_users
                .iter()
                .map(|id| self.user(id))
                .collect::<Result<Vec<_>>>()?;
            let scopes = self.output_scopes(
                &self.compiled,
                self.compiled.observation_outputs(job.node_index),
            )?;
            backend.authorize(origin, user, &scopes)?;
            let authority = backend.capture_authority(origin, user, capture_user, &source_users)?;
            let windows = backend.timer_windows(&scopes)?;
            ensure!(
                windows.len() == scopes.len(),
                "Observation sink recipient count changed"
            );
            let mut watched = windows.clone();
            watched.push(authority.window);
            let watch = backend.watch_timer_targets(&watched)?;
            let ticket = self
                .observation_ticket
                .get()
                .checked_add(1)
                .context("Mapper observation ticket exhausted")?;
            self.observation_ticket.set(ticket);
            context.observations.insert(
                job.id,
                ObservationRun {
                    ticket,
                    node_index: job.node_index,
                    origin,
                    source_user: user,
                    capture_user,
                    source_users,
                    authority,
                    scopes,
                    windows,
                    watch,
                    accepted_at: now,
                },
            );
            changes.observations.push(job.id);
        }
        context.signals.discard_observations(&dropped)?;
        Ok(changes)
    }

    fn anchor_schedules(
        &self,
        context: &mut ContextState,
        changes: &ScheduleChanges,
        accepted_at: Instant,
    ) -> Result<()> {
        // Core ordering is in microseconds. Round acceptance upward, then use
        // the same deadline for native scheduling so ties cannot disagree and
        // no value becomes due before its full accepted delay has elapsed.
        let elapsed = accepted_at.saturating_duration_since(self.run_started);
        let mut accepted_us =
            u64::try_from(elapsed.as_micros()).context("Mapper acceptance clock exhausted")?;
        if !elapsed.subsec_nanos().is_multiple_of(1_000) {
            accepted_us = accepted_us
                .checked_add(1)
                .context("Mapper acceptance clock exhausted")?;
        }
        context
            .signals
            .anchor_delays(&changes.delays, accepted_us)?;
        for index in &changes.timers {
            if let Some(TimerState::Running(timer)) = context.timers.get_mut(index) {
                timer.due = accepted_at + timer.interval;
            }
        }
        for id in &changes.observations {
            let observation = context
                .observations
                .get_mut(id)
                .context("Missing observation admission receipt")?;
            observation.accepted_at = accepted_at;
        }
        let pending = context.signals.pending_delays();
        for id in &changes.delays {
            let delay = context
                .delays
                .get_mut(id)
                .context("Missing new mapper delay receipt")?;
            let due_us = pending
                .iter()
                .find(|job| job.id == *id)
                .and_then(|job| job.due_elapsed_us)
                .context("Missing anchored mapper delay deadline")?;
            delay.due = self
                .run_started
                .checked_add(Duration::from_micros(due_us))
                .context("Mapper delay deadline exceeds monotonic clock")?;
        }
        Ok(())
    }

    fn prepare_effects(
        &self,
        origin: ProgramOrigin,
        context: &mut ContextState,
        scopes: &[OutputScope],
        emissions: Vec<Emission>,
    ) -> Result<Vec<PlannedEffect>> {
        let mut effects = Vec::new();
        for emission in emissions {
            let Some(effect) = context.quantizer.quantize(&emission)? else {
                continue;
            };
            let scope = scopes
                .iter()
                .find(|scope| self.compiled.definition().nodes[scope.sink].id == emission.sink)
                .context("Mapper emitted outside preauthorized dependencies")?;
            let owner = if effect.release() {
                let Some(owner) = context.owners.remove(&scope.sink) else {
                    continue;
                };
                owner
            } else {
                context
                    .owners
                    .get(&scope.sink)
                    .cloned()
                    .unwrap_or_else(|| SinkOwner {
                        origin,
                        run: self.state.run,
                        sink: scope.sink,
                        user: scope.user,
                        source_users: scope.source_users.clone(),
                    })
            };
            if effect.hold_down() {
                context.owners.insert(scope.sink, owner.clone());
            }
            effects.push(PlannedEffect { owner, effect });
        }
        ensure!(
            effects
                .iter()
                .map(|effect| effect.effect.cost())
                .sum::<usize>()
                <= mapper::MAX_EFFECTS,
            "Mapper wave exceeds its weighted effect bound"
        );
        Ok(effects)
    }

    /// Service at most one due wave per routing-loop turn. Oldest deadline
    /// wins; ties are deterministic across actors and nodes.
    pub(crate) fn poll_timers(&mut self, now: Instant, backend: &mut dyn Backend) -> Result<()> {
        let mut due = Vec::new();
        for (&actor, context) in &self.actors {
            for (&index, state) in &context.timers {
                if let TimerState::Running(timer) = state
                    && timer.due <= now
                {
                    due.push((timer.due, actor, DueWork::Timer(index)));
                }
            }
            for (&id, delay) in &context.delays {
                if delay.due <= now {
                    due.push((delay.due, actor, DueWork::Delay(id)));
                }
            }
        }
        let next_due = due.into_iter().min();
        // An observation is ordered by its accepted trigger time against due
        // Timer/Delay deadlines, preventing a stream of new captures starving
        // older scheduled work. Canceled physical tasks drain while disarmed.
        match self.poll_observations(now, next_due.map(|(due, _, _)| due), backend) {
            Ok(true) => return Ok(()),
            Ok(false) => (),
            Err(error) => {
                let reason = format!("Mapper observation stopped: {error:#}");
                let cleanup = self.retire(backend);
                self.state.error = Some(reason.clone());
                return match cleanup {
                    Ok(()) => Err(anyhow::anyhow!(reason)),
                    Err(cleanup) => Err(anyhow::anyhow!("{reason}; mapper cleanup: {cleanup:#}")),
                };
            }
        }
        if !self.state.armed {
            return Ok(());
        }
        let Some((_, actor, work)) = next_due else {
            return Ok(());
        };
        let result = match work {
            DueWork::Timer(index) => self.deliver_timer(actor, index, now, backend),
            DueWork::Delay(id) => self.deliver_delay(actor, id, now, backend),
        };
        if let Err(error) = result {
            let kind = match work {
                DueWork::Timer(_) => "timer",
                DueWork::Delay(_) => "delay",
            };
            let reason = format!("Mapper {kind} stopped: {error:#}");
            let cleanup = self.retire(backend);
            self.state.error = Some(reason.clone());
            return match cleanup {
                Ok(()) => Err(anyhow::anyhow!(reason)),
                Err(cleanup) => Err(anyhow::anyhow!("{reason}; mapper cleanup: {cleanup:#}")),
            };
        }
        Ok(())
    }

    fn check_observation(&self, job: &ObservationRun, backend: &mut dyn Backend) -> Result<()> {
        let scopes = self.output_scopes(
            &self.compiled,
            self.compiled.observation_outputs(job.node_index),
        )?;
        ensure!(scopes == job.scopes, "Observation sink scope changed");
        backend.authorize(job.origin, job.source_user, &scopes)?;
        ensure!(
            backend.capture_authority(
                job.origin,
                job.source_user,
                job.capture_user,
                &job.source_users
            )? == job.authority,
            "Observation capture permission, generation or selected target changed"
        );
        backend.check_scheduled_lifetime(job.watch.as_deref())?;
        ensure!(
            backend.timer_windows(&scopes)? == job.windows,
            "Observation sink recipient changed"
        );
        Ok(())
    }

    /// One dispatch or completion per turn. The task slot is independent of actor
    /// contexts so EOF/Disarm/Apply cannot free a still-running physical worker.
    fn poll_observations(
        &mut self,
        now: Instant,
        earlier_due: Option<Instant>,
        backend: &mut dyn Backend,
    ) -> Result<bool> {
        if let Some(task) = &mut self.observation_task {
            if task.ready.is_none() {
                task.ready = task.task.poll();
            }
            let receipt = self
                .actors
                .get(&task.actor)
                .and_then(|context| context.observations.get(&task.id))
                .filter(|job| {
                    self.state.armed && task.run == self.state.run && job.ticket == task.ticket
                })
                .cloned();
            if now >= task.deadline
                && (receipt.is_some() || (self.state.armed && task.ready.is_none()))
            {
                task.task.cancel();
                bail!("Mapper observation timed out");
            }
            if task.ready.is_some() {
                if receipt
                    .as_ref()
                    .is_some_and(|job| earlier_due.is_some_and(|due| due <= job.accepted_at))
                {
                    return Ok(false);
                }
                let mut task = self.observation_task.take().expect("observed task exists");
                let result = task.ready.take().expect("observed result exists");
                if let Some(job) = receipt {
                    self.check_observation(&job, backend)?;
                    self.complete_observation((task.actor, task.id), job, result?, now, backend)?;
                }
                // Canceled completions are discarded without samples or effects.
                return Ok(true);
            }
            if receipt.is_none() {
                task.task.cancel();
            }
            return Ok(false);
        }
        if !self.state.armed {
            return Ok(false);
        }
        let next = self
            .actors
            .iter()
            .flat_map(|(&actor, context)| {
                context
                    .observations
                    .iter()
                    .map(move |(&id, job)| (job.accepted_at, job.ticket, actor, id))
            })
            .min();
        let Some((_, _, actor, id)) = next else {
            return Ok(false);
        };
        let job = self.actors[&actor].observations[&id].clone();
        if earlier_due.is_some_and(|due| due <= job.accepted_at) {
            return Ok(false);
        }
        self.check_observation(&job, backend)?;
        let query = self
            .queries
            .get(&job.node_index)
            .cloned()
            .context("Mapper observation query is not prepared")?;
        let task = self.capture_provider.start(
            job.authority.window,
            self.identities[job.capture_user].clone(),
            query,
        )?;
        self.observation_task = Some(ObservationTask {
            actor,
            id,
            ticket: job.ticket,
            run: self.state.run,
            deadline: now
                .checked_add(Duration::from_secs(5))
                .context("Observation deadline exhausted")?,
            task,
            ready: None,
        });
        Ok(true)
    }

    fn complete_observation(
        &mut self,
        actor_request: (Actor, u64),
        job: ObservationRun,
        result: mapper::ObservationResult,
        now: Instant,
        backend: &mut dyn Backend,
    ) -> Result<()> {
        let (actor, id) = actor_request;
        let mut context = self
            .actors
            .get(&actor)
            .cloned()
            .context("Observation actor was retired")?;
        ensure!(
            context
                .observations
                .remove(&id)
                .is_some_and(|receipt| receipt.ticket == job.ticket),
            "Observation request identity changed"
        );
        // Always clone CURRENT state, preserving inputs accepted during capture.
        let emissions = context.signals.complete_observation(
            &self.compiled,
            id,
            result,
            self.run_elapsed_us(now),
        )?;
        let schedules = self.reconcile_schedules(
            actor,
            job.origin,
            job.source_user,
            &mut context,
            now,
            backend,
        )?;
        let effects = self.prepare_effects(job.origin, &mut context, &job.scopes, emissions)?;
        let expected: BTreeMap<_, _> = job
            .scopes
            .iter()
            .zip(&job.windows)
            .map(|(scope, &window)| (scope.sink, window))
            .collect();
        for planned in &effects {
            if !planned.effect.release() {
                let window = *expected
                    .get(&planned.owner.sink)
                    .context("Observation effect left saved sink scope")?;
                backend.check_timer_effect(&planned.owner, &planned.effect, window)?;
            }
        }
        backend.reserve(None, &effects)?;
        self.state
            .sequence
            .checked_add(1)
            .context("Mapper state sequence exhausted")?;
        for planned in &effects {
            let window = if planned.effect.release() {
                None
            } else {
                Some(
                    *expected
                        .get(&planned.owner.sink)
                        .context("Observation effect left saved sink scope")?,
                )
            };
            backend.deliver(&planned.owner, &planned.effect, window)?;
        }
        self.anchor_schedules(&mut context, &schedules, Instant::now().max(now))?;
        self.commit_context(actor, context);
        self.state.observation_completions = self.state.observation_completions.saturating_add(1);
        self.state.emitted_outputs = self
            .state
            .emitted_outputs
            .saturating_add(effects.len() as u64);
        self.advance()
    }

    fn deliver_timer(
        &mut self,
        actor: Actor,
        index: usize,
        now: Instant,
        backend: &mut dyn Backend,
    ) -> Result<()> {
        let mut context = self.actors[&actor].clone();
        let TimerState::Running(timer) = context.timers[&index].clone() else {
            unreachable!("due timer is running");
        };
        let scopes = self.output_scopes(&self.compiled, self.compiled.timer_outputs(index))?;
        backend.authorize(timer.origin, timer.source_user, &scopes)?;
        backend.check_scheduled_lifetime(timer.watch.as_deref())?;
        ensure!(
            backend.timer_windows(&scopes)? == timer.windows,
            "timer recipient changed"
        );
        let emissions =
            context
                .signals
                .evaluate_timer_at(&self.compiled, index, self.run_elapsed_us(now))?;
        let schedules = self.reconcile_schedules(
            actor,
            timer.origin,
            timer.source_user,
            &mut context,
            now,
            backend,
        )?;
        let effects = self.prepare_effects(timer.origin, &mut context, &scopes, emissions)?;
        let expected: BTreeMap<_, _> = scopes
            .iter()
            .zip(&timer.windows)
            .map(|(scope, &window)| (scope.sink, window))
            .collect();
        for planned in &effects {
            if !planned.effect.release() {
                backend.check_timer_effect(
                    &planned.owner,
                    &planned.effect,
                    expected[&planned.owner.sink],
                )?;
            }
        }
        backend.reserve(None, &effects)?;
        self.state
            .sequence
            .checked_add(1)
            .context("Mapper state sequence exhausted")?;
        for planned in &effects {
            let window = (!planned.effect.release()).then(|| expected[&planned.owner.sink]);
            backend.deliver(&planned.owner, &planned.effect, window)?;
        }
        let next = if timer.remaining == 1 {
            TimerState::Exhausted
        } else {
            TimerState::Running(TimerRun {
                due: Instant::now().max(now) + timer.interval,
                remaining: timer.remaining - 1,
                ..timer
            })
        };
        context.timers.insert(index, next);
        self.anchor_schedules(&mut context, &schedules, Instant::now().max(now))?;
        self.commit_context(actor, context);
        self.state.timer_pulses = self.state.timer_pulses.saturating_add(1);
        self.state.emitted_outputs = self
            .state
            .emitted_outputs
            .saturating_add(effects.len() as u64);
        self.advance()
    }

    fn deliver_delay(
        &mut self,
        actor: Actor,
        id: u64,
        now: Instant,
        backend: &mut dyn Backend,
    ) -> Result<()> {
        let mut context = self.actors[&actor].clone();
        let delay = context
            .delays
            .remove(&id)
            .context("Missing due mapper delay")?;
        let scopes = self.output_scopes(
            &self.compiled,
            self.compiled.delay_outputs(delay.node_index),
        )?;
        ensure!(scopes == delay.scopes, "Delayed mapper sink scope changed");
        backend.authorize(delay.origin, delay.source_user, &scopes)?;
        backend.check_scheduled_lifetime(delay.watch.as_deref())?;
        ensure!(
            backend.timer_windows(&scopes)? == delay.windows,
            "delay recipient changed"
        );
        let emissions =
            context
                .signals
                .evaluate_delay_at(&self.compiled, id, self.run_elapsed_us(now))?;
        let schedules = self.reconcile_schedules(
            actor,
            delay.origin,
            delay.source_user,
            &mut context,
            now,
            backend,
        )?;
        let effects = self.prepare_effects(delay.origin, &mut context, &scopes, emissions)?;
        let expected: BTreeMap<_, _> = scopes
            .iter()
            .zip(&delay.windows)
            .map(|(scope, &window)| (scope.sink, window))
            .collect();
        for planned in &effects {
            if !planned.effect.release() {
                let window = *expected
                    .get(&planned.owner.sink)
                    .context("Delayed effect left saved sink scope")?;
                backend.check_timer_effect(&planned.owner, &planned.effect, window)?;
            }
        }
        backend.reserve(None, &effects)?;
        self.state
            .sequence
            .checked_add(1)
            .context("Mapper state sequence exhausted")?;
        for planned in &effects {
            let window = if planned.effect.release() {
                None
            } else {
                Some(
                    *expected
                        .get(&planned.owner.sink)
                        .context("Delayed effect left saved sink scope")?,
                )
            };
            backend.deliver(&planned.owner, &planned.effect, window)?;
        }
        self.anchor_schedules(&mut context, &schedules, Instant::now().max(now))?;
        self.commit_context(actor, context);
        self.state.delayed_events = self.state.delayed_events.saturating_add(1);
        self.state.emitted_outputs = self
            .state
            .emitted_outputs
            .saturating_add(effects.len() as u64);
        self.advance()
    }

    fn action_source(&self, action: &Action) -> Result<Option<Source>> {
        let user = self.identities[action.user()].clone();
        Ok(match action {
            Action::MoveTo { .. } => Some(Source::Pointer { user }),
            Action::Text { .. } => None,
            Action::Packet { packet, .. } => {
                packet.validate()?;
                if packet.kind == 1 {
                    ensure!(
                        Some(packet.device) == self.seats[action.user()].keyboard,
                        "Mapper key packet does not belong to its source user"
                    );
                    Some(Source::Key {
                        user,
                        key: u16::try_from(packet.data)?,
                    })
                } else if packet.kind == 0 {
                    ensure!(
                        packet.device == self.seats[action.user()].mouse,
                        "Mapper pointer packet does not belong to its source user"
                    );
                    match packet.flags {
                        0 => Some(Source::Pointer { user }),
                        0x400 | 0x800 => Some(Source::Wheel {
                            user,
                            horizontal: packet.flags == 0x800,
                        }),
                        flags => Some(Source::Button {
                            user,
                            button: (flags.trailing_zeros() / 2 + 1) as u8,
                        }),
                    }
                } else {
                    None
                }
            }
        })
    }
    fn runtime_input(
        &self,
        action: &Action,
        context: &mut ContextState,
        backend: &mut dyn Backend,
    ) -> Result<mapper::RuntimeInput> {
        let user = action.user();
        let identity = self.identities[user].clone();
        if matches!(
            action,
            Action::MoveTo { .. }
                | Action::Packet {
                    packet: emulated::Packet {
                        kind: 0,
                        flags: 0,
                        ..
                    },
                    ..
                }
        ) {
            let previous = if let Some(position) = context.pointers.get(&user) {
                *position
            } else {
                let (x, y) = backend.pointer(user)?;
                [f64::from(x), f64::from(y)]
            };
            let (position, delta) = match action {
                Action::MoveTo { x, y, .. } => {
                    let position = [f64::from(*x), f64::from(*y)];
                    (
                        position,
                        [position[0] - previous[0], position[1] - previous[1]],
                    )
                }
                Action::Packet { packet, .. } => {
                    let delta = [f64::from(packet.x), f64::from(packet.y)];
                    ([previous[0] + delta[0], previous[1] + delta[1]], delta)
                }
                _ => unreachable!(),
            };
            context.pointers.insert(user, position);
            return Ok(mapper::RuntimeInput::Pointer {
                user: identity,
                position,
                delta,
            });
        }
        let Action::Packet { packet, .. } = action else {
            bail!("Unsupported mapper input action");
        };
        if packet.kind == 1 {
            let key = u16::try_from(packet.data)?;
            let raw = (user, key, packet.x, packet.flags & 6);
            if packet.flags & 1 == 0 {
                context.keys.insert(raw);
            } else {
                context.keys.remove(&raw);
            }
            ensure!(
                context.keys.len() <= MAX_RAW_KEYS_PER_ACTOR,
                "Mapper raw-key identity capacity reached"
            );
            let held = context
                .keys
                .iter()
                .any(|(source_user, code, _, _)| *source_user == user && *code == key);
            return Ok(mapper::RuntimeInput::Key {
                user: identity,
                key,
                held,
            });
        }
        if matches!(packet.flags, 0x400 | 0x800) {
            return Ok(mapper::RuntimeInput::Wheel {
                user: identity,
                horizontal: packet.flags == 0x800,
                amount: f64::from(packet.data as u16 as i16),
            });
        }
        Ok(mapper::RuntimeInput::Button {
            user: identity,
            button: (packet.flags.trailing_zeros() / 2 + 1) as u8,
            held: packet.flags.trailing_zeros() & 1 == 0,
        })
    }
    pub(crate) fn retire(&mut self, backend: &mut dyn Backend) -> Result<()> {
        if let Some(task) = &self.observation_task {
            task.task.cancel();
        }
        self.state.armed = false;
        self.actors.clear();
        self.global_cleanup = true;
        self.actor_cleanup.clear();
        self.last_cleanup = Some(Instant::now());
        let result = self.release_scope(None, backend);
        if let Err(error) = &result {
            self.state.error = Some(format!("{error:#}"));
        }
        self.advance()?;
        result
    }
    pub(crate) fn cancel_capture_actor(&self, actor: Actor) {
        if let Some(task) = &self.observation_task
            && task.actor == actor
        {
            task.task.cancel();
        }
    }
    pub(crate) fn retire_actor(&mut self, actor: Actor, backend: &mut dyn Backend) -> Result<()> {
        if let Some(task) = &self.observation_task
            && task.actor == actor
        {
            task.task.cancel();
        }
        self.actors.remove(&actor);
        self.last_cleanup = Some(Instant::now());
        let result = self.release_scope(Some(actor), backend);
        if let Err(error) = &result {
            self.state.error = Some(format!("{error:#}"));
        }
        self.advance()?;
        result
    }
    fn release_scope(&mut self, actor: Option<Actor>, backend: &mut dyn Backend) -> Result<()> {
        let result = backend.release(self.state.run, actor);
        let pending = backend.pending(self.state.run, actor);
        match actor {
            None => {
                self.global_cleanup = pending != 0;
                if pending == 0 {
                    self.actor_cleanup.clear();
                }
            }
            Some(actor) => {
                if pending == 0 {
                    self.actor_cleanup.remove(&actor);
                } else {
                    self.actor_cleanup.insert(actor);
                }
            }
        }
        self.state.cleanup_pending = self.global_cleanup || !self.actor_cleanup.is_empty();
        result?;
        ensure!(pending == 0, "Mapper releases remain pending");
        Ok(())
    }
    pub(crate) fn poll_cleanup(&mut self, backend: &mut dyn Backend) -> Result<()> {
        if self.state.armed
            && self
                .last_health
                .is_none_or(|last| last.elapsed() >= Duration::from_millis(250))
        {
            self.last_health = Some(Instant::now());
            if let Err(error) = backend.health(&self.all_scopes(&self.compiled)?) {
                let reason = format!("Mapper recipient unavailable: {error:#}");
                let cleanup = self.retire(backend);
                self.state.error = Some(reason.clone());
                return match cleanup {
                    Ok(()) => Err(anyhow::anyhow!(reason)),
                    Err(cleanup) => Err(anyhow::anyhow!("{reason}; mapper cleanup: {cleanup:#}")),
                };
            }
        }
        if !self.state.cleanup_pending
            || self
                .last_cleanup
                .is_some_and(|last| last.elapsed() < Duration::from_millis(250))
        {
            return Ok(());
        }
        self.last_cleanup = Some(Instant::now());
        let before = self.state.cleanup_pending;
        let result = if self.global_cleanup {
            self.release_scope(None, backend)
        } else {
            let mut error = None;
            for actor in self.actor_cleanup.clone() {
                if let Err(failure) = self.release_scope(Some(actor), backend) {
                    error = Some(failure);
                }
            }
            error.map_or(Ok(()), Err)
        };
        if before != self.state.cleanup_pending {
            self.advance()?;
        }
        if let Err(error) = &result {
            self.state.error = Some(format!("{error:#}"));
        }
        result
    }
    pub(crate) fn add_user(&mut self, identity: String, seat: ResolvedSeat) -> Result<()> {
        ensure!(
            !self.state.armed && !self.state.cleanup_pending,
            "Disarm and finish mapper cleanup before adding a user"
        );
        ensure!(
            !identity.is_empty() && !self.identities.contains(&identity),
            "Mapper user identity must be unique"
        );
        self.identities.push(identity);
        self.seats.push(seat);
        self.active.push(true);
        self.advance()
    }
    pub(crate) fn prepare_remove_user(
        &mut self,
        user: usize,
        backend: &mut dyn Backend,
    ) -> Result<u64> {
        ensure!(
            user < self.seats.len() && self.active[user],
            "Unknown active mapper user"
        );
        let identity = &self.identities[user];
        let referenced = self.compiled.sources().iter().any(|source| source.user() == identity)
            || self.compiled.definition().nodes.iter().any(|node| matches!(&node.kind, mapper::NodeKind::Sink { user, .. } if user == identity)
                || matches!(&node.kind, mapper::NodeKind::PixelColor { capture_user, .. } | mapper::NodeKind::ImageMatch { capture_user, .. } | mapper::NodeKind::Ocr { capture_user, .. } if capture_user == identity));
        if referenced && (self.state.armed || self.state.cleanup_pending) {
            self.retire(backend)?;
        }
        self.state
            .sequence
            .checked_add(1)
            .context("Mapper state sequence exhausted")
    }

    pub(crate) fn commit_remove_user(&mut self, user: usize, sequence: u64) {
        self.active[user] = false;
        self.state.sequence = sequence;
    }
}

/// Platform-independent delivery plan. Binary pulses remain explicit here so
/// admission reserves BOTH transitions before any native effect is attempted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Quantized {
    Position([i32; 2]),
    Delta([i32; 2]),
    Wheel {
        horizontal: bool,
        delta: i16,
    },
    Key {
        key: u16,
        held: bool,
        pulse: bool,
    },
    Button {
        button: u8,
        held: bool,
        pulse: bool,
    },
    /// One instantaneous WM_CHAR packet per UTF-16 unit, scoped to one recipient.
    Text(String),
}
impl Quantized {
    pub(crate) fn release(&self) -> bool {
        matches!(
            self,
            Self::Key {
                held: false,
                pulse: false,
                ..
            } | Self::Button {
                held: false,
                pulse: false,
                ..
            }
        )
    }
    pub(crate) fn hold_down(&self) -> bool {
        matches!(
            self,
            Self::Key {
                held: true,
                pulse: false,
                ..
            } | Self::Button {
                held: true,
                pulse: false,
                ..
            }
        )
    }
    pub(crate) fn cost(&self) -> usize {
        if let Self::Text(text) = self {
            return text.encode_utf16().count();
        }
        if matches!(
            self,
            Self::Key { pulse: true, .. } | Self::Button { pulse: true, .. }
        ) {
            2
        } else {
            1
        }
    }
}

/// One instance belongs to one actor and run. Prepare an entire wave on a clone
/// and commit it only after admission; retirement discards every remainder.
#[derive(Debug, Clone, Default)]
pub(crate) struct Quantizer {
    remainders: BTreeMap<String, [f64; 2]>,
}
impl Quantizer {
    pub(crate) fn quantize(&mut self, emission: &Emission) -> Result<Option<Quantized>> {
        ensure!(
            emission.channel.signal() == emission.value.signal(),
            "Mapper sink {} has an incompatible value",
            emission.sink
        );
        let previous = self
            .remainders
            .get(&emission.sink)
            .copied()
            .unwrap_or_default();
        let result = match (&emission.channel, &emission.value) {
            (OutputChannel::TypeText, Value::Text(text)) => {
                (mapper::type_text_cost(text)? != 0).then(|| Quantized::Text(text.clone()))
            }
            (OutputChannel::Position, Value::Position(value)) => {
                let x = integer(value[0], true)?;
                let y = integer(value[1], true)?;
                Some(Quantized::Position([x, y]))
            }
            (OutputChannel::Delta, Value::Delta(value)) => {
                let x = checked_number(value[0] + previous[0])?;
                let y = checked_number(value[1] + previous[1])?;
                let dx = integer(x, false)?;
                let dy = integer(y, false)?;
                self.remainders.insert(
                    emission.sink.clone(),
                    [x - f64::from(dx), y - f64::from(dy)],
                );
                (dx != 0 || dy != 0).then_some(Quantized::Delta([dx, dy]))
            }
            (OutputChannel::Wheel { horizontal }, Value::Impulse(value)) => {
                let total = checked_number(*value + previous[0])?;
                let quantized = integer(total, false)?;
                let delta = i16::try_from(quantized).map_err(|_| {
                    anyhow::anyhow!("Mapper wheel output is outside signed 16-bit range")
                })?;
                self.remainders
                    .insert(emission.sink.clone(), [total - f64::from(delta), 0.0]);
                (delta != 0).then_some(Quantized::Wheel {
                    horizontal: *horizontal,
                    delta,
                })
            }
            (OutputChannel::KeyHold { key }, Value::Hold(held)) => Some(Quantized::Key {
                key: *key,
                held: *held,
                pulse: false,
            }),
            (OutputChannel::KeyPulse { key }, Value::Pulse(active)) => {
                active.then_some(Quantized::Key {
                    key: *key,
                    held: true,
                    pulse: true,
                })
            }
            (OutputChannel::ButtonHold { button }, Value::Hold(held)) => Some(Quantized::Button {
                button: *button,
                held: *held,
                pulse: false,
            }),
            (OutputChannel::ButtonPulse { button }, Value::Pulse(active)) => {
                active.then_some(Quantized::Button {
                    button: *button,
                    held: true,
                    pulse: true,
                })
            }
            _ => bail!("Mapper output value does not match its channel"),
        };
        Ok(result)
    }
}

fn checked_number(value: f64) -> Result<f64> {
    ensure!(
        value.is_finite() && value.abs() <= NUMERIC_LIMIT,
        "Mapper numeric output exceeds its finite signal domain"
    );
    Ok(value)
}
fn integer(value: f64, absolute: bool) -> Result<i32> {
    let value = checked_number(value)?;
    let rounded = if absolute {
        value.round()
    } else {
        value.trunc()
    };
    ensure!(
        rounded >= f64::from(i32::MIN) && rounded <= f64::from(i32::MAX),
        "Mapper coordinate output is outside signed 32-bit range"
    );
    Ok(rounded as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct MockBackend {
        log: Vec<&'static str>,
        originals: Vec<(ProgramOrigin, Disposition)>,
        delivered: Vec<(SinkOwner, Quantized)>,
        holds: BTreeMap<SinkOwner, Quantized>,
        releases: Vec<(u64, Option<Actor>)>,
        held_source: bool,
        deny_authorization: bool,
        deny_reserve: bool,
        deny_exact_timer_recipient: bool,
        deny_watch: bool,
        expired_watch: bool,
        recipient_offset: usize,
        fail_timer_pulse_release: bool,
        fail_delivery: Option<usize>,
        fail_release: bool,
        delivery_attempts: usize,
        fail_original: bool,
        deny_capture: bool,
        capture_generation: u64,
        capture_window_offset: usize,
        capture_checks: Vec<(ProgramOrigin, usize, usize, Vec<usize>)>,
    }
    impl Backend for MockBackend {
        fn validate_definition(&mut self, _: &[Source], _: &[OutputScope]) -> Result<()> {
            self.log.push("definition");
            Ok(())
        }
        fn validate_arm(&mut self, _: &[Source], _: &[usize]) -> Result<()> {
            self.log.push("arm");
            ensure!(!self.held_source, "Affected binary source is held");
            Ok(())
        }
        fn authorize(&mut self, _: ProgramOrigin, _: usize, _: &[OutputScope]) -> Result<()> {
            self.log.push("authorize");
            ensure!(!self.deny_authorization, "Denied mapper scope");
            Ok(())
        }
        fn capture_authority(
            &mut self,
            origin: ProgramOrigin,
            source: usize,
            capture: usize,
            ancestry: &[usize],
        ) -> Result<CaptureAuthority> {
            self.capture_checks
                .push((origin, source, capture, ancestry.to_vec()));
            ensure!(!self.deny_capture, "Capture permission denied");
            Ok(CaptureAuthority {
                window: Window {
                    hwnd: capture + 10 + self.capture_window_offset,
                    pid: 100,
                },
                generation: self.capture_generation,
            })
        }
        fn pointer(&mut self, _: usize) -> Result<(i32, i32)> {
            self.log.push("pointer");
            Ok((100, 200))
        }
        fn timer_windows(&mut self, outputs: &[OutputScope]) -> Result<Vec<Window>> {
            Ok(outputs
                .iter()
                .map(|scope| Window {
                    hwnd: scope.user + 1 + self.recipient_offset,
                    pid: 100,
                })
                .collect())
        }
        fn watch_timer_targets(&self, _: &[Window]) -> Result<Option<Rc<ProcessWatch>>> {
            ensure!(!self.deny_watch, "Cannot retain scheduled process object");
            Ok(None)
        }
        fn check_scheduled_lifetime(&self, _: Option<&ProcessWatch>) -> Result<()> {
            ensure!(!self.expired_watch, "scheduled target process exited");
            Ok(())
        }
        fn reserve(
            &mut self,
            _: Option<(ProgramOrigin, &Action)>,
            _: &[PlannedEffect],
        ) -> Result<()> {
            self.log.push("reserve");
            ensure!(!self.deny_reserve, "Claim capacity exceeded");
            Ok(())
        }
        fn original(
            &mut self,
            origin: ProgramOrigin,
            _: &Action,
            disposition: Disposition,
        ) -> Result<()> {
            self.log.push("original");
            ensure!(!self.fail_original, "Controlled original input failure");
            self.originals.push((origin, disposition));
            Ok(())
        }
        fn check_timer_effect(&self, _: &SinkOwner, _: &Quantized, _: Window) -> Result<()> {
            ensure!(
                !self.deny_exact_timer_recipient,
                "timer effect recipient changed"
            );
            Ok(())
        }
        fn deliver(
            &mut self,
            owner: &SinkOwner,
            effect: &Quantized,
            expected: Option<Window>,
        ) -> Result<()> {
            self.log.push("deliver");
            self.delivery_attempts += 1;
            if expected.is_some() && self.fail_timer_pulse_release {
                self.holds.insert(owner.clone(), effect.clone());
                bail!("Native timer pulse UP remains pending");
            }
            ensure!(
                self.fail_delivery != Some(self.delivery_attempts),
                "Native sink failed"
            );
            if effect.hold_down() {
                self.holds.insert(owner.clone(), effect.clone());
            }
            if effect.release() {
                self.holds.remove(owner);
            }
            self.delivered.push((owner.clone(), effect.clone()));
            Ok(())
        }
        fn release(&mut self, run: u64, actor: Option<Actor>) -> Result<()> {
            self.log.push("release");
            self.releases.push((run, actor));
            ensure!(!self.fail_release, "Native UP remains pending");
            self.holds.retain(|owner, _| {
                owner.run != run
                    || actor.is_some_and(|actor| Actor::from_origin(owner.origin) != Some(actor))
            });
            Ok(())
        }
        fn pending(&self, run: u64, actor: Option<Actor>) -> usize {
            self.holds
                .keys()
                .filter(|owner| {
                    owner.run == run
                        && actor.is_none_or(|actor| Actor::from_origin(owner.origin) == Some(actor))
                })
                .count()
        }
    }
    fn runtime() -> Runtime {
        Runtime::new(
            vec!["a".into(), "b".into(), "c".into()],
            (0..3)
                .map(|index| {
                    let mouse = emulated::BASE | ((index + 1) << 1);
                    ResolvedSeat {
                        name: format!("User {index}"),
                        mouse,
                        keyboard: Some(mouse | 1),
                        target_pid: 1,
                    }
                })
                .collect(),
        )
        .unwrap()
    }
    fn graph(nodes: serde_json::Value, edges: serde_json::Value) -> Definition {
        serde_json::from_value(serde_json::json!({"version":1, "nodes":nodes, "edges":edges}))
            .unwrap()
    }
    fn motion_graph() -> Definition {
        graph(
            serde_json::json!([
                {"id":"source","kind":{"type":"pointer","user":"a"}},
                {"id":"scale","kind":{"type":"scale_delta","factor":0.5}},
                {"id":"sink","kind":{"type":"sink","user":"b","channel":{"type":"delta"}}}
            ]),
            serde_json::json!([
                {"from":{"node":"source","port":"delta"},"to":{"node":"scale","port":"value"}},
                {"from":{"node":"scale","port":"value"},"to":{"node":"sink","port":"value"}}
            ]),
        )
    }
    fn timer_graph() -> Definition {
        graph(
            serde_json::json!([
                {"id":"enable","kind":{"type":"key","user":"a","key":65,"disposition":"consume"}},
                {"id":"timer","kind":{"type":"timer","interval_ms":100,"count":2}},
                {"id":"sink","kind":{"type":"sink","user":"b","channel":{"type":"key_pulse","key":67}}}
            ]),
            serde_json::json!([
                {"from":{"node":"enable","port":"value"},"to":{"node":"timer","port":"enabled"}},
                {"from":{"node":"timer","port":"value"},"to":{"node":"sink","port":"value"}}
            ]),
        )
    }

    fn delay_wheel_graph() -> Definition {
        graph(
            serde_json::json!([
                {"id":"wheel","kind":{"type":"wheel","user":"a","horizontal":false,"disposition":"consume"}},
                {"id":"delay","kind":{"type":"delay","signal":"impulse","delay_ms":100}},
                {"id":"monitor","kind":{"type":"monitor","signal":"impulse"}},
                {"id":"sink","kind":{"type":"sink","user":"b","channel":{"type":"wheel","horizontal":false}}}
            ]),
            serde_json::json!([
                {"from":{"node":"wheel","port":"value"},"to":{"node":"delay","port":"value"}},
                {"from":{"node":"delay","port":"value"},"to":{"node":"monitor","port":"value"}},
                {"from":{"node":"delay","port":"value"},"to":{"node":"sink","port":"value"}}
            ]),
        )
    }

    fn delay_hold_graph() -> Definition {
        graph(
            serde_json::json!([
                {"id":"key","kind":{"type":"key","user":"a","key":65,"disposition":"consume"}},
                {"id":"hold-delay","kind":{"type":"delay","signal":"hold","delay_ms":100}},
                {"id":"sink","kind":{"type":"sink","user":"b","channel":{"type":"key_hold","key":67}}}
            ]),
            serde_json::json!([
                {"from":{"node":"key","port":"value"},"to":{"node":"hold-delay","port":"value"}},
                {"from":{"node":"hold-delay","port":"value"},"to":{"node":"sink","port":"value"}}
            ]),
        )
    }

    fn wheel(runtime: &Runtime, delta: i16) -> Action {
        Action::Packet {
            user: 0,
            packet: emulated::Packet::wheel(runtime.seats[0].mouse, delta),
        }
    }

    fn controlled_now(runtime: &Runtime) -> Instant {
        // Future monotonic time dominates real completion time, so no sleeps or
        // native processes are needed to qualify schedule boundaries.
        runtime.run_started + Duration::from_secs(86_400)
    }

    #[derive(Default)]
    struct CaptureControl {
        starts: Vec<(Window, String)>,
        ready: std::collections::VecDeque<Result<mapper::ObservationResult>>,
        cancellations: usize,
        provider_busy: bool,
    }
    struct FakeCaptureProvider(Rc<std::cell::RefCell<CaptureControl>>);
    struct FakeCaptureTask {
        control: Rc<std::cell::RefCell<CaptureControl>>,
        canceled: Cell<bool>,
    }
    impl crate::mapper_screen::Task for FakeCaptureTask {
        fn poll(&mut self) -> Option<Result<mapper::ObservationResult>> {
            self.control.borrow_mut().ready.pop_front()
        }
        fn cancel(&self) {
            if !self.canceled.replace(true) {
                self.control.borrow_mut().cancellations += 1;
            }
        }
    }
    impl crate::mapper_screen::Provider for FakeCaptureProvider {
        fn busy(&self) -> bool {
            self.0.borrow().provider_busy
        }
        fn start(
            &mut self,
            window: Window,
            user: String,
            _: Arc<crate::screen_analysis::PreparedQuery>,
        ) -> Result<Box<dyn crate::mapper_screen::Task>> {
            self.0.borrow_mut().starts.push((window, user));
            Ok(Box::new(FakeCaptureTask {
                control: self.0.clone(),
                canceled: Cell::new(false),
            }))
        }
    }
    fn capture_runtime() -> (Runtime, Rc<std::cell::RefCell<CaptureControl>>) {
        let mut runtime = runtime();
        let control = Rc::new(std::cell::RefCell::new(CaptureControl::default()));
        runtime.capture_provider = Box::new(FakeCaptureProvider(control.clone()));
        (runtime, control)
    }
    fn pixel_graph() -> Definition {
        graph(
            serde_json::json!([
                {"id":"source","kind":{"type":"wheel","user":"a","horizontal":false,"disposition":"consume"}},
                {"id":"pulse","kind":{"type":"impulse_to_pulse"}},
                {"id":"pixel","kind":{"type":"pixel_color","capture_user":"b","x":2,"y":3}},
                {"id":"red","kind":{"type":"monitor","signal":"state"}},
                {"id":"counter","kind":{"type":"counter","initial":0}},
                {"id":"count","kind":{"type":"monitor","signal":"state"}},
                {"id":"sink","kind":{"type":"sink","user":"c","channel":{"type":"key_pulse","key":67}}}
            ]),
            serde_json::json!([
                {"from":{"node":"source","port":"value"},"to":{"node":"pulse","port":"value"}},
                {"from":{"node":"pulse","port":"value"},"to":{"node":"pixel","port":"trigger"}},
                {"from":{"node":"pulse","port":"value"},"to":{"node":"counter","port":"count"}},
                {"from":{"node":"counter","port":"value"},"to":{"node":"count","port":"value"}},
                {"from":{"node":"pixel","port":"red"},"to":{"node":"red","port":"value"}},
                {"from":{"node":"pixel","port":"done"},"to":{"node":"sink","port":"value"}}
            ]),
        )
    }
    fn match_graph() -> Definition {
        use base64::Engine as _;
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::write_buffer_with_format(
            &mut bytes,
            &[1, 2, 3, 255],
            1,
            1,
            image::ColorType::Rgba8,
            image::ImageFormat::Png,
        )
        .unwrap();
        graph(
            serde_json::json!([
                {"id":"source","kind":{"type":"wheel","user":"a","horizontal":false,"disposition":"consume"}},
                {"id":"pulse","kind":{"type":"impulse_to_pulse"}},
                {"id":"match","kind":{"type":"image_match","capture_user":"b","template_png_base64":base64::engine::general_purpose::STANDARD.encode(bytes.into_inner()),"region":null,"min_score":0.95}},
                {"id":"score","kind":{"type":"monitor","signal":"state"}},
                {"id":"position","kind":{"type":"monitor","signal":"position"}},
                {"id":"sink","kind":{"type":"sink","user":"c","channel":{"type":"key_hold","key":67}}}
            ]),
            serde_json::json!([
                {"from":{"node":"source","port":"value"},"to":{"node":"pulse","port":"value"}},
                {"from":{"node":"pulse","port":"value"},"to":{"node":"match","port":"trigger"}},
                {"from":{"node":"match","port":"score"},"to":{"node":"score","port":"value"}},
                {"from":{"node":"match","port":"position"},"to":{"node":"position","port":"value"}},
                {"from":{"node":"match","port":"matched"},"to":{"node":"sink","port":"value"}}
            ]),
        )
    }
    fn ocr_graph() -> Definition {
        graph(
            serde_json::json!([
                {"id":"source","kind":{"type":"wheel","user":"a","horizontal":false}},
                {"id":"pulse","kind":{"type":"impulse_to_pulse"}},
                {"id":"ocr","kind":{"type":"ocr","capture_user":"b","region":null}},
                {"id":"text","kind":{"type":"monitor","signal":"text"}},
                {"id":"contains","kind":{"type":"text_contains","needle":"READY","case_sensitive":true}},
                {"id":"gate","kind":{"type":"gate","signal":"pulse"}},
                {"id":"counter","kind":{"type":"counter","initial":0}},
                {"id":"count","kind":{"type":"monitor","signal":"state"}},
                {"id":"sink","kind":{"type":"sink","user":"c","channel":{"type":"key_pulse","key":67}}}
            ]),
            serde_json::json!([
                {"from":{"node":"source","port":"value"},"to":{"node":"pulse","port":"value"}},
                {"from":{"node":"pulse","port":"value"},"to":{"node":"ocr","port":"trigger"}},
                {"from":{"node":"pulse","port":"value"},"to":{"node":"counter","port":"count"}},
                {"from":{"node":"counter","port":"value"},"to":{"node":"count","port":"value"}},
                {"from":{"node":"ocr","port":"text"},"to":{"node":"text","port":"value"}},
                {"from":{"node":"ocr","port":"text"},"to":{"node":"contains","port":"value"}},
                {"from":{"node":"contains","port":"value"},"to":{"node":"gate","port":"open"}},
                {"from":{"node":"ocr","port":"done"},"to":{"node":"gate","port":"value"}},
                {"from":{"node":"gate","port":"value"},"to":{"node":"sink","port":"value"}}
            ]),
        )
    }
    fn ocr_result(text: &str) -> mapper::ObservationResult {
        mapper::ObservationResult::Ocr { text: text.into() }
    }
    #[test]
    fn removing_ocr_read_user_retires_queued_work_before_user_tombstone() {
        let (mut runtime, control) = capture_runtime();
        let mut backend = MockBackend::default();
        apply_arm(&mut runtime, ocr_graph(), &mut backend);
        let now = controlled_now(&runtime);
        runtime
            .action_at(
                ProgramOrigin::Agent(1, 0),
                wheel(&runtime, 120),
                now,
                &mut backend,
            )
            .unwrap();
        runtime.poll_timers(now, &mut backend).unwrap();
        let sequence = runtime.prepare_remove_user(1, &mut backend).unwrap();
        runtime.commit_remove_user(1, sequence);
        assert!(!runtime.snapshot().armed);
        assert_eq!(runtime.snapshot().pending_observations, 0);
        assert!(control.borrow().cancellations > 0);
        control
            .borrow_mut()
            .ready
            .push_back(Ok(ocr_result("READY retired")));
        runtime.poll_timers(now, &mut backend).unwrap();
        assert!(backend.delivered.is_empty());
        assert!(runtime.monitor_snapshot(Actor::Agent(1)).samples.is_empty());
    }
    #[test]
    fn ocr_to_key_pulse_preserves_current_state_original_input_and_actor_private_text() {
        let (mut runtime, control) = capture_runtime();
        let mut backend = MockBackend::default();
        apply_arm(&mut runtime, ocr_graph(), &mut backend);
        assert!(control.borrow().starts.is_empty());
        let now = controlled_now(&runtime);
        let origin = ProgramOrigin::Agent(1, 0);
        runtime
            .action_at(origin, wheel(&runtime, 120), now, &mut backend)
            .unwrap();
        runtime.poll_timers(now, &mut backend).unwrap();
        runtime
            .action_at(origin, wheel(&runtime, 121), now, &mut backend)
            .unwrap();
        assert_eq!(backend.originals.len(), 2);
        assert!(
            backend
                .originals
                .iter()
                .all(|(_, disposition)| *disposition == Disposition::Passthrough)
        );
        control
            .borrow_mut()
            .ready
            .push_back(Ok(ocr_result("READY private-result")));
        runtime.poll_timers(now, &mut backend).unwrap();
        let samples = runtime.monitor_snapshot(Actor::Agent(1)).samples;
        assert!(samples.iter().any(|sample| sample.node == "text"
            && sample.value == Value::Text("READY private-result".into())));
        assert!(
            samples
                .iter()
                .any(|sample| sample.node == "count" && sample.value == Value::State(2.0))
        );
        assert_eq!(backend.delivered.len(), 1);
        assert!(matches!(
            backend.delivered[0].1,
            Quantized::Key {
                key: 67,
                pulse: true,
                ..
            }
        ));
        assert_eq!(backend.delivered[0].0.origin, origin);
        assert!(runtime.monitor_snapshot(Actor::Agent(2)).samples.is_empty());
        assert!(
            !serde_json::to_string(&runtime.snapshot())
                .unwrap()
                .contains("private-result")
        );
        assert!(
            !serde_json::to_string(runtime.compiled.definition())
                .unwrap()
                .contains("private-result")
        );
        runtime
            .action_at(origin, wheel(&runtime, 122), now, &mut backend)
            .unwrap();
        runtime.poll_timers(now, &mut backend).unwrap();
        control.borrow_mut().ready.push_back(Ok(ocr_result("")));
        runtime.poll_timers(now, &mut backend).unwrap();
        assert!(
            runtime
                .monitor_snapshot(Actor::Agent(1))
                .samples
                .iter()
                .any(|sample| sample.node == "text" && sample.value == Value::Text(String::new()))
        );
        assert_eq!(
            backend.delivered.len(),
            1,
            "empty OCR clears predicate without replaying a key pulse"
        );
        assert!(runtime.snapshot().armed);
    }
    #[test]
    fn ocr_denied_at_admission_and_revoked_at_return_publish_neither_text_nor_action() {
        for denied_at_admission in [true, false] {
            let (mut runtime, control) = capture_runtime();
            let mut backend = MockBackend::default();
            apply_arm(&mut runtime, ocr_graph(), &mut backend);
            let now = controlled_now(&runtime);
            backend.deny_capture = denied_at_admission;
            let result = runtime.action_at(
                ProgramOrigin::Agent(1, 0),
                wheel(&runtime, 120),
                now,
                &mut backend,
            );
            if denied_at_admission {
                assert!(result.is_err());
                assert!(backend.originals.is_empty() && control.borrow().starts.is_empty());
            } else {
                result.unwrap();
                runtime.poll_timers(now, &mut backend).unwrap();
                backend.deny_capture = true;
                control
                    .borrow_mut()
                    .ready
                    .push_back(Ok(ocr_result("READY private-result")));
                assert!(runtime.poll_timers(now, &mut backend).is_err());
                assert!(!runtime.snapshot().armed);
            }
            assert!(backend.delivered.is_empty());
            assert!(
                !runtime
                    .monitor_snapshot(Actor::Agent(1))
                    .samples
                    .iter()
                    .any(|sample| sample.node == "text")
            );
            assert!(
                !serde_json::to_string(&runtime.snapshot())
                    .unwrap()
                    .contains("private-result")
            );
        }
    }
    #[test]
    fn ocr_predicate_hold_clears_on_empty_text_and_failed_completion_releases_saved_claim() {
        for fail_completion_reserve in [false, true] {
            let (mut runtime, control) = capture_runtime();
            let mut backend = MockBackend::default();
            let mut graph = ocr_graph();
            let sink = graph
                .nodes
                .iter_mut()
                .find(|node| node.id == "sink")
                .unwrap();
            sink.kind = mapper::NodeKind::Sink {
                user: "c".into(),
                channel: OutputChannel::KeyHold { key: 67 },
            };
            graph
                .edges
                .iter_mut()
                .find(|edge| edge.to.node == "sink")
                .unwrap()
                .from
                .node = "contains".into();
            apply_arm(&mut runtime, graph, &mut backend);
            let now = controlled_now(&runtime);
            let origin = ProgramOrigin::Agent(1, 0);
            runtime
                .action_at(origin, wheel(&runtime, 120), now, &mut backend)
                .unwrap();
            runtime.poll_timers(now, &mut backend).unwrap();
            control
                .borrow_mut()
                .ready
                .push_back(Ok(ocr_result("READY")));
            runtime.poll_timers(now, &mut backend).unwrap();
            assert_eq!(backend.holds.len(), 1);
            assert_eq!(backend.holds.keys().next().unwrap().origin, origin);
            runtime
                .action_at(origin, wheel(&runtime, 121), now, &mut backend)
                .unwrap();
            runtime.poll_timers(now, &mut backend).unwrap();
            backend.deny_reserve = fail_completion_reserve;
            control.borrow_mut().ready.push_back(Ok(ocr_result("")));
            let result = runtime.poll_timers(now, &mut backend);
            assert_eq!(result.is_err(), fail_completion_reserve);
            assert!(
                backend.holds.is_empty(),
                "empty text and failed wave both release saved observation claims"
            );
            assert_eq!(runtime.snapshot().armed, !fail_completion_reserve);
            assert_eq!(runtime.snapshot().pending_observations, 0);
        }
    }
    #[test]
    fn ocr_retired_actor_does_not_publish_a_late_text_continuation_to_reused_context() {
        let (mut runtime, control) = capture_runtime();
        let mut backend = MockBackend::default();
        apply_arm(&mut runtime, ocr_graph(), &mut backend);
        let now = controlled_now(&runtime);
        runtime
            .action_at(
                ProgramOrigin::Manual(0),
                wheel(&runtime, 120),
                now,
                &mut backend,
            )
            .unwrap();
        runtime.poll_timers(now, &mut backend).unwrap();
        runtime.retire_actor(Actor::Manual, &mut backend).unwrap();
        runtime
            .action_at(
                ProgramOrigin::Manual(0),
                wheel(&runtime, 121),
                now,
                &mut backend,
            )
            .unwrap();
        control
            .borrow_mut()
            .ready
            .push_back(Ok(ocr_result("READY stale-private")));
        runtime.poll_timers(now, &mut backend).unwrap();
        assert!(backend.delivered.is_empty());
        assert_eq!(runtime.snapshot().observation_completions, 0);
        assert!(
            !runtime
                .monitor_snapshot(Actor::Manual)
                .samples
                .iter()
                .any(|sample| sample.node == "text")
        );
        runtime.poll_timers(now, &mut backend).unwrap();
        control
            .borrow_mut()
            .ready
            .push_back(Ok(ocr_result("READY fresh-private")));
        runtime.poll_timers(now, &mut backend).unwrap();
        assert_eq!(backend.delivered.len(), 1);
        assert!(control.borrow().cancellations > 0);
    }
    fn pixel_result(red: u8) -> mapper::ObservationResult {
        mapper::ObservationResult::PixelColor {
            rgba: [red, 2, 3, 255],
        }
    }
    fn found() -> mapper::ObservationResult {
        mapper::ObservationResult::ImageMatch {
            matched: Some(mapper::ObservationMatch {
                desktop_center: [10.5, 20.5],
                score: 1.0,
            }),
        }
    }

    #[test]
    fn observations_commit_after_admission_complete_current_state_and_keep_private_samples() {
        let (mut runtime, control) = capture_runtime();
        let mut backend = MockBackend::default();
        apply_arm(&mut runtime, pixel_graph(), &mut backend);
        assert!(
            control.borrow().starts.is_empty(),
            "Apply and Arm never capture"
        );
        let now = controlled_now(&runtime);
        let origin = ProgramOrigin::Agent(1, 0);
        runtime
            .action_at(origin, wheel(&runtime, 120), now, &mut backend)
            .unwrap();
        assert_eq!(runtime.snapshot().pending_observations, 1);
        assert!(control.borrow().starts.is_empty());
        assert!(backend.delivered.is_empty());
        assert_eq!(backend.capture_checks[0], (origin, 0, 1, vec![0]));
        runtime.poll_timers(now, &mut backend).unwrap();
        assert_eq!(control.borrow().starts.len(), 1);
        runtime
            .action_at(origin, wheel(&runtime, 121), now, &mut backend)
            .unwrap();
        control.borrow_mut().ready.push_back(Ok(pixel_result(44)));
        runtime.poll_timers(now, &mut backend).unwrap();
        let samples = runtime.monitor_snapshot(Actor::Agent(1)).samples;
        assert!(
            samples
                .iter()
                .any(|s| s.node == "red" && s.value == Value::State(44.0))
        );
        assert!(
            samples
                .iter()
                .any(|s| s.node == "count" && s.value == Value::State(2.0)),
            "completion must not restore stale counter1"
        );
        assert!(runtime.monitor_snapshot(Actor::Agent(2)).samples.is_empty());
        assert_eq!(runtime.snapshot().pending_observations, 0);
        assert_eq!(runtime.snapshot().observation_completions, 1);
        assert_eq!(runtime.snapshot().dropped_observations, 1);
        assert!(
            backend
                .delivered
                .iter()
                .all(|(owner, _)| owner.origin == origin && owner.source_users == vec![0])
        );
        let telemetry = serde_json::to_value(runtime.snapshot()).unwrap();
        assert!(
            telemetry.get("samples").is_none()
                && telemetry.get("frames").is_none()
                && telemetry.get("observations").is_none()
        );
    }

    #[test]
    fn observation_return_rechecks_capture_grant_generation_window_lifetime_and_sink_receipt() {
        for mode in 0..6 {
            let (mut runtime, control) = capture_runtime();
            let mut backend = MockBackend::default();
            apply_arm(&mut runtime, pixel_graph(), &mut backend);
            let now = controlled_now(&runtime);
            runtime
                .action_at(
                    ProgramOrigin::Agent(1, 0),
                    wheel(&runtime, 120),
                    now,
                    &mut backend,
                )
                .unwrap();
            runtime.poll_timers(now, &mut backend).unwrap();
            match mode {
                0 => backend.deny_capture = true,
                1 => backend.capture_generation += 1,
                2 => backend.capture_window_offset += 1,
                3 => backend.expired_watch = true,
                4 => backend.recipient_offset += 1,
                _ => backend.deny_exact_timer_recipient = true,
            }
            control.borrow_mut().ready.push_back(Ok(pixel_result(99)));
            assert!(
                runtime.poll_timers(now, &mut backend).is_err(),
                "guard mode{mode}"
            );
            assert!(!runtime.snapshot().armed && runtime.snapshot().pending_observations == 0);
            assert!(
                backend.delivered.is_empty()
                    && runtime.monitor_snapshot(Actor::Agent(1)).samples.is_empty()
            );
        }
    }

    #[test]
    fn observation_eof_and_recreated_actor_core_id_cannot_accept_a_late_result() {
        let (mut runtime, control) = capture_runtime();
        let mut backend = MockBackend::default();
        apply_arm(&mut runtime, pixel_graph(), &mut backend);
        let now = controlled_now(&runtime);
        runtime
            .action_at(
                ProgramOrigin::Manual(0),
                wheel(&runtime, 120),
                now,
                &mut backend,
            )
            .unwrap();
        runtime.poll_timers(now, &mut backend).unwrap();
        runtime.retire_actor(Actor::Manual, &mut backend).unwrap();
        runtime
            .action_at(
                ProgramOrigin::Manual(0),
                wheel(&runtime, 121),
                now,
                &mut backend,
            )
            .unwrap();
        control.borrow_mut().ready.push_back(Ok(pixel_result(12)));
        runtime.poll_timers(now, &mut backend).unwrap();
        assert_eq!(runtime.snapshot().observation_completions, 0);
        assert!(backend.delivered.is_empty());
        assert!(
            !runtime
                .monitor_snapshot(Actor::Manual)
                .samples
                .iter()
                .any(|s| s.node == "red")
        );
        runtime.poll_timers(now, &mut backend).unwrap();
        assert_eq!(control.borrow().starts.len(), 2);
        control.borrow_mut().ready.push_back(Ok(pixel_result(77)));
        runtime.poll_timers(now, &mut backend).unwrap();
        assert!(
            runtime
                .monitor_snapshot(Actor::Manual)
                .samples
                .iter()
                .any(|s| s.node == "red" && s.value == Value::State(77.0))
        );
        assert!(control.borrow().cancellations > 0);
    }

    #[test]
    fn canceled_actor_physical_timeout_does_not_leave_peer_jobs_waiting_forever() {
        let (mut runtime, _control) = capture_runtime();
        let mut backend = MockBackend::default();
        apply_arm(&mut runtime, pixel_graph(), &mut backend);
        let now = controlled_now(&runtime);
        for actor in [1, 2] {
            runtime
                .action_at(
                    ProgramOrigin::Agent(actor, 0),
                    wheel(&runtime, 120),
                    now,
                    &mut backend,
                )
                .unwrap();
        }
        runtime.poll_timers(now, &mut backend).unwrap();
        runtime.retire_actor(Actor::Agent(1), &mut backend).unwrap();
        assert_eq!(runtime.snapshot().pending_observations, 1);
        assert!(
            runtime
                .poll_timers(now + Duration::from_secs(6), &mut backend)
                .is_err()
        );
        assert!(!runtime.snapshot().armed && runtime.snapshot().pending_observations == 0);
        assert!(runtime.snapshot().capture_worker_busy && runtime.observation_task.is_some());
    }

    #[test]
    fn observation_timeout_keeps_physical_slot_and_rejects_arm_apply_until_it_drains() {
        let (mut runtime, control) = capture_runtime();
        let mut backend = MockBackend::default();
        apply_arm(&mut runtime, pixel_graph(), &mut backend);
        let now = controlled_now(&runtime);
        runtime
            .action_at(
                ProgramOrigin::Agent(1, 0),
                wheel(&runtime, 120),
                now,
                &mut backend,
            )
            .unwrap();
        runtime.poll_timers(now, &mut backend).unwrap();
        assert!(
            runtime
                .poll_timers(now + Duration::from_secs(6), &mut backend)
                .is_err()
        );
        assert!(runtime.snapshot().capture_worker_busy && runtime.observation_task.is_some());
        let armed = runtime
            .command(
                Command::Arm {
                    expected_revision: 1,
                    gui: None,
                },
                &mut backend,
            )
            .unwrap();
        assert!(!armed.armed && armed.error.is_some());
        let applied = runtime
            .command(
                Command::Apply {
                    definition: pixel_graph(),
                    expected_revision: 1,
                    gui: None,
                },
                &mut backend,
            )
            .unwrap();
        assert_eq!(applied.revision, 1);
        assert!(applied.error.is_some());
        control.borrow_mut().ready.push_back(Ok(pixel_result(9)));
        runtime
            .poll_timers(now + Duration::from_secs(6), &mut backend)
            .unwrap();
        assert!(
            !runtime.snapshot().capture_worker_busy
                && runtime.snapshot().observation_completions == 0
        );
        let armed = runtime
            .command(
                Command::Arm {
                    expected_revision: 1,
                    gui: None,
                },
                &mut backend,
            )
            .unwrap();
        assert!(armed.armed);
        let later = controlled_now(&runtime);
        runtime
            .action_at(
                ProgramOrigin::Agent(2, 0),
                wheel(&runtime, 121),
                later,
                &mut backend,
            )
            .unwrap();
        runtime.poll_timers(later, &mut backend).unwrap();
        assert_eq!(control.borrow().starts.len(), 2);
    }

    #[test]
    fn observation_burst_drops_new_requests_without_dropping_original_input_or_other_state() {
        let (mut runtime, control) = capture_runtime();
        let mut backend = MockBackend::default();
        let mut definition = pixel_graph();
        let mapper::NodeKind::Wheel { disposition, .. } = &mut definition.nodes[0].kind else {
            panic!("source")
        };
        *disposition = Disposition::Passthrough;
        apply_arm(&mut runtime, definition, &mut backend);
        let now = controlled_now(&runtime);
        backend.deny_reserve = true;
        assert!(
            runtime
                .action_at(
                    ProgramOrigin::Agent(1, 0),
                    wheel(&runtime, 120),
                    now,
                    &mut backend
                )
                .is_err()
        );
        assert_eq!(runtime.snapshot().pending_observations, 0);
        assert_eq!(runtime.snapshot().dropped_observations, 0);
        backend.deny_reserve = false;
        for _ in 0..125 {
            runtime
                .action_at(
                    ProgramOrigin::Agent(1, 0),
                    wheel(&runtime, 120),
                    now,
                    &mut backend,
                )
                .unwrap();
        }
        assert!(runtime.snapshot().armed);
        assert_eq!(runtime.snapshot().pending_observations, 1);
        assert_eq!(runtime.snapshot().dropped_observations, 124);
        assert!(control.borrow().starts.is_empty());
        assert_eq!(backend.originals.len(), 125);
        assert!(
            backend
                .originals
                .iter()
                .all(|(_, disposition)| *disposition == Disposition::Passthrough)
        );
        assert!(
            runtime
                .monitor_snapshot(Actor::Agent(1))
                .samples
                .iter()
                .any(|s| s.node == "count" && s.value == Value::State(125.0))
        );
        runtime.poll_timers(now, &mut backend).unwrap();
        for _ in 0..125 {
            runtime
                .action_at(
                    ProgramOrigin::Agent(1, 0),
                    wheel(&runtime, 121),
                    now,
                    &mut backend,
                )
                .unwrap();
        }
        assert_eq!(runtime.snapshot().dropped_observations, 249);
        assert_eq!(control.borrow().starts.len(), 1);
    }

    #[test]
    fn observation_session_saturation_drops_only_new_jobs_and_preserves_all_actors() {
        let mut definition = pixel_graph();
        for index in 1..8 {
            let id = format!("extra-{index}");
            definition.nodes.push(mapper::Node {
                id: id.clone(),
                kind: mapper::NodeKind::PixelColor {
                    capture_user: "b".into(),
                    x: 0,
                    y: 0,
                },
                position: None,
            });
            definition.edges.push(serde_json::from_value(serde_json::json!({"from":{"node":"pulse","port":"value"},"to":{"node":id,"port":"trigger"}})).unwrap());
        }
        let (mut runtime, control) = capture_runtime();
        let mut backend = MockBackend::default();
        apply_arm(&mut runtime, definition, &mut backend);
        let now = controlled_now(&runtime);
        for actor in 1..=5 {
            runtime
                .action_at(
                    ProgramOrigin::Agent(actor, 0),
                    wheel(&runtime, 120),
                    now,
                    &mut backend,
                )
                .unwrap();
        }
        assert!(runtime.snapshot().armed);
        assert_eq!(runtime.snapshot().pending_observations, 32);
        assert_eq!(runtime.snapshot().dropped_observations, 8);
        assert_eq!(backend.originals.len(), 5);
        assert_eq!(runtime.snapshot().actor_contexts, 5);
        assert!(control.borrow().starts.is_empty());
        assert!(
            runtime
                .monitor_snapshot(Actor::Agent(5))
                .samples
                .iter()
                .any(|s| s.node == "count" && s.value == Value::State(1.0))
        );
        assert!(backend.delivered.is_empty());
    }

    #[test]
    fn a_physical_worker_owned_by_an_older_runtime_blocks_apply_and_arm() {
        let (mut runtime, control) = capture_runtime();
        let mut backend = MockBackend::default();
        control.borrow_mut().provider_busy = true;
        let rejected = runtime
            .command(
                Command::Apply {
                    definition: pixel_graph(),
                    expected_revision: 0,
                    gui: None,
                },
                &mut backend,
            )
            .unwrap();
        assert!(rejected.error.is_some() && rejected.revision == 0 && rejected.capture_worker_busy);
        assert!(runtime.queries.is_empty());
        control.borrow_mut().provider_busy = false;
        apply_arm(&mut runtime, pixel_graph(), &mut backend);
        runtime.retire(&mut backend).unwrap();
        control.borrow_mut().provider_busy = true;
        let rejected = runtime
            .command(
                Command::Arm {
                    expected_revision: 1,
                    gui: None,
                },
                &mut backend,
            )
            .unwrap();
        assert!(!rejected.armed && rejected.error.is_some() && rejected.capture_worker_busy);
        control.borrow_mut().provider_busy = false;
        assert!(
            runtime
                .command(
                    Command::Arm {
                        expected_revision: 1,
                        gui: None
                    },
                    &mut backend
                )
                .unwrap()
                .armed
        );
        assert!(control.borrow().starts.is_empty());
    }

    #[test]
    fn observation_queue_wait_is_not_a_capture_timeout() {
        let (mut runtime, control) = capture_runtime();
        let mut backend = MockBackend::default();
        apply_arm(&mut runtime, pixel_graph(), &mut backend);
        let now = controlled_now(&runtime);
        runtime
            .action_at(
                ProgramOrigin::Agent(1, 0),
                wheel(&runtime, 120),
                now,
                &mut backend,
            )
            .unwrap();
        runtime
            .poll_timers(now + Duration::from_secs(20), &mut backend)
            .unwrap();
        assert_eq!(control.borrow().starts.len(), 1);
        assert!(runtime.snapshot().armed);
        control.borrow_mut().ready.push_back(Ok(pixel_result(55)));
        runtime
            .poll_timers(now + Duration::from_secs(24), &mut backend)
            .unwrap();
        assert_eq!(runtime.snapshot().observation_completions, 1);
    }

    #[test]
    fn observation_drop_new_preserves_an_existing_matched_hold_until_real_completion_or_eof() {
        let (mut runtime, control) = capture_runtime();
        let mut backend = MockBackend::default();
        apply_arm(&mut runtime, match_graph(), &mut backend);
        let now = controlled_now(&runtime);
        let origin = ProgramOrigin::Agent(1, 0);
        runtime
            .action_at(origin, wheel(&runtime, 120), now, &mut backend)
            .unwrap();
        runtime.poll_timers(now, &mut backend).unwrap();
        control.borrow_mut().ready.push_back(Ok(found()));
        runtime.poll_timers(now, &mut backend).unwrap();
        assert_eq!(backend.holds.len(), 1);
        for _ in 0..125 {
            runtime
                .action_at(origin, wheel(&runtime, 121), now, &mut backend)
                .unwrap();
        }
        assert!(runtime.snapshot().armed && backend.holds.len() == 1);
        assert_eq!(runtime.snapshot().pending_observations, 1);
        assert_eq!(runtime.snapshot().dropped_observations, 124);
        runtime.retire_actor(Actor::Agent(1), &mut backend).unwrap();
        assert!(backend.holds.is_empty());
    }

    #[test]
    fn observation_no_match_releases_original_hold_and_errors_or_eof_release_saved_claims() {
        for failure in 0..5 {
            let (mut runtime, control) = capture_runtime();
            let mut backend = MockBackend::default();
            apply_arm(&mut runtime, match_graph(), &mut backend);
            let now = controlled_now(&runtime);
            let origin = ProgramOrigin::Agent(1, 0);
            runtime
                .action_at(origin, wheel(&runtime, 120), now, &mut backend)
                .unwrap();
            runtime.poll_timers(now, &mut backend).unwrap();
            control.borrow_mut().ready.push_back(Ok(found()));
            runtime.poll_timers(now, &mut backend).unwrap();
            assert_eq!(backend.holds.len(), 1);
            if failure == 3 {
                runtime.retire_actor(Actor::Agent(1), &mut backend).unwrap();
            } else if failure == 4 {
                backend.deny_capture = true;
                assert!(
                    runtime
                        .action_at(origin, wheel(&runtime, 121), now, &mut backend)
                        .is_err()
                );
                assert!(!runtime.snapshot().armed);
            } else {
                runtime
                    .action_at(origin, wheel(&runtime, 121), now, &mut backend)
                    .unwrap();
                runtime.poll_timers(now, &mut backend).unwrap();
                if failure == 1 {
                    control
                        .borrow_mut()
                        .ready
                        .push_back(Err(anyhow::anyhow!("controlled capture failure")));
                } else {
                    control
                        .borrow_mut()
                        .ready
                        .push_back(Ok(mapper::ObservationResult::ImageMatch { matched: None }));
                }
                if failure == 2 {
                    backend.deny_reserve = true;
                }
                let result = runtime.poll_timers(now, &mut backend);
                if failure == 0 {
                    result.unwrap();
                    assert!(runtime.snapshot().armed);
                    assert!(
                        runtime
                            .monitor_snapshot(Actor::Agent(1))
                            .samples
                            .iter()
                            .any(|s| s.node == "score" && s.value == Value::State(0.0))
                    );
                } else {
                    assert!(result.is_err() && !runtime.snapshot().armed);
                }
            }
            assert!(
                backend.holds.is_empty(),
                "failure{failure} retained held match output"
            );
            assert!(
                backend
                    .delivered
                    .iter()
                    .all(|(owner, _)| owner.origin == origin)
            );
        }
    }

    #[test]
    fn observation_never_dispatches_if_original_or_immediate_native_effect_fails() {
        for original in [false, true] {
            let (mut runtime, control) = capture_runtime();
            let mut backend = MockBackend::default();
            let mut definition = pixel_graph();
            definition.nodes.push(serde_json::from_value(serde_json::json!({"id":"immediate","kind":{"type":"sink","user":"c","channel":{"type":"key_pulse","key":66}}})).unwrap());
            definition.edges.push(serde_json::from_value(serde_json::json!({"from":{"node":"pulse","port":"value"},"to":{"node":"immediate","port":"value"}})).unwrap());
            apply_arm(&mut runtime, definition, &mut backend);
            let now = controlled_now(&runtime);
            if original {
                backend.fail_original = true;
            } else {
                backend.fail_delivery = Some(1);
            }
            assert!(
                runtime
                    .action_at(
                        ProgramOrigin::Agent(1, 0),
                        wheel(&runtime, 120),
                        now,
                        &mut backend
                    )
                    .is_err()
            );
            assert!(!runtime.snapshot().armed && runtime.snapshot().pending_observations == 0);
            assert!(runtime.monitor_snapshot(Actor::Agent(1)).samples.is_empty());
            runtime.poll_timers(now, &mut backend).unwrap();
            assert!(control.borrow().starts.is_empty());
        }
    }

    #[test]
    fn observation_partial_native_failure_retains_failed_release_and_retries_only_saved_claims() {
        let (mut runtime, control) = capture_runtime();
        let mut backend = MockBackend::default();
        apply_arm(&mut runtime, pixel_graph(), &mut backend);
        let now = controlled_now(&runtime);
        runtime
            .action_at(
                ProgramOrigin::Agent(1, 0),
                wheel(&runtime, 120),
                now,
                &mut backend,
            )
            .unwrap();
        runtime.poll_timers(now, &mut backend).unwrap();
        control.borrow_mut().ready.push_back(Ok(pixel_result(1)));
        backend.fail_timer_pulse_release = true;
        backend.fail_release = true;
        assert!(runtime.poll_timers(now, &mut backend).is_err());
        assert!(
            !runtime.snapshot().armed
                && runtime.snapshot().cleanup_pending
                && backend.holds.len() == 1
        );
        assert_eq!(runtime.snapshot().pending_observations, 0);
        let starts = control.borrow().starts.len();
        backend.fail_release = false;
        runtime.last_cleanup = Some(Instant::now() - Duration::from_secs(1));
        runtime.poll_cleanup(&mut backend).unwrap();
        assert!(backend.holds.is_empty() && !runtime.snapshot().cleanup_pending);
        assert_eq!(
            control.borrow().starts.len(),
            starts,
            "cleanup must never replay a capture"
        );
    }

    #[test]
    fn malformed_observation_template_apply_keeps_previous_graph_cache_and_revision() {
        let (mut runtime, control) = capture_runtime();
        let mut backend = MockBackend::default();
        let good = match_graph();
        let applied = runtime
            .command(
                Command::Apply {
                    definition: good.clone(),
                    expected_revision: 0,
                    gui: None,
                },
                &mut backend,
            )
            .unwrap();
        assert!(applied.error.is_none() && applied.revision == 1);
        assert_eq!(runtime.queries.len(), 1);
        let mut invalid = good.clone();
        let mapper::NodeKind::ImageMatch {
            template_png_base64,
            ..
        } = &mut invalid.nodes[2].kind
        else {
            panic!("missing match")
        };
        *template_png_base64 = "bm90LWEtcG5n".into();
        let rejected = runtime
            .command(
                Command::Apply {
                    definition: invalid,
                    expected_revision: 1,
                    gui: None,
                },
                &mut backend,
            )
            .unwrap();
        assert!(rejected.error.is_some() && rejected.revision == 1 && rejected.definition == good);
        assert_eq!(runtime.queries.len(), 1);
        assert!(control.borrow().starts.is_empty());
    }

    #[test]
    fn actor_eof_preserves_peer_observations_and_capture_user_removal_retires_them() {
        let (mut runtime, control) = capture_runtime();
        let mut backend = MockBackend::default();
        apply_arm(&mut runtime, pixel_graph(), &mut backend);
        let now = controlled_now(&runtime);
        for actor in [1, 2] {
            runtime
                .action_at(
                    ProgramOrigin::Agent(actor, 0),
                    wheel(&runtime, 120),
                    now,
                    &mut backend,
                )
                .unwrap();
        }
        runtime.poll_timers(now, &mut backend).unwrap();
        runtime.retire_actor(Actor::Agent(1), &mut backend).unwrap();
        assert!(runtime.snapshot().armed && runtime.snapshot().pending_observations == 1);
        control.borrow_mut().ready.push_back(Ok(pixel_result(11)));
        runtime.poll_timers(now, &mut backend).unwrap();
        assert!(backend.delivered.is_empty());
        runtime.poll_timers(now, &mut backend).unwrap();
        control.borrow_mut().ready.push_back(Ok(pixel_result(22)));
        runtime.poll_timers(now, &mut backend).unwrap();
        assert!(runtime.monitor_snapshot(Actor::Agent(1)).samples.is_empty());
        assert!(
            runtime
                .monitor_snapshot(Actor::Agent(2))
                .samples
                .iter()
                .any(|s| s.node == "red" && s.value == Value::State(22.0))
        );
        runtime
            .action_at(
                ProgramOrigin::Agent(2, 0),
                wheel(&runtime, 120),
                now,
                &mut backend,
            )
            .unwrap();
        runtime.prepare_remove_user(1, &mut backend).unwrap();
        assert!(
            !runtime.snapshot().armed && runtime.snapshot().pending_observations == 0,
            "capture-only user reference must retire queued work before removal"
        );
    }

    #[test]
    fn observation_to_delay_and_older_delay_to_observation_preserve_wave_origin_and_order() {
        let mut definition = pixel_graph();
        definition.nodes.push(mapper::Node {
            id: "delay".into(),
            kind: mapper::NodeKind::Delay {
                signal: mapper::SignalType::Pulse,
                delay_ms: 100,
            },
            position: None,
        });
        let sink = definition
            .edges
            .iter_mut()
            .find(|edge| edge.to.node == "sink")
            .unwrap();
        sink.from.node = "delay".into();
        sink.from.port = "value".into();
        definition.edges.push(serde_json::from_value(serde_json::json!({"from":{"node":"pixel","port":"done"},"to":{"node":"delay","port":"value"}})).unwrap());
        let (mut runtime, control) = capture_runtime();
        let mut backend = MockBackend::default();
        apply_arm(&mut runtime, definition, &mut backend);
        let now = controlled_now(&runtime);
        let origin = ProgramOrigin::Agent(1, 0);
        runtime
            .action_at(origin, wheel(&runtime, 120), now, &mut backend)
            .unwrap();
        runtime.poll_timers(now, &mut backend).unwrap();
        control.borrow_mut().ready.push_back(Ok(pixel_result(1)));
        runtime.poll_timers(now, &mut backend).unwrap();
        assert_eq!(runtime.snapshot().pending_delays, 1);
        assert!(backend.delivered.is_empty());
        // A later capture must not starve the already-due Delay wave.
        runtime
            .action_at(
                origin,
                wheel(&runtime, 121),
                now + Duration::from_millis(101),
                &mut backend,
            )
            .unwrap();
        runtime
            .poll_timers(now + Duration::from_millis(101), &mut backend)
            .unwrap();
        assert_eq!(backend.delivered.len(), 1);
        assert_eq!(backend.delivered[0].0.origin, origin);
        assert_eq!(control.borrow().starts.len(), 1);
        runtime
            .poll_timers(now + Duration::from_millis(101), &mut backend)
            .unwrap();
        assert_eq!(control.borrow().starts.len(), 2);
    }

    #[test]
    fn throttle_admission_is_actor_private_and_failed_reservation_does_not_take_quota() {
        let mut definition = delay_wheel_graph();
        definition.nodes[1].kind = mapper::NodeKind::Throttle {
            signal: mapper::SignalType::Impulse,
            interval_ms: 100,
        };
        let (mut runtime, mut backend) = (runtime(), MockBackend::default());
        apply_arm(&mut runtime, definition, &mut backend);
        let now = controlled_now(&runtime);
        let first = ProgramOrigin::Agent(1, 0);
        backend.deny_reserve = true;
        assert!(
            runtime
                .action_at(first, wheel(&runtime, 120), now, &mut backend)
                .is_err()
        );
        assert_eq!(runtime.snapshot().actor_contexts, 0);
        assert!(backend.delivered.is_empty());
        backend.deny_reserve = false;
        runtime
            .action_at(first, wheel(&runtime, 120), now, &mut backend)
            .unwrap();
        runtime
            .action_at(
                first,
                wheel(&runtime, 240),
                now + Duration::from_millis(99),
                &mut backend,
            )
            .unwrap();
        assert_eq!(backend.delivered.len(), 1);
        runtime
            .action_at(
                ProgramOrigin::Agent(2, 0),
                wheel(&runtime, 360),
                now + Duration::from_millis(99),
                &mut backend,
            )
            .unwrap();
        assert_eq!(backend.delivered.len(), 2);
        runtime
            .action_at(
                first,
                wheel(&runtime, 480),
                now + Duration::from_millis(100),
                &mut backend,
            )
            .unwrap();
        assert_eq!(backend.delivered.len(), 3);
        assert_eq!(
            backend.delivered[2].1,
            Quantized::Wheel {
                horizontal: false,
                delta: 480
            }
        );
        runtime
            .poll_timers(now + Duration::from_secs(10), &mut backend)
            .unwrap();
        assert_eq!(
            backend.delivered.len(),
            3,
            "throttling has no trailing replay"
        );
        assert_eq!(runtime.snapshot().pending_delays, 0);
    }

    #[test]
    fn delayed_impulses_are_actor_private_ordered_and_one_per_worker_turn() {
        let (mut runtime, mut backend) = (runtime(), MockBackend::default());
        apply_arm(&mut runtime, delay_wheel_graph(), &mut backend);
        let now = controlled_now(&runtime);
        for (origin, value) in [
            (ProgramOrigin::Agent(1, 0), 120),
            (ProgramOrigin::Agent(1, 0), 240),
            (ProgramOrigin::Agent(2, 0), 360),
        ] {
            runtime
                .action_at(origin, wheel(&runtime, value), now, &mut backend)
                .unwrap();
        }
        assert_eq!(runtime.snapshot().pending_delays, 3);
        assert!(backend.delivered.is_empty());
        assert!(runtime.monitor_snapshot(Actor::Agent(1)).samples.is_empty());
        assert!(runtime.monitor_snapshot(Actor::Agent(2)).samples.is_empty());
        let deadline = now + Duration::from_millis(100);
        runtime
            .poll_timers(deadline - Duration::from_micros(1), &mut backend)
            .unwrap();
        assert!(backend.delivered.is_empty());
        for (index, value) in [120, 240, 360].into_iter().enumerate() {
            runtime.poll_timers(deadline, &mut backend).unwrap();
            assert_eq!(backend.delivered.len(), index + 1);
            assert_eq!(
                backend.delivered[index].1,
                Quantized::Wheel {
                    horizontal: false,
                    delta: value
                }
            );
        }
        assert_eq!(runtime.snapshot().pending_delays, 0);
        assert_eq!(runtime.snapshot().delayed_events, 3);
        assert_eq!(
            backend.originals.len(),
            3,
            "scheduled work must not reenter raw input"
        );
        assert_eq!(
            runtime.monitor_snapshot(Actor::Agent(1)).samples[0].value,
            Value::Impulse(240.0)
        );
        assert_eq!(
            runtime.monitor_snapshot(Actor::Agent(2)).samples[0].value,
            Value::Impulse(360.0)
        );
        runtime
            .poll_timers(deadline + Duration::from_secs(10), &mut backend)
            .unwrap();
        assert_eq!(
            backend.delivered.len(),
            3,
            "captured impulses are never replayed"
        );
        let telemetry = serde_json::to_value(runtime.snapshot()).unwrap();
        assert!(telemetry.get("samples").is_none() && telemetry.get("delays").is_none());
    }

    #[test]
    fn delayed_hold_down_up_keeps_original_owner_and_cancellation_releases_only_saved_claims() {
        let (mut runtime, mut backend) = (runtime(), MockBackend::default());
        apply_arm(&mut runtime, delay_hold_graph(), &mut backend);
        let now = controlled_now(&runtime);
        let origin = ProgramOrigin::Agent(1, 0);
        runtime
            .action_at(origin, key(&runtime, 0, 65, true), now, &mut backend)
            .unwrap();
        runtime
            .action_at(origin, key(&runtime, 0, 65, false), now, &mut backend)
            .unwrap();
        runtime
            .poll_timers(now + Duration::from_millis(100), &mut backend)
            .unwrap();
        assert_eq!(backend.holds.len(), 1);
        assert_eq!(runtime.snapshot().pending_delays, 1);
        runtime
            .poll_timers(now + Duration::from_millis(100), &mut backend)
            .unwrap();
        assert!(backend.holds.is_empty());
        assert_eq!(backend.delivered.len(), 2);
        assert_eq!(backend.delivered[0].0, backend.delivered[1].0);
        assert_eq!(backend.delivered[1].0.origin, origin);
        assert!(backend.delivered[1].1.release());
        // A new delayed DOWN is canceled before it can actuate on client EOF.
        runtime
            .action_at(origin, key(&runtime, 0, 65, true), now, &mut backend)
            .unwrap();
        runtime.retire_actor(Actor::Agent(1), &mut backend).unwrap();
        runtime
            .poll_timers(now + Duration::from_secs(10), &mut backend)
            .unwrap();
        assert_eq!(backend.delivered.len(), 2);
        assert_eq!(runtime.snapshot().pending_delays, 0);
        assert!(runtime.is_armed());
    }

    #[test]
    fn disconnecting_one_delayed_actor_preserves_the_other_actors_queue() {
        let (mut runtime, mut backend) = (runtime(), MockBackend::default());
        apply_arm(&mut runtime, delay_wheel_graph(), &mut backend);
        let now = controlled_now(&runtime);
        for client in [1, 2] {
            runtime
                .action_at(
                    ProgramOrigin::Agent(client, 0),
                    wheel(&runtime, 120),
                    now,
                    &mut backend,
                )
                .unwrap();
        }
        runtime.retire_actor(Actor::Agent(1), &mut backend).unwrap();
        assert_eq!(runtime.snapshot().pending_delays, 1);
        runtime
            .poll_timers(now + Duration::from_millis(100), &mut backend)
            .unwrap();
        assert_eq!(backend.delivered.len(), 1);
        assert_eq!(backend.delivered[0].0.origin, ProgramOrigin::Agent(2, 0));
        assert!(runtime.monitor_snapshot(Actor::Agent(1)).samples.is_empty());
        assert_eq!(runtime.monitor_snapshot(Actor::Agent(2)).samples.len(), 1);
        assert!(runtime.is_armed());
    }

    #[test]
    fn delayed_admission_rechecks_grants_recipient_lifetime_and_exact_sink() {
        for failure in 0..4 {
            let (mut runtime, mut backend) = (runtime(), MockBackend::default());
            apply_arm(&mut runtime, delay_wheel_graph(), &mut backend);
            let now = controlled_now(&runtime);
            runtime
                .action_at(
                    ProgramOrigin::Agent(1, 0),
                    wheel(&runtime, 120),
                    now,
                    &mut backend,
                )
                .unwrap();
            match failure {
                0 => backend.deny_authorization = true,
                1 => backend.recipient_offset = 1,
                2 => backend.expired_watch = true,
                3 => backend.deny_exact_timer_recipient = true,
                _ => unreachable!(),
            }
            assert!(
                runtime
                    .poll_timers(now + Duration::from_millis(100), &mut backend)
                    .is_err()
            );
            assert!(!runtime.is_armed());
            assert_eq!(runtime.snapshot().pending_delays, 0);
            assert!(backend.delivered.is_empty());
            assert!(runtime.monitor_snapshot(Actor::Agent(1)).samples.is_empty());
        }
    }

    #[test]
    fn delay_prepare_reserve_failure_commits_no_queue_or_samples_and_disarm_cancels_all() {
        let (mut runtime, mut backend) = (runtime(), MockBackend::default());
        apply_arm(&mut runtime, delay_wheel_graph(), &mut backend);
        let now = controlled_now(&runtime);
        backend.deny_reserve = true;
        assert!(
            runtime
                .action_at(
                    ProgramOrigin::Agent(1, 0),
                    wheel(&runtime, 120),
                    now,
                    &mut backend
                )
                .is_err()
        );
        assert!(runtime.is_armed());
        assert_eq!(runtime.snapshot().pending_delays, 0);
        assert_eq!(runtime.snapshot().actor_contexts, 0);
        assert!(backend.originals.is_empty() && backend.delivered.is_empty());
        backend.deny_reserve = false;
        for client in [1, 2] {
            runtime
                .action_at(
                    ProgramOrigin::Agent(client, 0),
                    wheel(&runtime, 120),
                    now,
                    &mut backend,
                )
                .unwrap();
        }
        let disarmed = runtime
            .command(
                Command::Disarm {
                    expected_revision: 1,
                    gui: None,
                },
                &mut backend,
            )
            .unwrap();
        assert!(!disarmed.armed && disarmed.pending_delays == 0);
        runtime
            .poll_timers(now + Duration::from_secs(10), &mut backend)
            .unwrap();
        assert!(backend.delivered.is_empty());
    }

    fn capacity_graph() -> Definition {
        let mut definition = delay_hold_graph();
        let wheel = delay_wheel_graph();
        definition
            .nodes
            .extend(wheel.nodes.into_iter().map(|mut node| {
                if node.id == "sink" {
                    node.id = "wheel-sink".into();
                }
                node
            }));
        definition
            .edges
            .extend(wheel.edges.into_iter().map(|mut edge| {
                if edge.to.node == "sink" {
                    edge.to.node = "wheel-sink".into();
                }
                edge
            }));
        definition
    }

    #[test]
    fn rejected_delayed_hold_up_at_actor_or_session_capacity_cancels_earlier_down() {
        for session_bound in [false, true] {
            let (mut runtime, mut backend) = (runtime(), MockBackend::default());
            apply_arm(&mut runtime, capacity_graph(), &mut backend);
            let now = controlled_now(&runtime);
            runtime
                .action_at(
                    ProgramOrigin::Agent(1, 0),
                    key(&runtime, 0, 65, true),
                    now,
                    &mut backend,
                )
                .unwrap();
            let fills = if session_bound {
                vec![(2, 128), (3, 127)]
            } else {
                vec![(1, 127)]
            };
            for (client, count) in fills {
                for _ in 0..count {
                    runtime
                        .action_at(
                            ProgramOrigin::Agent(client, 0),
                            wheel(&runtime, 1),
                            now,
                            &mut backend,
                        )
                        .unwrap();
                }
            }
            assert_eq!(
                runtime.snapshot().pending_delays,
                if session_bound { 256 } else { 128 }
            );
            assert!(
                runtime
                    .action_at(
                        ProgramOrigin::Agent(1, 0),
                        key(&runtime, 0, 65, false),
                        now,
                        &mut backend
                    )
                    .is_err()
            );
            assert!(!runtime.is_armed());
            assert_eq!(runtime.snapshot().pending_delays, 0);
            runtime
                .poll_timers(now + Duration::from_secs(10), &mut backend)
                .unwrap();
            assert!(backend.delivered.is_empty() && backend.holds.is_empty());
        }
    }

    #[test]
    fn rejected_up_admission_retires_pending_or_already_delivered_delayed_hold() {
        for failure in 0..3 {
            let (mut runtime, mut backend) = (runtime(), MockBackend::default());
            apply_arm(&mut runtime, delay_hold_graph(), &mut backend);
            let now = controlled_now(&runtime);
            runtime
                .action_at(
                    ProgramOrigin::Agent(1, 0),
                    key(&runtime, 0, 65, true),
                    now,
                    &mut backend,
                )
                .unwrap();
            if failure == 0 {
                runtime
                    .poll_timers(now + Duration::from_millis(100), &mut backend)
                    .unwrap();
                assert_eq!(backend.holds.len(), 1);
                backend.deny_reserve = true;
            } else if failure == 1 {
                backend.deny_watch = true;
            } else {
                backend.deny_authorization = true;
            }
            assert!(
                runtime
                    .action_at(
                        ProgramOrigin::Agent(1, 0),
                        key(&runtime, 0, 65, false),
                        now,
                        &mut backend
                    )
                    .is_err()
            );
            assert!(!runtime.is_armed());
            assert_eq!(runtime.snapshot().pending_delays, 0);
            assert!(backend.holds.is_empty());
        }
    }

    #[test]
    fn partial_delayed_native_failure_retires_generation_and_retains_failed_release() {
        let mut definition = delay_hold_graph();
        let mut second = definition.nodes.last().unwrap().clone();
        second.id = "second".into();
        second.kind = mapper::NodeKind::Sink {
            user: "c".into(),
            channel: OutputChannel::KeyHold { key: 68 },
        };
        definition.nodes.push(second);
        let mut edge = definition.edges.last().unwrap().clone();
        edge.to.node = "second".into();
        definition.edges.push(edge);
        let (mut runtime, mut backend) = (runtime(), MockBackend::default());
        apply_arm(&mut runtime, definition, &mut backend);
        let now = controlled_now(&runtime);
        runtime
            .action_at(
                ProgramOrigin::Agent(1, 0),
                key(&runtime, 0, 65, true),
                now,
                &mut backend,
            )
            .unwrap();
        backend.fail_delivery = Some(2);
        backend.fail_release = true;
        assert!(
            runtime
                .poll_timers(now + Duration::from_millis(100), &mut backend)
                .is_err()
        );
        assert!(!runtime.is_armed() && runtime.snapshot().cleanup_pending);
        assert_eq!(runtime.snapshot().pending_delays, 0);
        assert_eq!(backend.holds.len(), 1);
        runtime
            .poll_timers(now + Duration::from_secs(10), &mut backend)
            .unwrap();
        assert_eq!(backend.delivery_attempts, 2);
        backend.fail_release = false;
        runtime.last_cleanup = None;
        runtime.poll_cleanup(&mut backend).unwrap();
        assert!(backend.holds.is_empty() && !runtime.snapshot().cleanup_pending);
    }

    fn composed_timing_graph() -> Definition {
        graph(
            serde_json::json!([
                {"id":"source","kind":{"type":"key","user":"a","key":65,"disposition":"consume"}},
                {"id":"enable-delay","kind":{"type":"delay","signal":"hold","delay_ms":50}},
                {"id":"timer","kind":{"type":"timer","interval_ms":100,"count":1}},
                {"id":"pulse-delay","kind":{"type":"delay","signal":"pulse","delay_ms":50}},
                {"id":"second-delay","kind":{"type":"delay","signal":"pulse","delay_ms":25}},
                {"id":"sink","kind":{"type":"sink","user":"b","channel":{"type":"key_pulse","key":67}}}
            ]),
            serde_json::json!([
                {"from":{"node":"source","port":"value"},"to":{"node":"enable-delay","port":"value"}},
                {"from":{"node":"enable-delay","port":"value"},"to":{"node":"timer","port":"enabled"}},
                {"from":{"node":"timer","port":"value"},"to":{"node":"pulse-delay","port":"value"}},
                {"from":{"node":"pulse-delay","port":"value"},"to":{"node":"second-delay","port":"value"}},
                {"from":{"node":"second-delay","port":"value"},"to":{"node":"sink","port":"value"}}
            ]),
        )
    }

    #[test]
    fn delay_timer_delay_chains_capture_original_authority_and_anchor_each_accepted_wave() {
        let (mut runtime, mut backend) = (runtime(), MockBackend::default());
        apply_arm(&mut runtime, composed_timing_graph(), &mut backend);
        let now = controlled_now(&runtime);
        let origin = ProgramOrigin::Agent(1, 0);
        runtime
            .action_at(origin, key(&runtime, 0, 65, true), now, &mut backend)
            .unwrap();
        runtime
            .poll_timers(now + Duration::from_millis(50), &mut backend)
            .unwrap();
        assert_eq!(runtime.snapshot().pending_timers, 1);
        assert_eq!(runtime.snapshot().pending_delays, 0);
        runtime
            .poll_timers(now + Duration::from_millis(150), &mut backend)
            .unwrap();
        assert_eq!(runtime.snapshot().pending_timers, 0);
        assert_eq!(runtime.snapshot().pending_delays, 1);
        assert!(backend.delivered.is_empty());
        runtime
            .poll_timers(now + Duration::from_millis(200), &mut backend)
            .unwrap();
        assert_eq!(runtime.snapshot().pending_delays, 1);
        runtime
            .poll_timers(now + Duration::from_millis(224), &mut backend)
            .unwrap();
        assert!(backend.delivered.is_empty());
        runtime
            .poll_timers(now + Duration::from_millis(225), &mut backend)
            .unwrap();
        assert_eq!(backend.delivered.len(), 1);
        assert_eq!(backend.delivered[0].0.origin, origin);
        assert_eq!(runtime.snapshot().delayed_events, 3);
        assert_eq!(backend.originals.len(), 1);
    }

    #[test]
    fn delayed_hold_false_disables_a_downstream_timer_without_trailing_work() {
        let (mut runtime, mut backend) = (runtime(), MockBackend::default());
        apply_arm(&mut runtime, composed_timing_graph(), &mut backend);
        let now = controlled_now(&runtime);
        let origin = ProgramOrigin::Agent(1, 0);
        runtime
            .action_at(origin, key(&runtime, 0, 65, true), now, &mut backend)
            .unwrap();
        runtime
            .action_at(
                origin,
                key(&runtime, 0, 65, false),
                now + Duration::from_millis(10),
                &mut backend,
            )
            .unwrap();
        runtime
            .poll_timers(now + Duration::from_millis(50), &mut backend)
            .unwrap();
        assert_eq!(runtime.snapshot().pending_timers, 1);
        runtime
            .poll_timers(now + Duration::from_millis(60), &mut backend)
            .unwrap();
        assert_eq!(runtime.snapshot().pending_timers, 0);
        assert_eq!(runtime.snapshot().pending_delays, 0);
        runtime
            .poll_timers(now + Duration::from_secs(10), &mut backend)
            .unwrap();
        assert!(backend.delivered.is_empty());
    }

    #[test]
    fn timers_wait_do_not_catch_up_and_require_a_new_enable_after_exhaustion() {
        let (mut runtime, mut backend) = (runtime(), MockBackend::default());
        apply_arm(&mut runtime, timer_graph(), &mut backend);
        let origin = ProgramOrigin::Agent(1, 0);
        let before = Instant::now();
        runtime
            .action(origin, key(&runtime, 0, 65, true), &mut backend)
            .unwrap();
        assert_eq!(runtime.snapshot().pending_timers, 1);
        runtime.poll_timers(before, &mut backend).unwrap();
        assert!(backend.delivered.is_empty());
        let late = before + Duration::from_secs(20);
        runtime.poll_timers(late, &mut backend).unwrap();
        runtime.poll_timers(late, &mut backend).unwrap();
        assert_eq!(runtime.snapshot().timer_pulses, 1);
        assert_eq!(backend.delivered[0].0.origin, origin);
        assert_eq!(
            backend.originals.len(),
            1,
            "timers never submit a raw original"
        );
        let next = late + Duration::from_millis(100);
        runtime.poll_timers(next, &mut backend).unwrap();
        assert_eq!(runtime.snapshot().timer_pulses, 2);
        assert_eq!(runtime.snapshot().pending_timers, 0);
        runtime
            .action(origin, key(&runtime, 0, 65, true), &mut backend)
            .unwrap();
        runtime
            .poll_timers(next + Duration::from_secs(20), &mut backend)
            .unwrap();
        assert_eq!(backend.delivered.len(), 2);
        runtime
            .action(origin, key(&runtime, 0, 65, false), &mut backend)
            .unwrap();
        runtime
            .action(origin, key(&runtime, 0, 65, true), &mut backend)
            .unwrap();
        assert_eq!(runtime.snapshot().pending_timers, 1);
        runtime
            .action(origin, key(&runtime, 0, 65, false), &mut backend)
            .unwrap();
        assert_eq!(runtime.snapshot().pending_timers, 0);
        runtime
            .poll_timers(next + Duration::from_secs(20), &mut backend)
            .unwrap();
        assert_eq!(backend.delivered.len(), 2);
    }

    #[test]
    fn timer_recipient_change_and_client_retirement_never_deliver_stale_work() {
        let (mut runtime, mut backend) = (runtime(), MockBackend::default());
        apply_arm(&mut runtime, timer_graph(), &mut backend);
        let disconnected = ProgramOrigin::Agent(1, 0);
        let remaining = ProgramOrigin::Agent(2, 0);
        for origin in [disconnected, remaining] {
            runtime
                .action(origin, key(&runtime, 0, 65, true), &mut backend)
                .unwrap();
        }
        runtime.retire_actor(Actor::Agent(1), &mut backend).unwrap();
        assert_eq!(runtime.snapshot().pending_timers, 1);
        let late = Instant::now() + Duration::from_secs(20);
        runtime.poll_timers(late, &mut backend).unwrap();
        assert_eq!(backend.delivered[0].0.origin, remaining);
        backend.deny_exact_timer_recipient = true;
        assert!(
            runtime
                .poll_timers(late + Duration::from_millis(100), &mut backend)
                .is_err()
        );
        assert_eq!(backend.delivered.len(), 1);
        assert_eq!(runtime.snapshot().pending_timers, 0);
        assert!(!runtime.snapshot().armed);
        assert!(
            runtime
                .snapshot()
                .error
                .unwrap()
                .contains("timer effect recipient changed")
        );
        runtime
            .action(remaining, key(&runtime, 0, 65, true), &mut backend)
            .unwrap();
        assert_eq!(runtime.snapshot().pending_timers, 0);
    }

    #[test]
    fn timed_failed_up_retires_mapper_and_retains_the_saved_release() {
        let (mut runtime, mut backend) = (runtime(), MockBackend::default());
        apply_arm(&mut runtime, timer_graph(), &mut backend);
        let origin = ProgramOrigin::Agent(1, 0);
        runtime
            .action(origin, key(&runtime, 0, 65, true), &mut backend)
            .unwrap();
        backend.fail_timer_pulse_release = true;
        backend.fail_release = true;
        let late = Instant::now() + Duration::from_secs(20);
        assert!(runtime.poll_timers(late, &mut backend).is_err());
        assert!(!runtime.snapshot().armed);
        assert!(runtime.snapshot().cleanup_pending);
        assert_eq!(runtime.snapshot().pending_timers, 0);
        assert_eq!(backend.holds.len(), 1);
        assert_eq!(backend.holds.keys().next().unwrap().origin, origin);
        runtime
            .poll_timers(late + Duration::from_secs(20), &mut backend)
            .unwrap();
        assert_eq!(backend.delivery_attempts, 1);
        backend.fail_release = false;
        runtime.last_cleanup = None;
        runtime.poll_cleanup(&mut backend).unwrap();
        assert!(!runtime.snapshot().cleanup_pending);
        assert!(backend.holds.is_empty());
    }

    fn gated_hold_graph() -> Definition {
        graph(
            serde_json::json!([
                {"id":"value","kind":{"type":"key","user":"a","key":65}},
                {"id":"open","kind":{"type":"key","user":"b","key":66}},
                {"id":"gate","kind":{"type":"gate","signal":"hold"}},
                {"id":"sink","kind":{"type":"sink","user":"c","channel":{"type":"key_hold","key":67}}}
            ]),
            serde_json::json!([
                {"from":{"node":"value","port":"value"},"to":{"node":"gate","port":"value"}},
                {"from":{"node":"open","port":"value"},"to":{"node":"gate","port":"open"}},
                {"from":{"node":"gate","port":"value"},"to":{"node":"sink","port":"value"}}
            ]),
        )
    }
    fn apply_arm(runtime: &mut Runtime, definition: Definition, backend: &mut MockBackend) {
        let state = runtime
            .command(
                Command::Apply {
                    definition,
                    expected_revision: 0,
                    gui: None,
                },
                backend,
            )
            .unwrap();
        assert!(state.error.is_none(), "{:?}", state.error);
        let state = runtime
            .command(
                Command::Arm {
                    expected_revision: 1,
                    gui: None,
                },
                backend,
            )
            .unwrap();
        assert!(state.error.is_none(), "{:?}", state.error);
        backend.log.clear();
    }
    fn motion(runtime: &Runtime, user: usize, x: i32) -> Action {
        Action::Packet {
            user,
            packet: emulated::Packet::motion(runtime.seats[user].mouse, x, 0),
        }
    }
    fn key(runtime: &Runtime, user: usize, code: u32, held: bool) -> Action {
        Action::Packet {
            user,
            packet: emulated::Packet::key_scanned(
                runtime.seats[user].keyboard.unwrap(),
                code,
                1,
                0,
                !held,
            ),
        }
    }

    #[test]
    fn lifecycle_is_revision_checked_disarmed_and_reply_correlated() {
        let mut runtime = runtime();
        let mut backend = MockBackend::default();
        assert!(!runtime.snapshot().armed);
        let applied = runtime
            .command(
                Command::Apply {
                    definition: motion_graph(),
                    expected_revision: 0,
                    gui: Some(9),
                },
                &mut backend,
            )
            .unwrap();
        assert_eq!(applied.reply_to, Some(9));
        assert!(applied.acknowledged && !applied.armed);
        assert_eq!(applied.revision, 1);
        assert!(runtime.snapshot().reply_to.is_none());
        assert!(!runtime.snapshot().acknowledged);
        backend.held_source = true;
        let denied = runtime
            .command(
                Command::Arm {
                    expected_revision: 1,
                    gui: Some(10),
                },
                &mut backend,
            )
            .unwrap();
        assert!(denied.error.is_some());
        assert_eq!(denied.reply_to, Some(10));
        assert_eq!(denied.run, 0);
        backend.held_source = false;
        let armed = runtime
            .command(
                Command::Arm {
                    expected_revision: 1,
                    gui: None,
                },
                &mut backend,
            )
            .unwrap();
        assert!(armed.armed);
        assert_eq!(armed.run, 1);
        let releases = backend.releases.len();
        let stale = runtime
            .command(
                Command::Apply {
                    definition: Definition::default(),
                    expected_revision: 0,
                    gui: Some(11),
                },
                &mut backend,
            )
            .unwrap();
        assert!(stale.error.is_some() && stale.armed);
        assert_eq!(stale.revision, 1);
        assert_eq!(
            backend.releases.len(),
            releases,
            "stale Apply must not retire the current run"
        );
        assert!(backend.originals.is_empty() && backend.delivered.is_empty());
    }
    #[test]
    fn whole_wave_is_authorized_reserved_then_original_and_sinks() {
        let mut runtime = runtime();
        let mut backend = MockBackend::default();
        apply_arm(&mut runtime, motion_graph(), &mut backend);
        let first = motion(&runtime, 0, 1);
        runtime
            .action(ProgramOrigin::Agent(7, 0), first, &mut backend)
            .unwrap();
        assert_eq!(backend.log, ["authorize", "pointer", "reserve", "original"]);
        assert!(backend.delivered.is_empty());
        backend.log.clear();
        let second = motion(&runtime, 0, 1);
        runtime
            .action(ProgramOrigin::Agent(7, 0), second, &mut backend)
            .unwrap();
        assert_eq!(backend.log, ["authorize", "reserve", "original", "deliver"]);
        assert_eq!(backend.delivered[0].1, Quantized::Delta([1, 0]));
        assert_eq!(backend.delivered[0].0.source_users, [0]);
    }
    #[test]
    fn actor_remainders_and_gate_values_do_not_cross_clients() {
        let mut runtime = runtime();
        let mut backend = MockBackend::default();
        apply_arm(&mut runtime, motion_graph(), &mut backend);
        for client in [1, 2] {
            let action = motion(&runtime, 0, 1);
            runtime
                .action(ProgramOrigin::Agent(client, 0), action, &mut backend)
                .unwrap();
        }
        assert!(backend.delivered.is_empty());
        let action = motion(&runtime, 0, 1);
        runtime
            .action(ProgramOrigin::Agent(1, 0), action, &mut backend)
            .unwrap();
        assert_eq!(backend.delivered.len(), 1);
        assert_eq!(backend.delivered[0].0.origin, ProgramOrigin::Agent(1, 0));
        assert_eq!(runtime.snapshot().actor_contexts, 2);
    }
    #[test]
    fn different_source_closes_saved_owner_and_eof_preserves_other_actor() {
        let mut runtime = runtime();
        let mut backend = MockBackend::default();
        apply_arm(&mut runtime, gated_hold_graph(), &mut backend);
        let open = key(&runtime, 1, 66, true);
        runtime
            .action(ProgramOrigin::Agent(1, 1), open, &mut backend)
            .unwrap();
        let down = key(&runtime, 0, 65, true);
        runtime
            .action(ProgramOrigin::Agent(2, 0), down, &mut backend)
            .unwrap();
        assert!(
            backend.delivered.is_empty(),
            "another client's open gate must not admit this client's DOWN"
        );
        let down = key(&runtime, 0, 65, true);
        runtime
            .action(ProgramOrigin::Agent(1, 0), down, &mut backend)
            .unwrap();
        let open = key(&runtime, 1, 66, true);
        runtime
            .action(ProgramOrigin::Agent(2, 1), open, &mut backend)
            .unwrap();
        assert_eq!(backend.holds.len(), 2);
        let close = key(&runtime, 1, 66, false);
        runtime
            .action(ProgramOrigin::Agent(1, 1), close, &mut backend)
            .unwrap();
        let (owner, up) = backend.delivered.last().unwrap();
        assert_eq!(
            owner.origin,
            ProgramOrigin::Agent(1, 0),
            "closing event must not replace the DOWN origin"
        );
        assert_eq!(owner.source_users, [0, 1]);
        assert!(up.release());
        assert_eq!(backend.holds.len(), 1);
        runtime.retire_actor(Actor::Agent(1), &mut backend).unwrap();
        assert!(runtime.snapshot().armed);
        assert_eq!(runtime.snapshot().actor_contexts, 1);
        assert_eq!(
            backend.holds.keys().next().unwrap().origin,
            ProgramOrigin::Agent(2, 1)
        );
        assert_eq!(backend.releases.last(), Some(&(1, Some(Actor::Agent(1)))));
    }
    #[test]
    fn admission_failures_do_not_commit_remainders_or_stop_other_actors() {
        let mut runtime = runtime();
        let mut backend = MockBackend::default();
        apply_arm(&mut runtime, motion_graph(), &mut backend);
        backend.deny_authorization = true;
        let action = motion(&runtime, 0, 1);
        assert!(
            runtime
                .action(ProgramOrigin::Agent(1, 0), action, &mut backend)
                .is_err()
        );
        assert_eq!(backend.log, ["authorize"]);
        assert!(runtime.snapshot().armed);
        backend.deny_authorization = false;
        backend.deny_reserve = true;
        backend.log.clear();
        let action = motion(&runtime, 0, 1);
        assert!(
            runtime
                .action(ProgramOrigin::Agent(1, 0), action, &mut backend)
                .is_err()
        );
        assert_eq!(backend.log, ["authorize", "pointer", "reserve"]);
        assert!(backend.originals.is_empty());
        assert_eq!(runtime.snapshot().actor_contexts, 0);
        backend.deny_reserve = false;
        let action = motion(&runtime, 0, 1);
        runtime
            .action(ProgramOrigin::Agent(1, 0), action, &mut backend)
            .unwrap();
        assert!(
            backend.delivered.is_empty(),
            "rejected wave's fractional remainder must not leak"
        );
    }
    #[test]
    fn partial_delivery_stops_run_and_pending_cleanup_blocks_replacement() {
        let definition = graph(
            serde_json::json!([
                {"id":"source","kind":{"type":"key","user":"a","key":65}},
                {"id":"one","kind":{"type":"sink","user":"b","channel":{"type":"key_hold","key":66}}},
                {"id":"two","kind":{"type":"sink","user":"c","channel":{"type":"key_hold","key":67}}}
            ]),
            serde_json::json!([
                {"from":{"node":"source","port":"value"},"to":{"node":"one","port":"value"}},
                {"from":{"node":"source","port":"value"},"to":{"node":"two","port":"value"}}
            ]),
        );
        let mut runtime = runtime();
        let mut backend = MockBackend::default();
        apply_arm(&mut runtime, definition, &mut backend);
        let unrelated = SinkOwner {
            origin: ProgramOrigin::Manual(2),
            run: 99,
            sink: 0,
            user: 2,
            source_users: vec![2],
        };
        backend.holds.insert(
            unrelated.clone(),
            Quantized::Key {
                key: 70,
                held: true,
                pulse: false,
            },
        );
        backend.fail_delivery = Some(2);
        backend.fail_release = true;
        let down = key(&runtime, 0, 65, true);
        assert!(
            runtime
                .action(ProgramOrigin::Agent(1, 0), down, &mut backend)
                .is_err()
        );
        assert!(!runtime.snapshot().armed && runtime.snapshot().cleanup_pending);
        assert_eq!(runtime.snapshot().actor_contexts, 0);
        assert_eq!(backend.holds.len(), 2);
        let blocked = runtime
            .command(
                Command::Apply {
                    definition: Definition::default(),
                    expected_revision: 1,
                    gui: None,
                },
                &mut backend,
            )
            .unwrap();
        assert!(blocked.error.is_some());
        assert_eq!(blocked.revision, 1);
        let blocked = runtime
            .command(
                Command::Arm {
                    expected_revision: 1,
                    gui: None,
                },
                &mut backend,
            )
            .unwrap();
        assert!(blocked.error.is_some());
        backend.fail_release = false;
        runtime.last_cleanup = None;
        runtime.poll_cleanup(&mut backend).unwrap();
        assert!(!runtime.snapshot().cleanup_pending);
        assert_eq!(backend.holds.len(), 1);
        assert!(backend.holds.contains_key(&unrelated));
    }
    #[test]
    fn consumed_policy_is_only_requested_when_armed_and_original_still_runs_after_disarm() {
        let mut definition = motion_graph();
        let mapper::NodeKind::Pointer { disposition, .. } = &mut definition.nodes[0].kind else {
            unreachable!()
        };
        *disposition = Disposition::Consume;
        let mut runtime = runtime();
        let mut backend = MockBackend::default();
        apply_arm(&mut runtime, definition, &mut backend);
        let action = motion(&runtime, 0, 1);
        assert!(runtime.is_armed());
        assert_eq!(runtime.source_disposition(&action), Disposition::Consume);
        runtime
            .action(ProgramOrigin::Manual(0), action, &mut backend)
            .unwrap();
        assert_eq!(backend.originals.last().unwrap().1, Disposition::Consume);
        runtime.retire(&mut backend).unwrap();
        let action = motion(&runtime, 0, 1);
        assert_eq!(
            runtime.source_disposition(&action),
            Disposition::Passthrough
        );
        runtime
            .action(ProgramOrigin::Manual(0), action, &mut backend)
            .unwrap();
        assert_eq!(
            backend.originals.last().unwrap().1,
            Disposition::Passthrough,
            "backend still receives raw events to honor its retained DOWN ledger"
        );
    }
    #[test]
    fn same_virtual_key_different_scans_remains_held_until_both_release() {
        let definition = graph(
            serde_json::json!([
                {"id":"source","kind":{"type":"key","user":"a","key":65}},
                {"id":"sink","kind":{"type":"sink","user":"b","channel":{"type":"key_hold","key":66}}}
            ]),
            serde_json::json!([
                {"from":{"node":"source","port":"value"},"to":{"node":"sink","port":"value"}}
            ]),
        );
        let mut runtime = runtime();
        let mut backend = MockBackend::default();
        apply_arm(&mut runtime, definition, &mut backend);
        for (scan, extended) in [(1, 0), (2, 2)] {
            let action = Action::Packet {
                user: 0,
                packet: emulated::Packet::key_scanned(
                    runtime.seats[0].keyboard.unwrap(),
                    65,
                    scan,
                    extended,
                    false,
                ),
            };
            runtime
                .action(ProgramOrigin::Agent(1, 0), action, &mut backend)
                .unwrap();
        }
        assert_eq!(backend.delivered.len(), 1);
        let up = Action::Packet {
            user: 0,
            packet: emulated::Packet::key_scanned(
                runtime.seats[0].keyboard.unwrap(),
                65,
                1,
                0,
                true,
            ),
        };
        runtime
            .action(ProgramOrigin::Agent(1, 0), up, &mut backend)
            .unwrap();
        assert_eq!(
            backend.delivered.len(),
            1,
            "one raw scan UP must not release the other scan's aggregate hold"
        );
        let up = Action::Packet {
            user: 0,
            packet: emulated::Packet::key_scanned(
                runtime.seats[0].keyboard.unwrap(),
                65,
                2,
                2,
                true,
            ),
        };
        runtime
            .action(ProgramOrigin::Agent(1, 0), up, &mut backend)
            .unwrap();
        assert_eq!(backend.delivered.len(), 2);
        assert!(backend.delivered[1].1.release());
        assert!(backend.holds.is_empty());
    }
    #[test]
    fn referenced_user_is_retired_before_tombstone_and_cannot_be_rearmed() {
        let mut runtime = runtime();
        let mut backend = MockBackend::default();
        apply_arm(&mut runtime, motion_graph(), &mut backend);
        let sequence = runtime.prepare_remove_user(1, &mut backend).unwrap();
        assert!(!runtime.snapshot().armed);
        runtime.commit_remove_user(1, sequence);
        let state = runtime
            .command(
                Command::Arm {
                    expected_revision: 1,
                    gui: None,
                },
                &mut backend,
            )
            .unwrap();
        assert!(state.error.is_some());
        assert!(Runtime::new(vec!["a".into(), "a".into()], runtime.seats[..2].to_vec()).is_err());
    }
    fn effect(sink: &str, channel: OutputChannel, value: Value) -> Emission {
        Emission {
            sink: sink.into(),
            user: "target".into(),
            channel,
            value,
            source_users: ["source".into()].into(),
        }
    }
    #[test]
    fn fractional_motion_and_wheel_are_retained_per_sink_without_rounding_drift() {
        let mut quantizer = Quantizer::default();
        let motion = effect("motion", OutputChannel::Delta, Value::Delta([0.5, -0.5]));
        let wheel = effect(
            "wheel",
            OutputChannel::Wheel { horizontal: true },
            Value::Impulse(-0.5),
        );
        assert_eq!(quantizer.quantize(&motion).unwrap(), None);
        assert_eq!(quantizer.quantize(&wheel).unwrap(), None);
        assert_eq!(
            quantizer.quantize(&motion).unwrap(),
            Some(Quantized::Delta([1, -1]))
        );
        assert_eq!(
            quantizer.quantize(&wheel).unwrap(),
            Some(Quantized::Wheel {
                horizontal: true,
                delta: -1
            })
        );
        assert_eq!(
            Quantizer::default().quantize(&motion).unwrap(),
            None,
            "A retired run must not reuse old fractional state"
        );
    }
    #[test]
    fn invalid_quantization_preserves_the_previous_remainder_and_never_wraps() {
        let mut quantizer = Quantizer::default();
        let channel = OutputChannel::Wheel { horizontal: false };
        quantizer
            .quantize(&effect("wheel", channel.clone(), Value::Impulse(0.5)))
            .unwrap();
        assert!(
            quantizer
                .quantize(&effect("wheel", channel.clone(), Value::Impulse(32768.0)))
                .is_err()
        );
        assert_eq!(
            quantizer
                .quantize(&effect("wheel", channel, Value::Impulse(0.5)))
                .unwrap(),
            Some(Quantized::Wheel {
                horizontal: false,
                delta: 1
            })
        );
        assert!(
            quantizer
                .quantize(&effect(
                    "position",
                    OutputChannel::Position,
                    Value::Position([f64::NAN, 1.0])
                ))
                .is_err()
        );
    }
    #[test]
    fn absolute_rounding_and_binary_pulses_have_explicit_delivery_semantics() {
        let mut quantizer = Quantizer::default();
        assert_eq!(
            quantizer
                .quantize(&effect(
                    "position",
                    OutputChannel::Position,
                    Value::Position([1.5, -1.5])
                ))
                .unwrap(),
            Some(Quantized::Position([2, -2]))
        );
        assert_eq!(
            quantizer
                .quantize(&effect(
                    "pulse",
                    OutputChannel::KeyPulse { key: 65 },
                    Value::Pulse(true)
                ))
                .unwrap(),
            Some(Quantized::Key {
                key: 65,
                held: true,
                pulse: true
            })
        );
        assert!(
            quantizer
                .quantize(&effect(
                    "bad",
                    OutputChannel::KeyHold { key: 65 },
                    Value::Impulse(1.0)
                ))
                .is_err()
        );
    }
}
