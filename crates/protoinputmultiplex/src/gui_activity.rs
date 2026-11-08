//! Callback activity, independent of screenshot and filesystem publication.
//! This is evidence of Rust callback boundaries, never a health or authority verdict.
use serde::{Deserialize, Serialize};
use std::{
    marker::PhantomData,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering, fence},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Callback {
    RawInput,
    Logic,
    Ui,
}
impl Callback {
    fn index(self) -> usize {
        match self {
            Self::RawInput => 0,
            Self::Logic => 1,
            Self::Ui => 2,
        }
    }
    fn decode(value: u64) -> Option<Self> {
        match value {
            1 => Some(Self::RawInput),
            2 => Some(Self::Logic),
            3 => Some(Self::Ui),
            _ => None,
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counter {
    pub entered: u64,
    pub returned: u64,
    pub unwound: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub phase: Option<Callback>,
    pub in_callback: bool,
    pub closed: bool,
    /// Age of the last entry or exit marker, according to `in_callback`.
    pub phase_age_ms: Option<u64>,
    pub last_return_age_ms: Option<u64>,
    pub raw_input: Counter,
    pub logic: Counter,
    pub ui: Counter,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub process_id: u32,
    /// Unique only among probes within this process. The authenticated descriptor
    /// and its existing incarnation remain the connection/lifetime boundary.
    pub gui_instance: u64,
    pub coherent: bool,
    /// Receiver-added cache residence. Pipe transport time is not measured.
    pub sample_age_ms: u64,
    /// Mixed atomic reads, writer overlap or exhausted counters are unknown.
    pub observation: Option<Observation>,
    #[serde(default, deserialize_with = "deserialize_render_capture")]
    pub render_capture: Option<RenderCapture>,
}
/// Independent screenshot request/receipt and PNG-publication bookkeeping.
/// Ages are monotonic observation ages, never GPU/glReadPixels durations.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderCapture {
    pub script_id: u64,
    pub enabled: bool,
    pub requested_count: u64,
    pub requested_generation: u64,
    pub request_age_ms: Option<u64>,
    pub received_count: u64,
    pub received_generation: u64,
    pub receipt_age_ms: Option<u64>,
    pub pending_age_ms: Option<u64>,
    pub received_dimensions: Option<[u32; 2]>,
    pub worker_accepted: Option<bool>,
    pub retired: bool,
    /// Independently sampled under the publication queue lock. This may lead
    /// receipt bookkeeping transiently; the two stages are not one transaction.
    pub publication: Option<crate::gui_capture::PublicationSnapshot>,
}
impl RenderCapture {
    fn supported(&mut self) -> bool {
        if self
            .publication
            .as_ref()
            .is_some_and(|value| !value.supported())
        {
            self.publication = None;
        }
        self.script_id != 0
            && self.received_generation <= self.requested_generation
            && self.received_count <= self.requested_count
            && (self.requested_count == 0) == (self.requested_generation == 0)
            && (self.received_count == 0) == (self.received_generation == 0)
            && self.request_age_ms.is_some() == (self.requested_count != 0)
            && self.receipt_age_ms.is_some() == (self.received_count != 0)
            && self.received_dimensions.is_some() == (self.received_count != 0)
            && self.worker_accepted.is_some() == (self.received_count != 0)
            && self.pending_age_ms.is_some()
                == (!self.retired
                    && self.enabled
                    && self.requested_generation > self.received_generation)
            && (self.enabled || (self.requested_count == 0 && self.publication.is_none()))
    }
}
fn deserialize_render_capture<'de, D>(deserializer: D) -> Result<Option<RenderCapture>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value
        .and_then(|value| serde_json::from_value::<RenderCapture>(value).ok())
        .and_then(|mut value| value.supported().then_some(value)))
}

