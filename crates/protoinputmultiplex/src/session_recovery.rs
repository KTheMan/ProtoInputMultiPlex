//! Shared controller lifecycle and conservative original-process quarantine.
//! This state never grants PIDs, restarts routing, closes targets, or overrides cleanup.
use crate::session::Event;
use mux_core::Profile;
use mux_windows::targets::ProcessWatch;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, GetLastError},
    System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE},
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Starting,
    Running,
    Stopping,
    Quarantined,
    #[default]
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetStatus {
    Running,
    Exited,
    Unwatched,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetSnapshot {
    pub pid: u32,
    pub status: TargetStatus,
    pub affected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub phase: Phase,
    pub error: Option<String>,
    pub targets: Vec<TargetSnapshot>,
    pub can_start: bool,
}

/// Safe recovery data for an authenticated controller client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Projection {
    pub phase: Phase,
    pub can_start: bool,
    pub targets: Vec<TargetSnapshot>,
}

/// Internal metadata for certification against a controller and policy generation.
#[derive(Debug, Clone)]
pub(crate) struct Disclosure {
    pub generation: u64,
    pub projection: Projection,
    pub originals: Vec<OriginalEnrollment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OriginalEnrollment {
    pub pid: u32,
    pub token: u64,
    pub known_live: bool,
}

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
static NEXT_ENROLLMENT_TOKEN: AtomicU64 = AtomicU64::new(1);

fn allocate(counter: &AtomicU64) -> u64 {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
            next.checked_add(1)
        })
        .expect("recovery identity counter exhausted")
}
impl Default for Snapshot {
    fn default() -> Self {
        Model::default().snapshot()
    }
}

#[derive(Default)]
struct Model {
    phase: Phase,
    error: Option<String>,
    affected: BTreeMap<u32, TargetStatus>,
    application_names: BTreeMap<u32, String>,
}
impl Model {
    fn starting() -> Self {
        Self {
            phase: Phase::Starting,
            ..Self::default()
        }
    }
    fn enroll(&mut self, pid: u32, status: TargetStatus, affected: bool) {
        if pid != 0 && affected {
            self.affected.insert(pid, status);
        }
    }
    fn configured(&mut self) {
        if self.phase == Phase::Starting {
            self.phase = Phase::Running;
        }
    }
    fn stopping(&mut self) {
        if matches!(self.phase, Phase::Starting | Phase::Running) {
            self.phase = Phase::Stopping;
        }
    }
    fn clean_stop(&mut self) {
        if self.phase == Phase::Quarantined {
            return;
        }
        self.phase = Phase::Stopped;
        self.error = None;
        self.affected.clear();
        self.application_names.clear();
    }
    fn failed(&mut self, error: String) {
        self.phase = Phase::Quarantined;
        self.error = Some(error);
        self.recover_if_exited();
    }
    fn update(&mut self, pid: u32, status: TargetStatus) {
        if let Some(current) = self.affected.get_mut(&pid) {
            *current = status;
        }
    }
    fn recover_if_exited(&mut self) {
        if self.phase == Phase::Quarantined
            && self
                .affected
                .values()
                .all(|state| *state == TargetStatus::Exited)
        {
            self.phase = Phase::Stopped;
        }
    }
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            phase: self.phase,
            error: self.error.clone(),
            targets: self
                .affected
                .iter()
                .map(|(&pid, &status)| TargetSnapshot {
                    pid,
                    status,
                    affected: true,
                    application_name: self.application_names.get(&pid).cloned(),
                })
                .collect(),
            can_start: self.phase == Phase::Stopped,
        }
    }
}

// Process handles are kernel identity references: Win32 permits waiting and
// closing on another thread. All ownership/access remains inside Recovery's Mutex.
// This wrapper deliberately grants Send only, not Sync to ProcessWatch itself.
struct OwnedWatch(ProcessWatch);
unsafe impl Send for OwnedWatch {}

