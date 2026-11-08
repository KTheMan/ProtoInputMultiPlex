//! Single-worker, coalesced desktop inventory for the native GUI.
//!
//! The worker owns all discovery calls. The UI exchanges bounded snapshots and
//! scope revisions through a short-held mailbox; no OS query runs under its lock.

use mux_core::{Device, desktop::Bounds};
use mux_windows::{monitors::Monitor, targets};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scope {
    pub readiness_pids: Vec<u32>,
    pub picker: Option<PickerScope>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerScope {
    pub user: usize,
    pub revision: u64,
    /// An empty list preserves the GUI's existing unrestricted picker policy.
    pub allowed_pids: Vec<u32>,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub generation: u64,
    pub scope: Scope,
    pub devices: Result<Vec<Device>, String>,
    pub windows: Result<Vec<targets::Target>, String>,
    pub monitors: Result<Vec<Monitor>, String>,
    pub desktop_bounds: Result<Bounds, String>,
    pub readiness: BTreeMap<u32, targets::TargetEligibility>,
    pub picker: Option<Result<Vec<(targets::Target, targets::TargetEligibility)>, String>>,
    pub elapsed_ms: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Status {
    /// Independently sampled GUI callback activity, when explicitly enabled.
    #[serde(
        default,
        deserialize_with = "crate::gui_activity::deserialize_optional",
        skip_serializing_if = "Option::is_none"
    )]
    pub gui_activity: Option<crate::gui_activity::Snapshot>,
    pub requested_generation: u64,
    pub completed_generation: u64,
    pub in_flight: bool,
    pub queued: bool,
    pub coalesced_requests: u64,
    pub superseded_results: u64,
    pub age_ms: Option<u64>,
    #[serde(default)]
    pub sample_age_ms: u64,
    pub elapsed_ms: Option<u64>,
    pub device_count: usize,
    pub window_count: usize,
    pub monitor_count: usize,
    pub errors: Vec<String>,
    pub worker_failed: bool,
}

struct Job {
    generation: u64,
    scope: Scope,
}

struct Completed {
    snapshot: Snapshot,
}

#[derive(Default)]
struct Mailbox {
    next_generation: u64,
    requested_generation: u64,
    requested_scope: Option<Scope>,
    pending: Option<Job>,
    in_flight: bool,
    completed_generation: u64,
    completed: Option<Completed>,
    completed_at: Option<Instant>,
    elapsed_ms: Option<u64>,
    device_count: usize,
    window_count: usize,
    monitor_count: usize,
    errors: Vec<String>,
    coalesced_requests: u64,
    superseded_results: u64,
    worker_failed: bool,
    stopping: bool,
}

struct Shared {
    mailbox: Mutex<Mailbox>,
    wake: Condvar,
}

fn lock(shared: &Shared) -> MutexGuard<'_, Mailbox> {
    shared
        .mailbox
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Owns one discovery thread. Dropping the service signals cooperative stop
/// and detaches the join handle, so a native call cannot block UI shutdown.
pub struct Service {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl Service {
    pub fn start() -> anyhow::Result<Self> {
        Self::start_delayed(Duration::ZERO)
    }

    /// Start the native provider with a worker-only delay before each device
    /// scan. This bounded hook supports responsiveness acceptance checks while
    /// keeping production callers on the normal zero-delay path.
    pub fn start_delayed(delay: Duration) -> anyhow::Result<Self> {
        anyhow::ensure!(
            delay <= Duration::from_secs(15),
            "discovery delay cannot exceed 15 seconds"
        );
        let shared = Arc::new(Shared {
            mailbox: Mutex::new(Mailbox::default()),
            wake: Condvar::new(),
        });
        Self::start_with_shared(shared.clone(), NativeProvider { shared, delay })
    }

    #[cfg(test)]
    fn start_with_provider(provider: impl Provider) -> anyhow::Result<Self> {
        let shared = Arc::new(Shared {
            mailbox: Mutex::new(Mailbox::default()),
            wake: Condvar::new(),
        });
        Self::start_with_shared(shared, provider)
    }