#[derive(Debug)]
struct RenderShared {
    origin: Instant,
    script_id: u64,
    enabled: bool,
    sequence: AtomicU64,
    invalid: AtomicBool,
    requested_count: AtomicU64,
    requested_generation: AtomicU64,
    requested_at: AtomicU64,
    received_count: AtomicU64,
    received_generation: AtomicU64,
    received_at: AtomicU64,
    pending: AtomicBool,
    pending_at: AtomicU64,
    width: AtomicU64,
    height: AtomicU64,
    accepted: AtomicBool,
    retired: AtomicBool,
    publication: Option<crate::gui_capture::PublicationObserver>,
}
impl RenderShared {
    fn write(&self, operation: impl FnOnce(&Self)) {
        let sequence = self.sequence.load(Ordering::Relaxed);
        if self.invalid.load(Ordering::Relaxed) || sequence & 1 != 0 || sequence > u64::MAX - 2 {
            self.invalid.store(true, Ordering::Release);
            return;
        }
        self.sequence.fetch_add(1, Ordering::AcqRel);
        operation(self);
        self.sequence.store(sequence + 2, Ordering::Release);
    }
    fn increment(&self, value: &AtomicU64) {
        if let Some(next) = value.load(Ordering::Relaxed).checked_add(1) {
            value.store(next, Ordering::Relaxed);
        } else {
            self.invalid.store(true, Ordering::Release);
        }
    }
    fn tick(&self, now: Instant) -> u64 {
        millis(now.saturating_duration_since(self.origin))
    }
}
/// Script stays transferable during bootstrap. This handle is installed only
/// on the GUI thread and only that thread calls these private writer methods.
#[derive(Debug)]
pub(crate) struct CaptureReporter {
    shared: Arc<RenderShared>,
}
impl CaptureReporter {
    pub(crate) fn requested(&self, generation: u64, now: Instant) {
        self.shared.write(|shared| {
            if !shared.enabled
                || shared.retired.load(Ordering::Relaxed)
                || generation <= shared.requested_generation.load(Ordering::Relaxed)
            {
                return;
            }
            let tick = shared.tick(now);
            shared.increment(&shared.requested_count);
            shared
                .requested_generation
                .store(generation, Ordering::Relaxed);
            shared.requested_at.store(tick, Ordering::Relaxed);
            if !shared.pending.swap(true, Ordering::Relaxed) {
                shared.pending_at.store(tick, Ordering::Relaxed);
            }
        });
    }
    pub(crate) fn received(
        &self,
        generation: u64,
        dimensions: [usize; 2],
        accepted: bool,
        now: Instant,
    ) {
        self.shared.write(|shared| {
            if shared.retired.load(Ordering::Relaxed)
                || generation == 0
                || generation > shared.requested_generation.load(Ordering::Relaxed)
                || generation <= shared.received_generation.load(Ordering::Relaxed)
            {
                return;
            }
            let (Ok(width), Ok(height)) =
                (u32::try_from(dimensions[0]), u32::try_from(dimensions[1]))
            else {
                shared.invalid.store(true, Ordering::Release);
                return;
            };
            shared.increment(&shared.received_count);
            shared
                .received_generation
                .store(generation, Ordering::Relaxed);
            shared
                .received_at
                .store(shared.tick(now), Ordering::Relaxed);
            shared.width.store(u64::from(width), Ordering::Relaxed);
            shared.height.store(u64::from(height), Ordering::Relaxed);
            shared.accepted.store(accepted, Ordering::Relaxed);
            if generation == shared.requested_generation.load(Ordering::Relaxed) {
                shared.pending.store(false, Ordering::Relaxed);
            }
        });
    }
}
impl Drop for CaptureReporter {
    fn drop(&mut self) {
        self.shared.write(|shared| {
            shared.retired.store(true, Ordering::Relaxed);
            shared.pending.store(false, Ordering::Relaxed);
        });
    }
}
#[derive(Clone, Debug)]
struct RenderObserver {
    shared: Arc<RenderShared>,
}
impl RenderObserver {
    fn snapshot_at(&self, now: Instant) -> Option<RenderCapture> {
        let shared = &self.shared;
        let first = shared.sequence.load(Ordering::Acquire);
        if first & 1 != 0 || shared.invalid.load(Ordering::Acquire) {
            return None;
        }
        let now = shared.tick(now);
        let requested_count = shared.requested_count.load(Ordering::Relaxed);
        let received_count = shared.received_count.load(Ordering::Relaxed);
        let mut value = RenderCapture {
            script_id: shared.script_id,
            enabled: shared.enabled,
            requested_count,
            requested_generation: shared.requested_generation.load(Ordering::Relaxed),
            request_age_ms: (requested_count != 0)
                .then(|| now.saturating_sub(shared.requested_at.load(Ordering::Relaxed))),
            received_count,
            received_generation: shared.received_generation.load(Ordering::Relaxed),
            receipt_age_ms: (received_count != 0)
                .then(|| now.saturating_sub(shared.received_at.load(Ordering::Relaxed))),
            pending_age_ms: shared
                .pending
                .load(Ordering::Relaxed)
                .then(|| now.saturating_sub(shared.pending_at.load(Ordering::Relaxed))),
            received_dimensions: (received_count != 0).then(|| {
                [
                    shared.width.load(Ordering::Relaxed) as u32,
                    shared.height.load(Ordering::Relaxed) as u32,
                ]
            }),
            worker_accepted: (received_count != 0).then(|| shared.accepted.load(Ordering::Relaxed)),
            retired: shared.retired.load(Ordering::Relaxed),
            publication: None,
        };
        fence(Ordering::Acquire);
        if shared.sequence.load(Ordering::Relaxed) != first
            || shared.invalid.load(Ordering::Acquire)
        {
            return None;
        }
        value.publication = shared
            .publication
            .as_ref()
            .and_then(|observer| observer.snapshot());
        value.supported().then_some(value)
    }
}