enum Identity {
    Watched(OwnedWatch),
    Unwatched,
    Exited,
}
impl Identity {
    fn open(pid: u32) -> Self {
        ProcessWatch::open([pid])
            .map(|watch| Self::Watched(OwnedWatch(watch)))
            .unwrap_or(Self::Unwatched)
    }
    fn open_with_application_name(pid: u32) -> (Self, Option<String>) {
        match ProcessWatch::open_with_application_name(pid) {
            Ok((watch, name)) => (Self::Watched(OwnedWatch(watch)), name),
            Err(_) => (Self::Unwatched, None),
        }
    }
    fn poll(&mut self, pid: u32) -> TargetStatus {
        match self {
            Self::Watched(watch) => {
                if watch.0.exited().is_some() {
                    TargetStatus::Exited
                } else {
                    TargetStatus::Running
                }
            }
            Self::Exited => TargetStatus::Exited,
            Self::Unwatched => {
                // A later successful open is deliberately conservative: if the
                // PID was reused, keep quarantine until that process also exits.
                // Never label this later identity as the original application.
                if let Ok(watch) = ProcessWatch::open([pid]) {
                    let status = if watch.exited().is_some() {
                        TargetStatus::Exited
                    } else {
                        TargetStatus::Running
                    };
                    *self = Self::Watched(OwnedWatch(watch));
                    status
                } else if process_is_absent(pid) {
                    *self = Self::Exited;
                    TargetStatus::Exited
                } else {
                    TargetStatus::Unwatched
                }
            }
        }
    }
}

struct Registry {
    model: Model,
    identities: BTreeMap<u32, Identity>,
    generation: u64,
    original_tokens: BTreeMap<u32, u64>,
}
impl Default for Registry {
    fn default() -> Self {
        Self {
            model: Model::default(),
            identities: BTreeMap::new(),
            generation: allocate(&NEXT_GENERATION),
            original_tokens: BTreeMap::new(),
        }
    }
}
impl Registry {
    fn starting() -> Self {
        Self {
            model: Model::starting(),
            identities: BTreeMap::new(),
            generation: allocate(&NEXT_GENERATION),
            original_tokens: BTreeMap::new(),
        }
    }