    fn start_with_shared(shared: Arc<Shared>, provider: impl Provider) -> anyhow::Result<Self> {
        let worker_shared = shared.clone();
        let worker = thread::Builder::new()
            .name("gui-discovery".into())
            .spawn(move || {
                if catch_unwind(AssertUnwindSafe(|| worker_loop(&worker_shared, provider))).is_err()
                {
                    let mut mailbox = lock(&worker_shared);
                    mailbox.in_flight = false;
                    mailbox.pending = None;
                    mailbox.worker_failed = true;
                }
            })?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }

    /// Request a fresh snapshot. Identical pending work shares its generation;
    /// a request during active native work is kept as the one pending refresh.
    pub fn request(&mut self, scope: Scope) -> u64 {
        let scope = normalize_scope(scope);
        let mut mailbox = lock(&self.shared);
        let pending_generation = mailbox
            .pending
            .as_ref()
            .filter(|pending| pending.scope == scope)
            .map(|pending| pending.generation);
        if let Some(generation) = pending_generation {
            mailbox.coalesced_requests = mailbox.coalesced_requests.saturating_add(1);
            return generation;
        }
        mailbox.next_generation = mailbox.next_generation.saturating_add(1).max(1);
        let generation = mailbox.next_generation;
        if mailbox.pending.is_some() {
            mailbox.coalesced_requests = mailbox.coalesced_requests.saturating_add(1);
        }
        mailbox.requested_generation = generation;
        mailbox.requested_scope = Some(scope.clone());
        mailbox.pending = Some(Job { generation, scope });
        drop(mailbox);
        self.shared.wake.notify_one();
        generation
    }

    /// Ensure the latest requested scope exists, without refreshing an
    /// identical pending, active, or most recently completed request.
    pub fn ensure(&mut self, scope: Scope) -> u64 {
        let scope = normalize_scope(scope);
        {
            let mailbox = lock(&self.shared);
            if mailbox.requested_scope.as_ref() == Some(&scope) {
                return mailbox.requested_generation;
            }
        }
        self.request(scope)
    }

    /// Take only a result for the latest requested generation. Older results
    /// are discarded and counted, never applied to a newer UI scope.
    pub fn poll(&mut self) -> Option<Snapshot> {
        let mut mailbox = lock(&self.shared);
        let completed = mailbox.completed.take()?;
        if completed.snapshot.generation != mailbox.requested_generation {
            mailbox.superseded_results = mailbox.superseded_results.saturating_add(1);
            return None;
        }
        Some(completed.snapshot)
    }

    pub fn latest_generation(&self) -> u64 {
        lock(&self.shared).requested_generation
    }

    pub fn requested_scope(&self) -> Option<Scope> {
        lock(&self.shared).requested_scope.clone()
    }

    pub fn status(&self) -> Status {
        let mailbox = lock(&self.shared);
        Status {
            gui_activity: None,
            requested_generation: mailbox.requested_generation,
            completed_generation: mailbox.completed_generation,
            in_flight: mailbox.in_flight,
            queued: mailbox.pending.is_some(),
            coalesced_requests: mailbox.coalesced_requests,
            superseded_results: mailbox.superseded_results,
            age_ms: mailbox.completed_at.map(|at| millis(at.elapsed())),
            sample_age_ms: 0,
            elapsed_ms: mailbox.elapsed_ms,
            device_count: mailbox.device_count,
            window_count: mailbox.window_count,
            monitor_count: mailbox.monitor_count,
            errors: mailbox.errors.clone(),
            worker_failed: mailbox.worker_failed,
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        {
            let mut mailbox = lock(&self.shared);
            mailbox.stopping = true;
            mailbox.pending = None;
        }
        self.shared.wake.notify_one();
        // Dropping JoinHandle detaches; never wait on a native API from UI drop.
        drop(self.worker.take());
    }
}

fn normalize_scope(mut scope: Scope) -> Scope {
    scope.readiness_pids.sort_unstable();
    scope.readiness_pids.dedup();
    if let Some(picker) = &mut scope.picker {
        picker.allowed_pids.sort_unstable();
        picker.allowed_pids.dedup();
    }
    scope
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

trait Provider: Send + 'static {
    fn devices(&mut self) -> anyhow::Result<Vec<Device>>;
    fn windows(&mut self) -> anyhow::Result<Vec<targets::Target>>;
    fn monitors(&mut self) -> anyhow::Result<Vec<Monitor>>;
    fn desktop_bounds(&mut self) -> anyhow::Result<Bounds>;
    fn eligibility(&mut self, pid: u32) -> targets::TargetEligibility;
}

struct NativeProvider {
    shared: Arc<Shared>,
    delay: Duration,
}

impl Provider for NativeProvider {
    fn devices(&mut self) -> anyhow::Result<Vec<Device>> {
        if !wait_for_delay_or_stop(&self.shared, self.delay) {
            return Ok(Vec::new());
        }
        mux_windows::raw_input::devices()
    }