fn millis(value: Duration) -> u64 {
    u64::try_from(value.as_millis()).unwrap_or(u64::MAX)
}
impl Snapshot {
    fn supported(&self) -> bool {
        if self.version != 1 || self.process_id == 0 || self.gui_instance == 0 {
            return false;
        }
        let Some(observation) = &self.observation else {
            return !self.coherent;
        };
        if !self.coherent || observation.phase.is_some() != observation.phase_age_ms.is_some() {
            return false;
        }
        let counters = [&observation.raw_input, &observation.logic, &observation.ui];
        for (index, counter) in counters.iter().enumerate() {
            let Some(finished) = counter.returned.checked_add(counter.unwound) else {
                return false;
            };
            let pending = u64::from(
                observation.in_callback
                    && observation
                        .phase
                        .is_some_and(|phase| phase.index() == index),
            );
            if finished.checked_add(pending) != Some(counter.entered) {
                return false;
            }
        }
        (!observation.in_callback || observation.phase.is_some())
            && observation.last_return_age_ms.is_some()
                == counters.iter().any(|counter| counter.returned != 0)
            && observation
                .phase
                .is_none_or(|phase| counters[phase.index()].entered != 0)
    }
    pub fn age_by(&mut self, elapsed: Duration) {
        let elapsed = millis(elapsed);
        self.sample_age_ms = self.sample_age_ms.saturating_add(elapsed);
        if let Some(capture) = &mut self.render_capture {
            for age in [
                &mut capture.request_age_ms,
                &mut capture.receipt_age_ms,
                &mut capture.pending_age_ms,
            ]
            .into_iter()
            .flatten()
            {
                *age = age.saturating_add(elapsed);
            }
        }
        if let Some(observation) = &mut self.observation {
            for age in [
                &mut observation.phase_age_ms,
                &mut observation.last_return_age_ms,
            ]
            .into_iter()
            .flatten()
            {
                *age = age.saturating_add(elapsed);
            }
        }
    }
}