    fn for_profile(profile: &Profile) -> Self {
        let mut state = Self {
            model: Model::starting(),
            identities: BTreeMap::new(),
            generation: allocate(&NEXT_GENERATION),
            original_tokens: BTreeMap::new(),
        };
        if let Some(desktop) = &profile.desktop {
            for &pid in &desktop.allowed_pids {
                if pid != 0 && !state.identities.contains_key(&pid) {
                    state.capture_prepared_original(pid);
                }
            }
        } else {
            // Assigned routing attaches before Configured and before a State can
            // reach either frontend. Enroll all selected original identities now.
            for seat in &profile.seats {
                if seat.target_pid != 0 && !state.identities.contains_key(&seat.target_pid) {
                    state.capture_prepared_original(seat.target_pid);
                }
                state.attempt(seat.target_pid);
            }
        }
        state
    }
    // Only the unshared preflight constructor calls this. In particular, neither
    // an attachment observer nor GUI snapshot queries names under Recovery's Mutex.
    fn capture_prepared_original(&mut self, pid: u32) {
        debug_assert!(!self.model.affected.contains_key(&pid));
        let (identity, name) = Identity::open_with_application_name(pid);
        if matches!(&identity, Identity::Watched(watch) if watch.0.exited().is_none()) {
            self.original_tokens
                .insert(pid, allocate(&NEXT_ENROLLMENT_TOKEN));
        }
        self.identities.insert(pid, identity);
        self.model.application_names.remove(&pid);
        if let Some(name) = name {
            self.model.application_names.insert(pid, name);
        }
    }
    fn attempt(&mut self, pid: u32) {
        if pid == 0 {
            return;
        }
        if self.model.affected.contains_key(&pid) {
            // Reader and relay both observe Attaching. Preserve the first
            // affected original identity; a duplicate must not reopen by PID.
            return;
        }
        // A prewatched, unused process may have exited before a later attachment
        // attempt. Capture that attempt's current identity instead of treating a
        // previously signaled watch as proof that the new attempt is gone.
        let replace = match self.identities.get(&pid) {
            None | Some(Identity::Unwatched | Identity::Exited) => true,
            Some(Identity::Watched(watch)) => watch.0.exited().is_some(),
        };
        if replace {
            // Shared attachment observers must not issue image queries. An old
            // unused label cannot describe a replacement captured by numeric PID.
            self.model.application_names.remove(&pid);
            self.original_tokens.remove(&pid);
            self.identities.insert(pid, Identity::open(pid));
        }
        let identity = self
            .identities
            .get_mut(&pid)
            .expect("prepared original identity");
        let status = identity.poll(pid);
        self.model.enroll(pid, status, true);
    }
    fn poll(&mut self) {
        let affected: Vec<u32> = self.model.affected.keys().copied().collect();
        for pid in affected {
            if self.model.affected.get(&pid) == Some(&TargetStatus::Exited)
                && !self.identities.contains_key(&pid)
            {
                continue;
            }
            let status = self
                .identities
                .entry(pid)
                .or_insert(Identity::Unwatched)
                .poll(pid);
            self.model.update(pid, status);
            if status == TargetStatus::Exited
                && matches!(self.model.phase, Phase::Quarantined | Phase::Stopped)
            {
                self.identities.remove(&pid);
            }
        }
        self.model.recover_if_exited();
    }
    fn observe(&mut self, event: &Event) {
        match event {
            Event::Attaching(pid) => self.attempt(*pid),
            Event::Configured(_) => self.model.configured(),
            Event::Stopped => {
                if self.model.phase != Phase::Quarantined {
                    self.model.clean_stop();
                    self.identities.clear();
                    self.original_tokens.clear();
                }
            }
            Event::Failed(error) => {
                self.model.failed(error.clone());
                self.identities
                    .retain(|pid, _| self.model.affected.contains_key(pid));
                self.original_tokens
                    .retain(|pid, _| self.model.affected.contains_key(pid));
                self.model
                    .application_names
                    .retain(|pid, _| self.model.affected.contains_key(pid));
                self.poll();
            }
            _ => {}
        }
    }

    #[cfg(test)]
    fn disclosure(&self) -> Disclosure {
        let snapshot = self.model.snapshot();
        self.disclosure_from_snapshot(snapshot)
    }

    fn disclosure_from_snapshot(&self, snapshot: Snapshot) -> Disclosure {
        let originals = snapshot
            .targets
            .iter()
            .filter_map(|target| {
                let token = *self.original_tokens.get(&target.pid)?;
                let known_live =
                    matches!(self.identities.get(&target.pid), Some(Identity::Watched(_)))
                        && target.status == TargetStatus::Running;
                Some(OriginalEnrollment {
                    pid: target.pid,
                    token,
                    known_live,
                })
            })
            .collect();
        Disclosure {
            generation: self.generation,
            projection: Projection {
                phase: snapshot.phase,
                can_start: snapshot.can_start,
                targets: snapshot.targets,
            },
            originals,
        }
    }
}

pub(crate) struct PreparedRecovery(Registry);

#[derive(Clone)]
pub(crate) struct NativeSample {
    pub poll_started: Instant,
    pub sequence: u64,
    pub source_revision: u64,
    pub generation: u64,
    pub disclosure: Disclosure,
}

pub(crate) struct PublishedState {
    pub snapshot: Snapshot,
    pub revision: u64,
    pub generation: u64,
    pub native_sample: Option<NativeSample>,
}

struct Publication {
    revision: u64,
    sample_sequence: u64,
    snapshot: Snapshot,
    disclosure: Disclosure,
    native_sample: Option<NativeSample>,
    stop_at: Option<Instant>,
}