    fn windows(&mut self) -> anyhow::Result<Vec<targets::Target>> {
        targets::windows()
    }

    fn monitors(&mut self) -> anyhow::Result<Vec<Monitor>> {
        mux_windows::monitors::inventory()
    }

    fn desktop_bounds(&mut self) -> anyhow::Result<Bounds> {
        mux_windows::monitors::physical_desktop_bounds()
    }

    fn eligibility(&mut self, pid: u32) -> targets::TargetEligibility {
        targets::eligibility(pid)
    }
}

/// Wait on the worker thread while leaving the mailbox unlocked during the
/// delay. Drop wakes this wait so no scan starts after cooperative shutdown.
fn wait_for_delay_or_stop(shared: &Shared, delay: Duration) -> bool {
    if delay.is_zero() {
        return !should_stop(shared);
    }
    let mailbox = lock(shared);
    if mailbox.stopping {
        return false;
    }
    let (mailbox, _) = shared
        .wake
        .wait_timeout_while(mailbox, delay, |mailbox| !mailbox.stopping)
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    !mailbox.stopping
}

fn worker_loop(provider_shared: &Shared, mut provider: impl Provider) {
    loop {
        let job = {
            let mut mailbox = lock(provider_shared);
            while mailbox.pending.is_none() && !mailbox.stopping {
                mailbox = provider_shared
                    .wake
                    .wait(mailbox)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            if mailbox.stopping {
                return;
            }
            mailbox.in_flight = true;
            mailbox.pending.take().expect("pending checked above")
        };

        let started = Instant::now();
        let Some(snapshot) = collect_snapshot(provider_shared, &mut provider, job, started) else {
            return;
        };
        let at = Instant::now();
        let mut mailbox = lock(provider_shared);
        mailbox.in_flight = false;
        if mailbox.stopping {
            return;
        }
        if snapshot.generation != mailbox.requested_generation {
            mailbox.superseded_results = mailbox.superseded_results.saturating_add(1);
            continue;
        }
        if mailbox.completed.take().is_some() {
            mailbox.superseded_results = mailbox.superseded_results.saturating_add(1);
        }
        mailbox.completed_generation = snapshot.generation;
        mailbox.elapsed_ms = Some(snapshot.elapsed_ms);
        mailbox.device_count = snapshot.devices.as_ref().map_or(0, Vec::len);
        mailbox.window_count = snapshot.windows.as_ref().map_or(0, Vec::len);
        mailbox.monitor_count = snapshot.monitors.as_ref().map_or(0, Vec::len);
        mailbox.errors = snapshot_errors(&snapshot);
        mailbox.completed_at = Some(at);
        mailbox.completed = Some(Completed { snapshot });
    }
}

fn should_stop(shared: &Shared) -> bool {
    lock(shared).stopping
}

fn collect_snapshot(
    shared: &Shared,
    provider: &mut impl Provider,
    job: Job,
    started: Instant,
) -> Option<Snapshot> {
    let devices = provider.devices().map_err(|error| format!("{error:#}"));
    if should_stop(shared) {
        return None;
    }
    let windows = provider.windows().map_err(|error| format!("{error:#}"));
    if should_stop(shared) {
        return None;
    }
    let monitors = provider.monitors().map_err(|error| format!("{error:#}"));
    if should_stop(shared) {
        return None;
    }
    let desktop_bounds = provider
        .desktop_bounds()
        .and_then(|bounds| {
            anyhow::ensure!(bounds.valid(), "desktop bounds are invalid");
            Ok(bounds)
        })
        .map_err(|error| format!("{error:#}"));
    if should_stop(shared) {
        return None;
    }

    let mut pids: BTreeSet<u32> = job.scope.readiness_pids.iter().copied().collect();
    if job.scope.picker.is_some()
        && let Ok(windows) = &windows
    {
        let allowed = job.scope.picker.as_ref().expect("picker checked");
        pids.extend(
            windows
                .iter()
                .filter(|window| {
                    allowed.allowed_pids.is_empty() || allowed.allowed_pids.contains(&window.pid)
                })
                .map(|window| window.pid),
        );
    }
    let mut all_readiness = BTreeMap::new();
    for pid in pids {
        if should_stop(shared) {
            return None;
        }
        all_readiness.insert(pid, provider.eligibility(pid));
    }
    let readiness = job
        .scope
        .readiness_pids
        .iter()
        .filter_map(|pid| all_readiness.get(pid).cloned().map(|item| (*pid, item)))
        .collect();
    let picker = job
        .scope
        .picker
        .as_ref()
        .map(|picker_scope| match &windows {
            Ok(windows) => Ok(windows
                .iter()
                .filter(|window| {
                    picker_scope.allowed_pids.is_empty()
                        || picker_scope.allowed_pids.contains(&window.pid)
                })
                .filter_map(|window| {
                    all_readiness
                        .get(&window.pid)
                        .cloned()
                        .map(|readiness| (window.clone(), readiness))
                })
                .collect()),
            Err(error) => Err(error.clone()),
        });
    Some(Snapshot {
        generation: job.generation,
        scope: job.scope,
        devices,
        windows,
        monitors,
        desktop_bounds,
        readiness,
        picker,
        elapsed_ms: millis(started.elapsed()),
    })
}

fn snapshot_errors(snapshot: &Snapshot) -> Vec<String> {
    let mut errors = Vec::new();
    if let Err(error) = &snapshot.devices {
        errors.push(format!("devices: {error}"));
    }
    if let Err(error) = &snapshot.windows {
        errors.push(format!("windows: {error}"));
    }
    if let Err(error) = &snapshot.monitors {
        errors.push(format!("monitors: {error}"));
    }
    if let Err(error) = &snapshot.desktop_bounds {
        errors.push(format!("display geometry: {error}"));
    }
    if let Some(Err(error)) = &snapshot.picker {
        errors.push(format!("picker: {error}"));
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Default)]
    struct Gate {
        entered: Mutex<bool>,
        released: Mutex<bool>,
        wake: Condvar,
    }

    impl Gate {
        fn block(&self) {
            let mut entered = self.entered.lock().unwrap();
            *entered = true;
            self.wake.notify_all();
            drop(entered);
            let mut released = self.released.lock().unwrap();
            while !*released {
                released = self.wake.wait(released).unwrap();
            }
        }

        fn wait_entered(&self) {
            let entered = self.entered.lock().unwrap();
            let (entered, timeout) = self
                .wake
                .wait_timeout_while(entered, Duration::from_secs(2), |entered| !*entered)
                .unwrap();
            assert!(*entered, "worker did not enter blocked provider");
            assert!(!timeout.timed_out());
        }

        fn release(&self) {
            *self.released.lock().unwrap() = true;
            self.wake.notify_all();
        }
    }

    struct TestProvider {
        gate: Option<Arc<Gate>>,
        first: bool,
        fail: bool,
        desktop_bounds: Bounds,
        eligibility_calls: Arc<Mutex<Vec<u32>>>,
    }

    impl Provider for TestProvider {
        fn devices(&mut self) -> anyhow::Result<Vec<Device>> {
            if self.first {
                self.first = false;
                if let Some(gate) = &self.gate {
                    gate.block();
                }
            }
            if self.fail {
                anyhow::bail!("device provider failure");
            }
            Ok(Vec::new())
        }

        fn windows(&mut self) -> anyhow::Result<Vec<targets::Target>> {
            if self.fail {
                anyhow::bail!("window provider failure");
            }
            Ok(vec![targets::Target {
                hwnd: 99,
                pid: 7,
                title: "fixture".into(),
            }])
        }

        fn monitors(&mut self) -> anyhow::Result<Vec<Monitor>> {
            if self.fail {
                anyhow::bail!("monitor provider failure");
            }
            Ok(Vec::new())
        }

        fn desktop_bounds(&mut self) -> anyhow::Result<Bounds> {
            Ok(self.desktop_bounds)
        }

        fn eligibility(&mut self, pid: u32) -> targets::TargetEligibility {
            self.eligibility_calls.lock().unwrap().push(pid);
            targets::TargetEligibility {
                pid,
                creation: Some(1),
                creation_token: Some("1".into()),
                state: targets::EligibilityState::Fresh,
                reason: "ready".into(),
            }
        }
    }

    fn provider(gate: Option<Arc<Gate>>, fail: bool) -> TestProvider {
        TestProvider {
            gate,
            first: true,
            fail,
            desktop_bounds: Bounds {
                left: -1920,
                top: -200,
                right: 2560,
                bottom: 1440,
            },
            eligibility_calls: Arc::default(),
        }
    }

    fn eventually(service: &Service, predicate: impl Fn(&Status) -> bool) -> Status {
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            let status = service.status();
            if predicate(&status) {
                return status;
            }
            assert!(Instant::now() < until, "condition not reached: {status:?}");
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn pending_work_coalesces_and_superseded_generation_is_discarded() {
        let gate = Arc::new(Gate::default());
        let mut service =
            Service::start_with_provider(provider(Some(gate.clone()), false)).unwrap();
        let first = service.request(Scope::default());
        gate.wait_entered();
        let scope = Scope {
            readiness_pids: vec![9, 8, 9],
            picker: None,
        };
        let second = service.request(scope.clone());
        assert_eq!(second, service.request(scope.clone()));
        let third = service.request(Scope {
            readiness_pids: vec![10],
            picker: None,
        });
        assert!(third > second && second > first);
        gate.release();
        let status = eventually(&service, |status| status.completed_generation == third);
        assert!(status.coalesced_requests >= 2);
        assert!(status.superseded_results >= 1);
        let snapshot = service
            .poll()
            .expect("latest generation should be available");
        assert_eq!(snapshot.generation, third);
        assert_eq!(snapshot.scope.readiness_pids, vec![10]);
        assert!(service.poll().is_none());
    }

    #[test]
    fn readiness_queries_are_deduplicated_and_component_errors_are_retained() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut p = provider(None, true);
        p.eligibility_calls = calls.clone();
        let mut service = Service::start_with_provider(p).unwrap();
        let generation = service.request(Scope {
            readiness_pids: vec![7, 7],
            picker: Some(PickerScope {
                user: 2,
                revision: 3,
                allowed_pids: vec![],
            }),
        });
        eventually(&service, |status| status.completed_generation == generation);
        let snapshot = service.poll().unwrap();
        assert!(snapshot.devices.is_err());
        assert!(snapshot.windows.is_err());
        assert!(snapshot.monitors.is_err());
        let desktop = snapshot.desktop_bounds.unwrap();
        assert_eq!(desktop.left, -1920);
        assert_eq!(desktop.top, -200);
        assert!(snapshot.picker.as_ref().unwrap().is_err());
        assert_eq!(*calls.lock().unwrap(), vec![7]);
        let status = service.status();
        assert_eq!(status.errors.len(), 4);
        assert!(status.errors.iter().all(|error| !error.contains("fixture")));
    }

    #[test]
    fn invalid_desktop_bounds_do_not_poison_monitor_inventory() {
        let mut provider = provider(None, false);
        provider.desktop_bounds = Bounds::default();
        let mut service = Service::start_with_provider(provider).unwrap();
        let generation = service.request(Scope::default());
        eventually(&service, |status| status.completed_generation == generation);
        let snapshot = service.poll().unwrap();
        assert!(snapshot.monitors.is_ok());
        assert_eq!(
            snapshot.desktop_bounds.unwrap_err(),
            "desktop bounds are invalid"
        );
        assert_eq!(
            service.status().errors,
            ["display geometry: desktop bounds are invalid"]
        );
    }

    #[test]
    fn dropping_service_does_not_wait_for_native_provider() {
        let gate = Arc::new(Gate::default());
        let mut service =
            Service::start_with_provider(provider(Some(gate.clone()), false)).unwrap();
        service.request(Scope::default());
        gate.wait_entered();
        let started = Instant::now();
        drop(service);
        assert!(started.elapsed() < Duration::from_millis(250));
        gate.release();
    }

    #[test]
    fn ensure_reuses_the_latest_requested_scope() {
        let gate = Arc::new(Gate::default());
        let mut service =
            Service::start_with_provider(provider(Some(gate.clone()), false)).unwrap();
        let scope = Scope {
            readiness_pids: vec![5, 4, 5],
            picker: None,
        };
        let requested = service.ensure(scope.clone());
        gate.wait_entered();
        assert_eq!(service.ensure(scope), requested);
        assert_eq!(service.latest_generation(), requested);
        gate.release();
        eventually(&service, |status| status.completed_generation == requested);
        assert!(!service.status().in_flight);
    }

    #[test]
    fn delayed_start_rejects_values_over_fifteen_seconds_before_spawning() {
        assert!(Service::start_delayed(Duration::from_millis(15_001)).is_err());
    }

    #[test]
    fn drop_stop_is_observed_between_collection_phases() {
        struct StopProvider {
            gate: Arc<Gate>,
            calls: Arc<Mutex<Vec<&'static str>>>,
            exited: Arc<AtomicBool>,
        }

        impl Provider for StopProvider {
            fn devices(&mut self) -> anyhow::Result<Vec<Device>> {
                self.calls.lock().unwrap().push("devices");
                self.gate.block();
                Ok(Vec::new())
            }

            fn windows(&mut self) -> anyhow::Result<Vec<targets::Target>> {
                self.calls.lock().unwrap().push("windows");
                Ok(Vec::new())
            }

            fn monitors(&mut self) -> anyhow::Result<Vec<Monitor>> {
                self.calls.lock().unwrap().push("monitors");
                Ok(Vec::new())
            }

            fn desktop_bounds(&mut self) -> anyhow::Result<Bounds> {
                self.calls.lock().unwrap().push("desktop_bounds");
                Ok(Bounds::default())
            }

            fn eligibility(&mut self, pid: u32) -> targets::TargetEligibility {
                self.calls.lock().unwrap().push("eligibility");
                targets::TargetEligibility {
                    pid,
                    creation: None,
                    creation_token: None,
                    state: targets::EligibilityState::Unavailable,
                    reason: "unavailable".into(),
                }
            }
        }

        impl Drop for StopProvider {
            fn drop(&mut self) {
                self.exited.store(true, Ordering::Release);
            }
        }

        let gate = Arc::new(Gate::default());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let exited = Arc::new(AtomicBool::new(false));
        let provider = StopProvider {
            gate: gate.clone(),
            calls: calls.clone(),
            exited: exited.clone(),
        };
        let mut service = Service::start_with_provider(provider).unwrap();
        service.request(Scope::default());
        gate.wait_entered();
        drop(service);
        gate.release();

        let until = Instant::now() + Duration::from_secs(2);
        while !exited.load(Ordering::Acquire) {
            assert!(Instant::now() < until, "worker provider did not exit");
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(*calls.lock().unwrap(), vec!["devices"]);
    }
}