/// Optional telemetry must not invalidate an otherwise valid older bridge frame.
/// Unknown versions or malformed samples clear the cached field, rather than
/// retaining an earlier sample or converting it into a responsiveness claim.
pub fn deserialize_optional<'de, D>(deserializer: D) -> Result<Option<Snapshot>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value
        .and_then(|value| serde_json::from_value::<Snapshot>(value).ok())
        .filter(Snapshot::supported))
}
#[derive(Debug, Default)]
struct AtomicCounter {
    entered: AtomicU64,
    returned: AtomicU64,
    unwound: AtomicU64,
}
impl AtomicCounter {
    fn read(&self) -> Counter {
        Counter {
            entered: self.entered.load(Ordering::Relaxed),
            returned: self.returned.load(Ordering::Relaxed),
            unwound: self.unwound.load(Ordering::Relaxed),
        }
    }
}
#[derive(Debug)]
struct Shared {
    origin: Instant,
    instance: u64,
    sequence: AtomicU64,
    invalid: AtomicBool,
    phase: AtomicU64,
    active: AtomicBool,
    phase_at: AtomicU64,
    has_return: AtomicBool,
    return_at: AtomicU64,
    closed: AtomicBool,
    counters: [AtomicCounter; 3],
    capture: Mutex<Option<RenderObserver>>,
}
impl Shared {
    fn clock_at(&self, now: Instant) -> u64 {
        millis(now.saturating_duration_since(self.origin))
    }
    /// The Probe and its spans are deliberately !Send/!Sync. Only the GUI writer
    /// changes these atomics; observers perform one bounded read, never a spin.
    fn write(&self, operation: impl FnOnce(&Self)) -> bool {
        if self.invalid.load(Ordering::Relaxed) {
            return false;
        }
        let sequence = self.sequence.load(Ordering::Relaxed);
        if sequence & 1 != 0 || sequence > u64::MAX - 2 {
            self.invalid.store(true, Ordering::Release);
            return false;
        }
        self.sequence.fetch_add(1, Ordering::AcqRel);
        operation(self);
        self.sequence.store(sequence + 2, Ordering::Release);
        true
    }
    fn increment(&self, counter: &AtomicU64) {
        let value = counter.load(Ordering::Relaxed);
        if let Some(next) = value.checked_add(1) {
            counter.store(next, Ordering::Relaxed);
        } else {
            self.invalid.store(true, Ordering::Release);
        }
    }
}
#[derive(Clone, Debug)]
pub struct Observer {
    shared: Arc<Shared>,
}
impl Observer {
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot_at(Instant::now())
    }
    fn snapshot_at(&self, now: Instant) -> Snapshot {
        let shared = &self.shared;
        let mut snapshot = Snapshot {
            version: 1,
            process_id: std::process::id(),
            gui_instance: shared.instance,
            coherent: false,
            sample_age_ms: 0,
            observation: None,
            render_capture: shared
                .capture
                .try_lock()
                .ok()
                .and_then(|slot| slot.clone())
                .and_then(|observer| observer.snapshot_at(now)),
        };
        let first = shared.sequence.load(Ordering::Acquire);
        if first & 1 != 0 || shared.invalid.load(Ordering::Acquire) {
            return snapshot;
        }
        let now = shared.clock_at(now);
        let phase = Callback::decode(shared.phase.load(Ordering::Relaxed));
        let observation = Observation {
            phase,
            in_callback: shared.active.load(Ordering::Relaxed),
            closed: shared.closed.load(Ordering::Relaxed),
            phase_age_ms: phase
                .map(|_| now.saturating_sub(shared.phase_at.load(Ordering::Relaxed))),
            last_return_age_ms: shared
                .has_return
                .load(Ordering::Relaxed)
                .then(|| now.saturating_sub(shared.return_at.load(Ordering::Relaxed))),
            raw_input: shared.counters[0].read(),
            logic: shared.counters[1].read(),
            ui: shared.counters[2].read(),
        };
        fence(Ordering::Acquire);
        if shared.sequence.load(Ordering::Relaxed) == first
            && !shared.invalid.load(Ordering::Acquire)
        {
            snapshot.coherent = true;
            snapshot.observation = Some(observation);
        }
        snapshot
    }
}
#[derive(Debug)]
pub struct Probe {
    shared: Arc<Shared>,
    _single_writer: PhantomData<Rc<()>>,
}
impl Default for Probe {
    fn default() -> Self {
        Self::new()
    }
}
impl Probe {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let instance = NEXT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .unwrap_or(0);
        Self {
            shared: Arc::new(Shared {
                origin: Instant::now(),
                instance,
                sequence: AtomicU64::new(0),
                invalid: AtomicBool::new(instance == 0),
                phase: AtomicU64::new(0),
                active: AtomicBool::new(false),
                phase_at: AtomicU64::new(0),
                has_return: AtomicBool::new(false),
                return_at: AtomicU64::new(0),
                closed: AtomicBool::new(false),
                counters: std::array::from_fn(|_| AtomicCounter::default()),
                capture: Mutex::new(None),
            }),
            _single_writer: PhantomData,
        }
    }
    pub(crate) fn bind_render_capture(
        &self,
        script_id: u64,
        enabled: bool,
        publication: Option<crate::gui_capture::PublicationObserver>,
    ) -> CaptureReporter {
        let shared = Arc::new(RenderShared {
            origin: Instant::now(),
            script_id,
            enabled,
            sequence: AtomicU64::new(0),
            invalid: AtomicBool::new(script_id == 0),
            requested_count: AtomicU64::new(0),
            requested_generation: AtomicU64::new(0),
            requested_at: AtomicU64::new(0),
            received_count: AtomicU64::new(0),
            received_generation: AtomicU64::new(0),
            received_at: AtomicU64::new(0),
            pending: AtomicBool::new(false),
            pending_at: AtomicU64::new(0),
            width: AtomicU64::new(0),
            height: AtomicU64::new(0),
            accepted: AtomicBool::new(false),
            retired: AtomicBool::new(false),
            publication,
        });
        // Startup-only GUI binding; the observer holds this lock only to clone
        // an Arc. It never holds it while reading atomics or worker metadata.
        *self
            .shared
            .capture
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(RenderObserver {
            shared: shared.clone(),
        });
        CaptureReporter { shared }
    }
    pub fn observer(&self) -> Observer {
        Observer {
            shared: self.shared.clone(),
        }
    }
    pub fn enter(&self, callback: Callback) -> Span {
        let accepted = self.shared.write(|shared| {
            if shared.active.load(Ordering::Relaxed) || shared.closed.load(Ordering::Relaxed) {
                shared.invalid.store(true, Ordering::Release);
                return;
            }
            shared.increment(&shared.counters[callback.index()].entered);
            shared
                .phase
                .store(callback.index() as u64 + 1, Ordering::Relaxed);
            shared
                .phase_at
                .store(shared.clock_at(Instant::now()), Ordering::Relaxed);
            shared.active.store(true, Ordering::Relaxed);
        });
        Span {
            shared: self.shared.clone(),
            callback,
            accepted,
            _single_writer: PhantomData,
        }
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        self.shared
            .write(|shared| shared.closed.store(true, Ordering::Relaxed));
    }
}
pub struct Span {
    shared: Arc<Shared>,
    callback: Callback,
    accepted: bool,
    _single_writer: PhantomData<Rc<()>>,
}
impl Drop for Span {
    fn drop(&mut self) {
        if !self.accepted {
            return;
        }
        self.shared.write(|shared| {
            if std::thread::panicking() {
                shared.increment(&shared.counters[self.callback.index()].unwound);
            } else {
                shared.increment(&shared.counters[self.callback.index()].returned);
                shared
                    .return_at
                    .store(shared.clock_at(Instant::now()), Ordering::Relaxed);
                shared.has_return.store(true, Ordering::Relaxed);
            }
            shared
                .phase_at
                .store(shared.clock_at(Instant::now()), Ordering::Relaxed);
            shared.active.store(false, Ordering::Relaxed);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_request_receipt_ages_pending_reset_and_retirement_are_independent() {
        let probe = Probe::new();
        let observer = probe.observer();
        let reporter = probe.bind_render_capture(7, true, None);
        let base = reporter.shared.origin;
        reporter.requested(1, base);
        reporter.requested(2, base + Duration::from_secs(2));
        reporter.received(1, [20, 30], true, base + Duration::from_secs(4));
        let state = observer
            .snapshot_at(base + Duration::from_secs(5))
            .render_capture
            .unwrap();
        assert_eq!(
            (
                state.requested_count,
                state.requested_generation,
                state.received_count,
                state.received_generation
            ),
            (2, 2, 1, 1)
        );
        assert_eq!(
            (
                state.request_age_ms,
                state.receipt_age_ms,
                state.pending_age_ms
            ),
            (Some(3000), Some(1000), Some(5000))
        );
        reporter.received(1, [1, 1], false, base + Duration::from_secs(5));
        assert_eq!(
            observer
                .snapshot_at(base + Duration::from_secs(5))
                .render_capture
                .unwrap()
                .received_dimensions,
            Some([20, 30])
        );
        reporter.received(2, [0, 30], false, base + Duration::from_secs(6));
        let mut state = observer.snapshot_at(base + Duration::from_secs(7));
        let capture = state.render_capture.as_ref().unwrap();
        assert_eq!(capture.pending_age_ms, None);
        assert_eq!(capture.worker_accepted, Some(false));
        state.age_by(Duration::from_millis(500));
        assert_eq!(
            state.render_capture.as_ref().unwrap().request_age_ms,
            Some(5500)
        );
        assert_eq!(
            state.render_capture.as_ref().unwrap().receipt_age_ms,
            Some(1500)
        );
        probe.shared.sequence.store(1, Ordering::Relaxed);
        let mixed = observer.snapshot_at(base + Duration::from_secs(8));
        assert!(!mixed.coherent && mixed.observation.is_none() && mixed.render_capture.is_some());
        probe.shared.sequence.store(0, Ordering::Relaxed);
        drop(reporter);
        let retired = observer
            .snapshot_at(base + Duration::from_secs(8))
            .render_capture
            .unwrap();
        assert!(retired.retired && retired.pending_age_ms.is_none());
        assert_eq!(retired.received_generation, 2);
    }

    #[test]
    fn malformed_optional_capture_never_discards_valid_callback_or_keeps_stale_data() {
        #[derive(Deserialize)]
        struct Envelope {
            #[serde(default, deserialize_with = "deserialize_optional")]
            activity: Option<Snapshot>,
        }
        let probe = Probe::new();
        drop(probe.enter(Callback::Logic));
        let original = serde_json::to_value(probe.observer().snapshot()).unwrap();
        let mut older = original.clone();
        older.as_object_mut().unwrap().remove("render_capture");
        let parsed: Envelope =
            serde_json::from_value(serde_json::json!({"activity":older})).unwrap();
        assert!(parsed.activity.unwrap().render_capture.is_none());
        for malformed in [
            serde_json::json!({"script_id":42}),
            serde_json::json!("wrong shape"),
            serde_json::json!({"script_id":7,"enabled":true,
            "requested_count":1,"requested_generation":1,"request_age_ms":0,"received_count":1,"received_generation":2,
            "receipt_age_ms":0,"received_dimensions":[1,1],"worker_accepted":true,"retired":false}),
        ] {
            let mut value = original.clone();
            value["render_capture"] = malformed;
            let parsed: Envelope =
                serde_json::from_value(serde_json::json!({"activity":value})).unwrap();
            let snapshot = parsed.activity.unwrap();
            assert!(snapshot.coherent && snapshot.observation.is_some());
            assert!(snapshot.render_capture.is_none());
        }
        let reporter = probe.bind_render_capture(8, false, None);
        let mut value = serde_json::to_value(probe.observer().snapshot()).unwrap();
        value["render_capture"]["publication"] =
            serde_json::json!({"accepted":0,"published":99,"retired":false});
        let parsed: Envelope =
            serde_json::from_value(serde_json::json!({"activity":value})).unwrap();
        assert!(
            parsed
                .activity
                .unwrap()
                .render_capture
                .unwrap()
                .publication
                .is_none()
        );
        drop(reporter);
    }

    #[test]
    fn replacement_capture_identity_does_not_inherit_old_pending_or_retirement() {
        let probe = Probe::new();
        let observer = probe.observer();
        let first = probe.bind_render_capture(1, true, None);
        first.requested(1, first.shared.origin);
        assert!(
            observer
                .snapshot()
                .render_capture
                .unwrap()
                .pending_age_ms
                .is_some()
        );
        let second = probe.bind_render_capture(2, false, None);
        drop(first);
        let current = observer.snapshot().render_capture.unwrap();
        assert_eq!(current.script_id, 2);
        assert!(!current.enabled && !current.retired);
        assert_eq!(current.requested_generation, 0);
        assert_eq!(current.pending_age_ms, None);
        drop(second);
        assert!(observer.snapshot().render_capture.unwrap().retired);
    }

    #[test]
    fn independent_reader_sees_entry_return_hidden_ui_and_owner_retirement() {
        let probe = Probe::new();
        let observer = probe.observer();
        let initial = observer.snapshot().observation.unwrap();
        assert_eq!(initial.phase, None);
        assert_eq!(initial.phase_age_ms, None);
        assert_eq!(initial.last_return_age_ms, None);
        let guard = probe.enter(Callback::Logic);
        let entry = observer.snapshot().observation.unwrap();
        assert!(entry.in_callback);
        assert_eq!(
            entry.logic,
            Counter {
                entered: 1,
                returned: 0,
                unwound: 0
            }
        );
        let later = observer.snapshot_at(Instant::now() + Duration::from_secs(5));
        assert!(later.observation.unwrap().phase_age_ms.unwrap() >= 5000);
        drop(guard);
        for _ in 0..3 {
            drop(probe.enter(Callback::Logic));
        }
        let returned = observer.snapshot().observation.unwrap();
        assert!(!returned.in_callback);
        assert_eq!(returned.logic.returned, 4);
        assert_eq!(
            returned.ui.entered, 0,
            "logic can run without painting a hidden UI"
        );
        drop(probe);
        assert!(observer.snapshot().observation.unwrap().closed);
    }
    #[test]
    fn panic_unwind_is_not_a_normal_callback_return() {
        let probe = Probe::new();
        let observer = probe.observer();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = probe.enter(Callback::Ui);
            panic!("controlled callback unwind");
        }));
        assert!(result.is_err());
        let state = observer.snapshot().observation.unwrap();
        assert_eq!(
            state.ui,
            Counter {
                entered: 1,
                returned: 0,
                unwound: 1
            }
        );
        assert_eq!(state.last_return_age_ms, None);
        assert!(!state.in_callback);
    }
    #[test]
    fn mixed_or_exhausted_samples_are_unknown_without_waiting() {
        let probe = Probe::new();
        probe.shared.sequence.store(1, Ordering::Relaxed);
        assert!(!probe.observer().snapshot().coherent);
        assert!(probe.observer().snapshot().observation.is_none());
        probe.shared.sequence.store(0, Ordering::Relaxed);
        probe.shared.counters[0]
            .entered
            .store(u64::MAX, Ordering::Relaxed);
        drop(probe.enter(Callback::RawInput));
        assert!(probe.observer().snapshot().observation.is_none());
        let probe = Probe::new();
        let _outer = probe.enter(Callback::Logic);
        let _unexpected_nested = probe.enter(Callback::Ui);
        assert!(probe.observer().snapshot().observation.is_none());
    }
    #[test]
    fn receipt_aging_is_saturating_and_does_not_invent_progress() {
        let probe = Probe::new();
        drop(probe.enter(Callback::Logic));
        let mut sample = probe.observer().snapshot();
        let counts = sample.observation.as_ref().unwrap().logic.clone();
        sample.age_by(Duration::from_millis(300));
        assert_eq!(sample.sample_age_ms, 300);
        assert!(
            sample
                .observation
                .as_ref()
                .unwrap()
                .last_return_age_ms
                .unwrap()
                >= 300
        );
        sample.age_by(Duration::from_millis(u64::MAX));
        assert_eq!(sample.sample_age_ms, u64::MAX);
        assert_eq!(
            sample.observation.as_ref().unwrap().phase_age_ms,
            Some(u64::MAX)
        );
        assert_eq!(sample.observation.unwrap().logic, counts);
    }
}