struct Shared {
    registry: Mutex<Registry>,
    published: Mutex<Publication>,
    stop_latched: AtomicBool,
}
impl Shared {
    fn new(registry: Registry) -> Self {
        let snapshot = registry.model.snapshot();
        let disclosure = registry.disclosure_from_snapshot(snapshot.clone());
        Self {
            registry: Mutex::new(registry),
            published: Mutex::new(Publication {
                revision: 0,
                sample_sequence: 0,
                snapshot,
                disclosure,
                native_sample: None,
                stop_at: None,
            }),
            stop_latched: AtomicBool::new(false),
        }
    }

    // Call with Registry locked. Never call a native API while Published is held.
    fn publish(&self, registry: &mut Registry, native_poll_started: Option<Instant>) {
        let mut published = self.published.lock().unwrap_or_else(|e| e.into_inner());
        if registry.generation < published.disclosure.generation {
            return;
        }
        if self.stop_latched.load(Ordering::Acquire) {
            registry.model.stopping();
        }
        published.revision = published
            .revision
            .checked_add(1)
            .expect("recovery revision exhausted");
        let snapshot = registry.model.snapshot();
        let disclosure = registry.disclosure_from_snapshot(snapshot.clone());
        let native_sample = native_poll_started.and_then(|poll_started| {
            if published
                .stop_at
                .is_some_and(|stop_at| poll_started < stop_at)
            {
                return None;
            }
            published.sample_sequence = published
                .sample_sequence
                .checked_add(1)
                .expect("recovery sample sequence exhausted");
            Some(NativeSample {
                poll_started,
                sequence: published.sample_sequence,
                source_revision: published.revision,
                generation: registry.generation,
                disclosure: disclosure.clone(),
            })
        });
        published.native_sample = native_sample;
        published.snapshot = snapshot;
        published.disclosure = disclosure;
    }
}

#[derive(Clone)]
pub struct Recovery(Arc<Shared>);

impl Default for Recovery {
    fn default() -> Self {
        Self(Arc::new(Shared::new(Registry::default())))
    }
}
impl Recovery {
    pub(crate) fn starting() -> Self {
        Self(Arc::new(Shared::new(Registry::starting())))
    }

    #[cfg(test)]
    pub(crate) fn for_profile(profile: &Profile) -> Self {
        Self(Arc::new(Shared::new(Registry::for_profile(profile))))
    }

    /// Build process watches without holding the shared Recovery mutex.
    pub(crate) fn prepare_profile(profile: &Profile) -> PreparedRecovery {
        PreparedRecovery(Registry::for_profile(profile))
    }

    pub(crate) fn from_prepared(prepared: PreparedRecovery) -> Self {
        Self(Arc::new(Shared::new(prepared.0)))
    }

    pub(crate) fn relay_is_last_owner(&self) -> bool {
        Arc::strong_count(&self.0) == 1
    }

    /// Install prepared identities while preserving a Stop latched during
    /// preflight. Process handles were opened before taking this mutex.
    pub(crate) fn install_prepared(&self, prepared: PreparedRecovery) {
        let mut current = self
            .0
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let stopping =
            self.0.stop_latched.load(Ordering::Acquire) || current.model.phase == Phase::Stopping;
        let mut prepared = prepared.0;
        prepared.generation = allocate(&NEXT_GENERATION);
        if stopping {
            prepared.model.stopping();
        }
        *current = prepared;
        self.0.publish(&mut current, None);
    }

    /// Clear a launch that is known not to have delivered a profile to a worker.
    pub(crate) fn cancel_prelaunch(&self) {
        let mut current = self
            .0
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if matches!(current.model.phase, Phase::Starting | Phase::Stopping) {
            current.model.clean_stop();
            current.identities.clear();
            current.original_tokens.clear();
            self.0.publish(&mut current, None);
        }
    }
    pub fn mark_stopping(&self) {
        let mut published = self.0.published.lock().unwrap_or_else(|e| e.into_inner());
        if !self.0.stop_latched.swap(true, Ordering::AcqRel) {
            published.stop_at = Some(Instant::now());
            published.revision = published
                .revision
                .checked_add(1)
                .expect("recovery revision exhausted");
            if matches!(published.snapshot.phase, Phase::Starting | Phase::Running) {
                published.snapshot.phase = Phase::Stopping;
                published.snapshot.can_start = false;
                published.disclosure.projection.phase = Phase::Stopping;
                published.disclosure.projection.can_start = false;
            }
            published.native_sample = None;
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        self.0
            .published
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot
            .clone()
    }

    pub(crate) fn published(&self) -> PublishedState {
        let published = self.0.published.lock().unwrap_or_else(|e| e.into_inner());
        PublishedState {
            snapshot: published.snapshot.clone(),
            revision: published.revision,
            generation: published.disclosure.generation,
            native_sample: published.native_sample.clone(),
        }
    }

    pub(crate) fn poll_native(&self) {
        let mut state = self.0.registry.lock().unwrap_or_else(|e| e.into_inner());
        let poll_started = Instant::now();
        state.poll();
        self.0.publish(&mut state, Some(poll_started));
    }

    /// Poll once, then copy the safe projection and watch-derived enrollment
    /// metadata. The caller must invoke this outside publication locks.
    #[cfg(test)]
    pub(crate) fn disclosure(&self) -> Disclosure {
        self.poll_native();
        self.0
            .published
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .disclosure
            .clone()
    }
    pub(crate) fn observe_attempt(&self, pid: u32) {
        let mut state = self.0.registry.lock().unwrap_or_else(|e| e.into_inner());
        state.attempt(pid);
        self.0.publish(&mut state, None);
    }
    pub(crate) fn observe(&self, event: &Event) {
        if !matches!(
            event,
            Event::Attaching(_) | Event::Configured(_) | Event::Stopped | Event::Failed(_)
        ) {
            return;
        }
        let mut state = self.0.registry.lock().unwrap_or_else(|e| e.into_inner());
        let native_poll_started = matches!(event, Event::Failed(_)).then(Instant::now);
        state.observe(event);
        self.0.publish(&mut state, native_poll_started);
    }
}

fn process_is_absent(pid: u32) -> bool {
    unsafe {
        let process = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if process.is_null() {
            GetLastError() == ERROR_INVALID_PARAMETER
        } else {
            CloseHandle(process);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread, time::Duration};

    #[test]
    fn cached_reads_and_stop_do_not_wait_for_a_blocked_native_poll() {
        let recovery = Recovery::starting();
        let owner = recovery.clone();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut registry = owner.0.registry.lock().unwrap();
            let poll_started = Instant::now();
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            registry.poll();
            owner.0.publish(&mut registry, Some(poll_started));
        });
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let started = Instant::now();
        let initial = recovery.snapshot();
        let initial_published = recovery.published();
        recovery.mark_stopping();
        let elapsed = started.elapsed();
        let stopping = recovery.published();
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        assert!(elapsed < Duration::from_millis(250));
        assert_eq!(initial.phase, Phase::Starting);
        assert_eq!(initial_published.snapshot.phase, Phase::Starting);
        assert_eq!(stopping.snapshot.phase, Phase::Stopping);
        let after = recovery.published();
        assert_eq!(after.snapshot.phase, Phase::Stopping);
        assert!(!after.snapshot.can_start);
        assert!(after.native_sample.is_none());

        recovery.poll_native();
        let post_stop = recovery.published();
        let sample = post_stop.native_sample.unwrap();
        assert_eq!(sample.disclosure.projection.phase, Phase::Stopping);
        assert_eq!(sample.source_revision, post_stop.revision);

        recovery.observe(&Event::Stopped);
        assert!(recovery.snapshot().can_start);
        assert_eq!(Recovery::starting().snapshot().phase, Phase::Starting);
    }

    #[test]
    fn an_old_generation_candidate_cannot_replace_new_enrollment() {
        let recovery = Recovery::starting();
        let mut stale = Registry::starting();
        stale.model.enroll(7, TargetStatus::Running, true);
        let mut prepared = Registry::starting();
        prepared.model.enroll(9, TargetStatus::Unwatched, true);
        recovery.install_prepared(PreparedRecovery(prepared));
        let current = recovery.published();
        let _owner = recovery.0.registry.lock().unwrap();
        recovery.0.publish(&mut stale, None);
        let after = recovery.published();
        assert_eq!(after.generation, current.generation);
        assert_eq!(after.revision, current.revision);
        assert_eq!(after.snapshot.targets[0].pid, 9);
    }
    #[test]
    fn clean_stop_clears_targets_and_drop_stop_cannot_restart_stopping() {
        let mut model = Model::starting();
        model.enroll(7, TargetStatus::Running, true);
        model.application_names.insert(7, "original.exe".into());
        model.application_names.insert(9, "unused.exe".into());
        model.configured();
        model.stopping();
        model.clean_stop();
        model.stopping();
        assert_eq!(model.phase, Phase::Stopped);
        assert!(model.snapshot().can_start);
        assert!(model.snapshot().targets.is_empty());
        assert!(model.application_names.is_empty());
    }
    #[test]
    fn failed_cleanup_retains_only_affected_targets_until_original_exits() {
        let mut model = Model::starting();
        model.enroll(7, TargetStatus::Running, true);
        model.enroll(9, TargetStatus::Running, false); // unused explicit scope
        model.failed("queue fence timed out".into());
        model.stopping();
        model.clean_stop(); // A stale success event cannot override failed cleanup.
        assert_eq!(model.phase, Phase::Quarantined);
        assert!(!model.snapshot().can_start);
        assert_eq!(model.snapshot().targets.len(), 1);
        assert_eq!(model.snapshot().targets[0].pid, 7);
        model.update(7, TargetStatus::Exited);
        model.recover_if_exited();
        assert_eq!(model.phase, Phase::Stopped);
        assert!(model.snapshot().can_start);
        assert_eq!(model.error.as_deref(), Some("queue fence timed out"));
    }
    #[test]
    fn assigned_enrollment_survives_failure_before_configured() {
        let mut model = Model::starting();
        for pid in [7, 8] {
            model.enroll(pid, TargetStatus::Running, true);
        }
        model.failed("second assigned attachment failed".into());
        assert_eq!(
            model
                .snapshot()
                .targets
                .iter()
                .map(|target| target.pid)
                .collect::<Vec<_>>(),
            vec![7, 8]
        );
        model.update(7, TargetStatus::Exited);
        model.recover_if_exited();
        assert!(!model.snapshot().can_start);
        model.update(8, TargetStatus::Exited);
        model.recover_if_exited();
        assert!(model.snapshot().can_start);
    }
    #[test]
    fn stopping_needs_terminal_even_if_all_targets_exited_and_configured_arrives_late() {
        let mut model = Model::starting();
        model.enroll(7, TargetStatus::Running, true);
        model.stopping();
        model.update(7, TargetStatus::Exited);
        model.recover_if_exited();
        model.configured();
        assert_eq!(model.phase, Phase::Stopping);
        assert!(!model.snapshot().can_start);
        model.failed("worker failed after target exit".into());
        assert!(model.snapshot().can_start);
    }

    #[test]
    fn prepared_start_preserves_latched_stop_and_cancel_releases_unaffected_watches() {
        let recovery = Recovery::starting();
        let mut registry = Registry::starting();
        registry.model.enroll(7, TargetStatus::Running, true);
        registry.identities.insert(7, Identity::Exited);

        recovery.mark_stopping();
        recovery.install_prepared(PreparedRecovery(registry));
        recovery.observe(&Event::Configured(1));
        assert_eq!(recovery.snapshot().phase, Phase::Stopping);
        assert_eq!(recovery.snapshot().targets[0].pid, 7);

        recovery.cancel_prelaunch();
        let snapshot = recovery.snapshot();
        assert_eq!(snapshot.phase, Phase::Stopped);
        assert!(snapshot.targets.is_empty());
        assert!(snapshot.can_start);
    }
    #[test]
    fn registry_captures_duplicate_assigned_original_once_before_configured() {
        let pid = std::process::id();
        let mut profile = Profile {
            seats: vec![mux_core::Seat {
                id: String::new(),
                name: "Owned test process".into(),
                mouse: "virtual://mouse/1".into(),
                keyboard: None,
                target_pid: pid,
                desktop_start: None,
                cursor: Default::default(),
                annotation: Default::default(),
                magnifier: Default::default(),
                output: Default::default(),
            }],
            ..Default::default()
        };
        profile.seats.push(profile.seats[0].clone());
        let mut registry = Registry::for_profile(&profile);
        assert_eq!(registry.identities.len(), 1);
        assert_eq!(registry.model.snapshot().targets.len(), 1);
        assert!(matches!(
            registry.identities.get(&pid),
            Some(Identity::Watched(_))
        ));
        registry.observe(&Event::Failed("startup failed before Configured".into()));
        assert_eq!(registry.model.phase, Phase::Quarantined);
        assert_eq!(registry.model.snapshot().targets[0].pid, pid);
        assert!(!registry.model.snapshot().can_start);
        registry.observe(&Event::Stopped);
        assert_eq!(registry.model.phase, Phase::Quarantined);
        assert!(registry.identities.contains_key(&pid));
    }
    #[test]
    fn registry_releases_unused_scope_on_failure_without_quarantining_it() {
        let pid = std::process::id();
        let profile = Profile {
            desktop: Some(mux_core::routing::DesktopRouting {
                allowed_pids: vec![pid],
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut registry = Registry::for_profile(&profile);
        assert!(matches!(
            registry.identities.get(&pid),
            Some(Identity::Watched(_))
        ));
        assert!(registry.model.snapshot().targets.is_empty());
        registry
            .model
            .application_names
            .insert(pid, "unused.exe".into());
        assert!(registry.model.snapshot().targets.is_empty());
        registry.observe(&Event::Failed("runtime failed before attachment".into()));
        assert!(registry.model.snapshot().can_start);
        assert!(registry.identities.is_empty());
        assert!(registry.model.snapshot().targets.is_empty());
        assert!(registry.model.application_names.is_empty());
    }

    #[test]
    fn original_label_survives_exit_watch_drop_and_duplicate_attachment() {
        let pid = std::process::id();
        let mut registry = Registry::starting();
        let watch = ProcessWatch::open([pid]).unwrap();
        registry
            .identities
            .insert(pid, Identity::Watched(OwnedWatch(watch)));
        registry
            .model
            .application_names
            .insert(pid, "original.exe".into());
        registry.model.enroll(pid, TargetStatus::Running, true);
        registry.model.failed("cleanup incomplete".into());

        // Supply the terminal identity state without forcing real PID reuse.
        // Replacing this entry drops its real synchronize-only process handle.
        registry.identities.insert(pid, Identity::Exited);
        registry.poll();
        assert!(!registry.identities.contains_key(&pid));
        let target = &registry.model.snapshot().targets[0];
        assert_eq!(target.status, TargetStatus::Exited);
        assert_eq!(target.application_name.as_deref(), Some("original.exe"));

        // The numeric PID is currently live, but affected enrollment must not
        // reopen it or replace the original display label after watch removal.
        registry.observe(&Event::Attaching(pid));
        assert!(!registry.identities.contains_key(&pid));
        let target = &registry.model.snapshot().targets[0];
        assert_eq!(target.status, TargetStatus::Exited);
        assert_eq!(target.application_name.as_deref(), Some("original.exe"));
    }

    #[test]
    fn conservative_unwatched_reopen_never_acquires_a_label() {
        let pid = std::process::id();
        let mut registry = Registry::starting();
        registry.identities.insert(pid, Identity::Unwatched);
        registry.model.enroll(pid, TargetStatus::Unwatched, true);
        registry.poll();
        assert!(matches!(
            registry.identities.get(&pid),
            Some(Identity::Watched(_))
        ));
        let target = &registry.model.snapshot().targets[0];
        assert_eq!(target.status, TargetStatus::Running);
        assert!(target.application_name.is_none());
        assert!(registry.disclosure().originals.is_empty());
        registry.observe(&Event::Attaching(pid));
        assert!(
            registry.model.snapshot().targets[0]
                .application_name
                .is_none()
        );
    }

    #[test]
    fn unused_prewatch_replacement_discards_label_without_a_name_query() {
        let pid = std::process::id();
        let mut registry = Registry::starting();
        registry.identities.insert(pid, Identity::Exited);
        registry
            .model
            .application_names
            .insert(pid, "unused prewatch sentinel.exe".into());
        assert!(registry.model.snapshot().targets.is_empty());
        registry.attempt(pid);
        let target = &registry.model.snapshot().targets[0];
        assert_eq!(target.status, TargetStatus::Running);
        assert!(target.application_name.is_none());
    }

    #[test]
    fn old_recovery_json_deserializes_without_a_label_and_none_is_omitted() {
        let old = r#"{"phase":"quarantined","error":"incomplete","targets":[{"pid":7,"status":"running","affected":true}],"can_start":false}"#;
        let mut snapshot: Snapshot = serde_json::from_str(old).unwrap();
        assert!(snapshot.targets[0].application_name.is_none());
        let value = serde_json::to_value(&snapshot).unwrap();
        assert!(value["targets"][0].get("application_name").is_none());
        snapshot.targets[0].application_name = Some("original.exe".into());
        let value = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(value["targets"][0]["application_name"], "original.exe");
    }

    #[test]
    fn disclosure_keeps_certifiable_original_token_after_exit() {
        let pid = std::process::id();
        let mut registry = Registry::starting();
        registry.identities.insert(
            pid,
            Identity::Watched(OwnedWatch(ProcessWatch::open([pid]).unwrap())),
        );
        let token = allocate(&NEXT_ENROLLMENT_TOKEN);
        registry.original_tokens.insert(pid, token);
        registry.model.enroll(pid, TargetStatus::Running, true);
        registry.model.failed("cleanup incomplete".into());

        let live = registry.disclosure();
        assert_eq!(live.originals.len(), 1);
        assert_eq!(live.originals[0].token, token);
        assert!(live.originals[0].known_live);

        registry.identities.insert(pid, Identity::Exited);
        registry.poll();
        let exited = registry.disclosure();
        assert_eq!(exited.originals.len(), 1);
        assert_eq!(exited.originals[0].token, token);
        assert!(!exited.originals[0].known_live);
        assert!(!registry.identities.contains_key(&pid));
    }

    #[test]
    fn late_unwatched_reopen_has_no_original_enrollment_token() {
        let pid = std::process::id();
        let mut registry = Registry::starting();
        registry.identities.insert(pid, Identity::Unwatched);
        registry.model.enroll(pid, TargetStatus::Unwatched, true);
        registry.model.failed("cleanup incomplete".into());

        registry.poll();
        assert!(matches!(
            registry.identities.get(&pid),
            Some(Identity::Watched(_))
        ));
        assert!(registry.disclosure().originals.is_empty());
    }

    #[test]
    fn installing_prepared_registry_changes_generation_and_clean_stop_drops_tokens() {
        let recovery = Recovery::starting();
        let before = recovery.disclosure().generation;
        let mut prepared = Registry::starting();
        let prepared_generation = prepared.generation;
        let pid = std::process::id();
        prepared.identities.insert(
            pid,
            Identity::Watched(OwnedWatch(ProcessWatch::open([pid]).unwrap())),
        );
        prepared
            .original_tokens
            .insert(pid, allocate(&NEXT_ENROLLMENT_TOKEN));
        prepared.model.enroll(pid, TargetStatus::Running, true);
        recovery.install_prepared(PreparedRecovery(prepared));

        let disclosure = recovery.disclosure();
        assert_ne!(disclosure.generation, before);
        assert_ne!(disclosure.generation, prepared_generation);
        assert_eq!(disclosure.originals.len(), 1);

        recovery.observe(&Event::Stopped);
        let stopped = recovery.disclosure();
        assert!(stopped.originals.is_empty());
    }
}
