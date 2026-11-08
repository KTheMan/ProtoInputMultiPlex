//! Bounded, in-memory scheduling for delayed and paced replication copies.
use crate::routing_engine::ProgramOrigin;
use anyhow::{Result, ensure};
use mux_core::automation::Action;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    time::{Duration, Instant},
};

pub const QUEUE_CAPACITY: usize = 4096;
pub const QUEUE_BYTE_CAPACITY: usize = 8 * 1024 * 1024;
const ACTION_BASE_BYTES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Overflow;

impl fmt::Display for Overflow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("replication queue capacity exceeded")
    }
}

impl Error for Overflow {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub pending_events: usize,
    pub pending_motion: usize,
    pub pending_bytes: usize,
    pub capacity: usize,
    pub byte_capacity: usize,
    pub suspended: bool,
    pub error: Option<String>,
    pub emitted: u64,
    pub coalesced: u64,
    pub cancelled: u64,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            pending_events: 0,
            pending_motion: 0,
            pending_bytes: 0,
            capacity: QUEUE_CAPACITY,
            byte_capacity: QUEUE_BYTE_CAPACITY,
            suspended: false,
            error: None,
            emitted: 0,
            coalesced: 0,
            cancelled: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Delivery {
    pub action: Action,
    pub origin: ProgramOrigin,
    pub source: usize,
    pub path: Vec<usize>,
}

#[derive(Debug, Clone, Copy)]
pub struct MotionRequest {
    pub user: usize,
    pub goal: (i32, i32),
    pub start: (i32, i32),
    pub origin: ProgramOrigin,
    pub source: usize,
    pub due: Instant,
    pub interval: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct MotionKey {
    origin: ProgramOrigin,
    source: usize,
    target: usize,
}

#[derive(Debug, Clone)]
struct MotionJob {
    path: Vec<usize>,
    bytes: usize,
    current: (i32, i32),
    goal: (i32, i32),
    interval: Duration,
    due: Instant,
    sequence: u64,
}

#[derive(Debug, Clone)]
struct EventJob {
    path: Vec<usize>,
    action: Action,
    origin: ProgramOrigin,
    source: usize,
    due: Instant,
    sequence: u64,
    bytes: usize,
}

#[derive(Debug, Clone, Copy)]
enum Candidate {
    Event(u64),
    Motion(MotionKey),
}

#[derive(Debug, Clone, Default)]
pub struct Queue {
    events: Vec<EventJob>,
    motion: BTreeMap<MotionKey, MotionJob>,
    cadence: BTreeMap<MotionKey, Instant>,
    cadence_paths: BTreeMap<MotionKey, Vec<usize>>,
    pending_bytes: usize,
    next_sequence: u64,
    suspended: bool,
    error: Option<String>,
    emitted: u64,
    coalesced: u64,
    cancelled: u64,
}

impl Queue {
    #[cfg(test)]
    pub fn push_event(
        &mut self,
        action: Action,
        origin: ProgramOrigin,
        source: usize,
        due: Instant,
    ) -> Result<()> {
        self.push_event_via(action, origin, source, due, &[])
    }
    pub fn push_event_via(
        &mut self,
        action: Action,
        origin: ProgramOrigin,
        source: usize,
        due: Instant,
        path: &[usize],
    ) -> Result<()> {
        self.ensure_running()?;
        let bytes = action_bytes(&action)?
            .checked_add(std::mem::size_of_val(path))
            .ok_or(Overflow)?;
        self.ensure_capacity(1, bytes)?;
        let sequence = self.allocate_sequence()?;
        self.events.push(EventJob {
            path: path.to_vec(),
            action,
            origin,
            source,
            due,
            sequence,
            bytes,
        });
        self.pending_bytes += bytes;
        let key = MotionKey {
            origin,
            source,
            target: self.events.last().unwrap().action.user(),
        };
        if self.cadence.contains_key(&key) {
            self.cadence_paths.insert(key, path.to_vec());
        }
        Ok(())
    }

    /// Add or update one paced motion stream. Replacements retain progress and the first
    /// deadline of the current segment while moving its final goal to the newest sample.
    #[cfg(test)]
    pub fn push_motion(&mut self, request: MotionRequest) -> Result<()> {
        self.push_motion_via(request, &[])
    }
    pub fn push_motion_via(&mut self, request: MotionRequest, path: &[usize]) -> Result<()> {
        let MotionRequest {
            user,
            goal,
            start,
            origin,
            source,
            due,
            interval,
        } = request;
        self.ensure_running()?;
        ensure!(
            !interval.is_zero() && interval <= Duration::from_secs(1),
            "paced motion interval must be between 1 and 1000 ms"
        );
        let key = MotionKey {
            origin,
            source,
            target: user,
        };
        if self.motion.contains_key(&key) {
            if self.motion[&key].current == goal {
                self.pending_bytes -= self.motion.remove(&key).unwrap().bytes;
            } else {
                let job = self.motion.get_mut(&key).expect("motion key still exists");
                job.goal = goal;
                job.interval = interval;
            }
            self.coalesced = self.coalesced.saturating_add(1);
            return Ok(());
        }
        if start == goal {
            return Ok(());
        }
        let bytes = ACTION_BASE_BYTES
            .checked_add(std::mem::size_of_val(path))
            .ok_or(Overflow)?;
        self.ensure_capacity(1, bytes)?;
        let sequence = self.allocate_sequence()?;
        self.motion.insert(
            key,
            MotionJob {
                path: path.to_vec(),
                bytes,
                current: start,
                goal,
                interval,
                due,
                sequence,
            },
        );
        self.pending_bytes += bytes;
        Ok(())
    }

    /// End a motion coalescing segment before a discrete event for the same route.
    /// The final point receives a sequence number before the caller queues that event.
    pub fn barrier(
        &mut self,
        origin: ProgramOrigin,
        source: usize,
        target: usize,
        due: Instant,
    ) -> Result<()> {
        self.ensure_running()?;
        let key = MotionKey {
            origin,
            source,
            target,
        };
        let Some((current, goal)) = self.motion.get(&key).map(|job| (job.current, job.goal)) else {
            return Ok(());
        };
        if current == goal {
            self.pending_bytes -= self.motion.remove(&key).unwrap().bytes;
            return Ok(());
        }

        // Replacing the motion slot with its final point has the same count and byte cost.
        let sequence = self.allocate_sequence()?;
        let job = self.motion.remove(&key).unwrap();
        self.events.push(EventJob {
            path: job.path,
            action: Action::MoveTo {
                user: target,
                x: goal.0,
                y: goal.1,
            },
            origin,
            source,
            due,
            sequence,
            bytes: job.bytes,
        });
        Ok(())
    }

    /// Check a batch before the caller emits its source action. This is intentionally
    /// conservative; callers can avoid charging coalescing motion replacements.
    pub fn ensure_capacity(&self, event_count: usize, bytes: usize) -> Result<()> {
        if self.suspended {
            anyhow::bail!("replication queue is suspended");
        }
        let pending = self.events.len().saturating_add(self.motion.len());
        let count = pending.checked_add(event_count).ok_or(Overflow)?;
        let total_bytes = self.pending_bytes.checked_add(bytes).ok_or(Overflow)?;
        if count > QUEUE_CAPACITY || total_bytes > QUEUE_BYTE_CAPACITY {
            return Err(Overflow.into());
        }
        Ok(())
    }

    pub fn has_motion(&self, origin: ProgramOrigin, source: usize, target: usize) -> bool {
        self.motion.contains_key(&MotionKey {
            origin,
            source,
            target,
        })
    }

    /// Return the latest not-yet-delivered destination point for this route, including
    /// the final point retained as a discrete barrier event.
    pub fn goal(&self, origin: ProgramOrigin, source: usize, target: usize) -> Option<(i32, i32)> {
        let motion = self
            .motion
            .get(&MotionKey {
                origin,
                source,
                target,
            })
            .map(|job| (job.sequence, job.goal));
        let event = self
            .events
            .iter()
            .filter(|job| job.origin == origin && job.source == source)
            .filter_map(|job| match &job.action {
                Action::MoveTo { user, x, y } if *user == target => Some((job.sequence, (*x, *y))),
                _ => None,
            })
            .max_by_key(|(sequence, _)| *sequence);
        match (motion, event) {
            (Some(motion), Some(event)) => Some(if motion.0 > event.0 {
                motion.1
            } else {
                event.1
            }),
            (Some((_, goal)), None) | (None, Some((_, goal))) => Some(goal),
            (None, None) => None,
        }
    }

    /// Reserve the next temporal slot shared by all event components for this route.
    /// With a positive interval, the first event is due now and later events receive
    /// monotonically spaced deadlines. This is cadence, not motion scaling.
    pub fn next_slot(
        &mut self,
        origin: ProgramOrigin,
        source: usize,
        target: usize,
        requested_due: Instant,
        interval: Duration,
        clock: Instant,
    ) -> Result<Instant> {
        self.ensure_running()?;
        ensure!(
            interval <= Duration::from_secs(1),
            "replication step interval exceeds 1000 ms"
        );
        self.cadence.retain(|_, slot| *slot > clock);
        self.cadence_paths
            .retain(|key, _| self.cadence.contains_key(key));
        let key = MotionKey {
            origin,
            source,
            target,
        };
        if interval.is_zero() {
            return Ok(requested_due);
        }
        self.ensure_slots(&[(origin, source, target)], clock)?;
        let slot = self
            .cadence
            .get(&key)
            .copied()
            .unwrap_or(requested_due)
            .max(requested_due);
        self.cadence.insert(
            key,
            slot.checked_add(interval)
                .ok_or_else(|| anyhow::anyhow!("replication cadence deadline overflow"))?,
        );
        Ok(slot)
    }

    /// Preflight a set of cadence reservations before the source event is emitted.
    /// Expired reservations do not consume capacity; duplicate requested keys count once.
    pub fn ensure_slots(&self, keys: &[(ProgramOrigin, usize, usize)], now: Instant) -> Result<()> {
        if keys.len() > QUEUE_CAPACITY {
            return Err(Overflow.into());
        }
        let live = self.cadence.values().filter(|slot| **slot > now).count();
        let live_keys: std::collections::BTreeSet<_> = self
            .cadence
            .iter()
            .filter_map(|(key, slot)| (*slot > now).then_some(*key))
            .collect();
        let requested: std::collections::BTreeSet<_> = keys
            .iter()
            .map(|(origin, source, target)| MotionKey {
                origin: *origin,
                source: *source,
                target: *target,
            })
            .filter(|key| !live_keys.contains(key))
            .collect();
        let count = live.checked_add(requested.len()).ok_or(Overflow)?;
        if count > QUEUE_CAPACITY {
            return Err(Overflow.into());
        }
        Ok(())
    }

    /// Emit due work in `(deadline, insertion order)` order without reordering a target.
    pub fn take_due(&mut self, now: Instant, budget: usize) -> Vec<Delivery> {
        self.cadence.retain(|_, slot| *slot > now);
        self.cadence_paths
            .retain(|key, _| self.cadence.contains_key(key));
        if self.suspended || budget == 0 {
            return Vec::new();
        }
        let mut candidates = Vec::<(Instant, u64, Candidate)>::new();
        candidates.extend(
            self.events
                .iter()
                .filter(|job| job.due <= now)
                .map(|job| (job.due, job.sequence, Candidate::Event(job.sequence))),
        );
        candidates.extend(self.motion.iter().filter_map(|(key, job)| {
            (job.due <= now).then_some((job.due, job.sequence, Candidate::Motion(*key)))
        }));
        candidates.sort_by_key(|(due, sequence, _)| (*due, *sequence));

        let mut output = Vec::with_capacity(budget.min(candidates.len()));
        for (_, _, candidate) in candidates.into_iter().take(budget) {
            match candidate {
                Candidate::Event(sequence) => {
                    let Some(index) = self.events.iter().position(|job| job.sequence == sequence)
                    else {
                        continue;
                    };
                    let job = self.events.remove(index);
                    self.pending_bytes -= job.bytes;
                    output.push(Delivery {
                        path: job.path,
                        action: job.action,
                        origin: job.origin,
                        source: job.source,
                    });
                }
                Candidate::Motion(key) => {
                    let Some(mut job) = self.motion.remove(&key) else {
                        continue;
                    };
                    let next = approach_half(job.current, job.goal);
                    let complete = next == job.goal;
                    output.push(Delivery {
                        path: job.path.clone(),
                        action: Action::MoveTo {
                            user: key.target,
                            x: next.0,
                            y: next.1,
                        },
                        origin: key.origin,
                        source: key.source,
                    });
                    if complete {
                        self.pending_bytes -= job.bytes;
                    } else {
                        job.current = next;
                        job.due = job
                            .due
                            .checked_add(job.interval)
                            .filter(|deadline| *deadline > now)
                            .unwrap_or_else(|| now + job.interval);
                        self.motion.insert(key, job);
                    }
                }
            }
        }
        self.emitted = self.emitted.saturating_add(output.len() as u64);
        output
    }

    pub fn has_origin(&self, origin: ProgramOrigin) -> bool {
        self.events.iter().any(|job| job.origin == origin)
            || self.motion.keys().any(|key| key.origin == origin)
    }

    pub fn cancel_origin(&mut self, origin: ProgramOrigin) {
        let before_events = self.events.len();
        let mut removed_bytes = 0usize;
        self.events.retain(|job| {
            if job.origin == origin {
                removed_bytes += job.bytes;
                false
            } else {
                true
            }
        });
        let before_motion = self.motion.len();
        self.motion.retain(|key, job| {
            if key.origin == origin {
                removed_bytes += job.bytes;
                false
            } else {
                true
            }
        });
        self.pending_bytes -= removed_bytes;
        self.cadence.retain(|key, _| key.origin != origin);
        self.cadence_paths.retain(|key, _| key.origin != origin);
        self.cancelled = self.cancelled.saturating_add(
            (before_events - self.events.len() + before_motion - self.motion.len()) as u64,
        );
    }

    pub fn cancel_all(&mut self) {
        let removed = self.events.len() + self.motion.len();
        self.events.clear();
        self.motion.clear();
        self.cadence.clear();
        self.cadence_paths.clear();
        self.pending_bytes = 0;
        self.cancelled = self.cancelled.saturating_add(removed as u64);
    }

    /// A keyboard configuration boundary retires only raw keyboard/text copies.
    /// Mouse-origin actions (including later mouse-to-key conversion) remain independent.
    pub fn cancel_keyboard_user(&mut self, user: usize) {
        let before = self.events.len();
        let mut removed_bytes = 0;
        self.events.retain(|job| {
            let remove = crate::automation_runtime::keyboard_action(&job.action)
                && (job.source == user || job.action.user() == user || job.path.contains(&user));
            if remove {
                removed_bytes += job.bytes;
            }
            !remove
        });
        self.pending_bytes -= removed_bytes;
        self.cancelled = self
            .cancelled
            .saturating_add((before - self.events.len()) as u64);
    }
    /// Inspect affected routes without draining jobs or pruning cadence state.
    /// A delivered event can still reserve a future slot along its entire path.
    pub fn has_user_work(&self, user: usize, now: Instant) -> bool {
        self.events
            .iter()
            .any(|job| job.source == user || job.action.user() == user || job.path.contains(&user))
            || self.motion.iter().any(|(key, job)| {
                key.source == user || key.target == user || job.path.contains(&user)
            })
            || self.cadence.iter().any(|(key, due)| {
                *due > now
                    && (key.source == user
                        || key.target == user
                        || self
                            .cadence_paths
                            .get(key)
                            .is_some_and(|path| path.contains(&user)))
            })
    }
    pub fn cancel_user(&mut self, user: usize) {
        let before = self.events.len() + self.motion.len();
        let mut bytes = 0;
        self.events.retain(|job| {
            let remove =
                job.source == user || job.action.user() == user || job.path.contains(&user);
            if remove {
                bytes += job.bytes;
            }
            !remove
        });
        self.motion.retain(|key, job| {
            let remove = key.source == user || key.target == user || job.path.contains(&user);
            if remove {
                bytes += job.bytes;
            }
            !remove
        });
        self.cadence.retain(|key, _| {
            key.source != user
                && key.target != user
                && !self
                    .cadence_paths
                    .get(key)
                    .is_some_and(|path| path.contains(&user))
        });
        self.cadence_paths
            .retain(|key, _| self.cadence.contains_key(key));
        self.pending_bytes -= bytes;
        self.cancelled = self
            .cancelled
            .saturating_add((before - self.events.len() - self.motion.len()) as u64);
    }

    pub fn suspend(&mut self, error: String) {
        self.cancel_all();
        self.suspended = true;
        self.error = Some(error);
    }

    pub fn resume(&mut self) {
        self.suspended = false;
        self.error = None;
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            pending_events: self.events.len() + self.motion.len(),
            pending_motion: self.motion.len(),
            pending_bytes: self.pending_bytes,
            capacity: QUEUE_CAPACITY,
            byte_capacity: QUEUE_BYTE_CAPACITY,
            suspended: self.suspended,
            error: self.error.clone(),
            emitted: self.emitted,
            coalesced: self.coalesced,
            cancelled: self.cancelled,
        }
    }

    fn ensure_running(&self) -> Result<()> {
        ensure!(!self.suspended, "replication queue is suspended");
        Ok(())
    }

    fn allocate_sequence(&mut self) -> Result<u64> {
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("replication queue sequence exhausted"))?;
        Ok(sequence)
    }
}

fn action_bytes(action: &Action) -> Result<usize> {
    match action {
        Action::Text { text, .. } => ACTION_BASE_BYTES
            .checked_add(text.len())
            .ok_or_else(|| Overflow.into()),
        _ => Ok(ACTION_BASE_BYTES),
    }
}

fn approach_half(current: (i32, i32), goal: (i32, i32)) -> (i32, i32) {
    fn axis(current: i32, goal: i32) -> i32 {
        let delta = i64::from(goal) - i64::from(current);
        if delta.unsigned_abs() <= 1 {
            return goal;
        }
        let step = delta / 2;
        (i64::from(current) + if step == 0 { delta.signum() } else { step }) as i32
    }
    (axis(current.0, goal.0), axis(current.1, goal.1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mux_core::emulated::Packet;

    #[test]
    fn selection_inspection_preserves_future_events_and_all_affected_users() {
        let now = Instant::now();
        let due = now + Duration::from_secs(60);
        let mut queue = Queue::default();
        queue
            .push_event_via(action("later"), origin(0), 0, due, &[0, 1, 2])
            .unwrap();
        let before = format!("{queue:?}");
        for user in [0, 1, 2] {
            assert!(queue.has_user_work(user, now));
        }
        assert!(!queue.has_user_work(3, now));
        assert_eq!(format!("{queue:?}"), before);
        assert!(queue.take_due(now, 8).is_empty());
        let delivered = queue.take_due(due, 8);
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].path, vec![0, 1, 2]);
        assert!(matches!(&delivered[0].action, Action::Text { text, .. } if text == "later"));
    }

    #[test]
    fn selection_inspection_preserves_future_motion_progress_and_path() {
        let now = Instant::now();
        let due = now + Duration::from_secs(60);
        let mut queue = Queue::default();
        queue
            .push_motion_via(
                MotionRequest {
                    user: 2,
                    source: 0,
                    origin: origin(0),
                    start: (0, 0),
                    goal: (100, 0),
                    due,
                    interval: Duration::from_millis(25),
                },
                &[0, 1, 2],
            )
            .unwrap();
        let before = format!("{queue:?}");
        for user in [0, 1, 2] {
            assert!(queue.has_user_work(user, now));
        }
        assert!(!queue.has_user_work(3, now));
        assert_eq!(format!("{queue:?}"), before);
        let delivered = queue.take_due(due, 1);
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].path, vec![0, 1, 2]);
        assert!(matches!(
            &delivered[0].action,
            Action::MoveTo {
                user: 2,
                x: 50,
                y: 0
            }
        ));
    }

    #[test]
    fn selection_inspection_checks_cadence_after_delivery_without_pruning_expired_paths() {
        let now = Instant::now();
        let interval = Duration::from_secs(1);
        let mut queue = Queue::default();
        let due = queue
            .next_slot(origin(0), 0, 2, now, interval, now)
            .unwrap();
        queue
            .push_event_via(action("first"), origin(0), 0, due, &[0, 1, 2])
            .unwrap();
        assert_eq!(queue.take_due(now, 1).len(), 1);
        assert_eq!(queue.snapshot().pending_events, 0);
        let before = format!("{queue:?}");
        for user in [0, 1, 2] {
            assert!(queue.has_user_work(user, now));
            assert!(!queue.has_user_work(user, now + interval));
        }
        assert!(!queue.has_user_work(3, now));
        assert_eq!(format!("{queue:?}"), before);
        assert_eq!(
            queue
                .next_slot(origin(0), 0, 2, now, interval, now)
                .unwrap(),
            now + interval
        );
    }

    fn action(text: &str) -> Action {
        Action::Text {
            user: 2,
            text: text.to_owned(),
        }
    }

    fn origin(user: usize) -> ProgramOrigin {
        ProgramOrigin::Studio(user)
    }

    #[test]
    fn retiring_an_intermediate_user_cancels_paths_without_touching_other_sources() {
        let now = Instant::now();
        let mut queue = Queue::default();
        queue
            .push_event_via(action("through clone"), origin(0), 0, now, &[0, 1, 2])
            .unwrap();
        queue
            .push_event_via(action("independent"), origin(3), 3, now, &[3, 2])
            .unwrap();
        queue
            .push_motion_via(
                MotionRequest {
                    user: 2,
                    goal: (80, 0),
                    start: (0, 0),
                    origin: origin(0),
                    source: 0,
                    due: now,
                    interval: Duration::from_millis(25),
                },
                &[0, 1, 2],
            )
            .unwrap();
        queue
            .next_slot(origin(0), 0, 2, now, Duration::from_millis(100), now)
            .unwrap();
        queue.cadence_paths.insert(
            MotionKey {
                origin: origin(0),
                source: 0,
                target: 2,
            },
            vec![0, 1, 2],
        );
        let independent_bytes =
            action_bytes(&action("independent")).unwrap() + 2 * std::mem::size_of::<usize>();
        queue.cancel_user(1);
        assert_eq!(queue.snapshot().pending_events, 1);
        assert_eq!(queue.snapshot().pending_bytes, independent_bytes);
        assert_eq!(queue.snapshot().cancelled, 2);
        assert!(queue.cadence.is_empty());
        let due = queue.take_due(now, 64);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].source, 3);
        assert_eq!(due[0].path, vec![3, 2]);
        assert_eq!(queue.snapshot().pending_bytes, 0);
    }

    #[test]
    fn motion_coalesces_latest_goal_and_advances_with_bounded_steps() {
        let now = Instant::now();
        let mut queue = Queue::default();
        queue
            .push_motion(MotionRequest {
                user: 3,
                goal: (80, 0),
                start: (0, 0),
                origin: origin(1),
                source: 1,
                due: now,
                interval: Duration::from_millis(10),
            })
            .unwrap();
        queue
            .push_motion(MotionRequest {
                user: 3,
                goal: (100, 0),
                start: (77, 77),
                origin: origin(1),
                source: 1,
                due: now,
                interval: Duration::from_millis(10),
            })
            .unwrap();
        assert_eq!(queue.snapshot().pending_events, 1);
        assert_eq!(queue.snapshot().pending_motion, 1);
        assert_eq!(queue.snapshot().coalesced, 1);
        assert!(queue.has_motion(origin(1), 1, 3));

        let first = queue.take_due(now, 1);
        assert!(matches!(
            &first[0].action,
            Action::MoveTo {
                user: 3,
                x: 50,
                y: 0
            }
        ));
        let second = queue.take_due(now + Duration::from_millis(10), 1);
        assert!(matches!(&second[0].action, Action::MoveTo { x: 75, .. }));
    }

    #[test]
    fn barrier_flushes_final_motion_before_discrete_work_and_starts_new_segment() {
        let now = Instant::now();
        let at = now + Duration::from_millis(20);
        let mut queue = Queue::default();
        queue
            .push_motion(MotionRequest {
                user: 4,
                goal: (100, 50),
                start: (0, 0),
                origin: origin(1),
                source: 0,
                due: now,
                interval: Duration::from_millis(10),
            })
            .unwrap();
        queue.barrier(origin(1), 0, 4, at).unwrap();
        assert_eq!(queue.goal(origin(1), 0, 4), Some((100, 50)));
        queue
            .push_event(action("boundary"), origin(1), 0, at)
            .unwrap();
        assert!(!queue.has_motion(origin(1), 0, 4));
        let deliveries = queue.take_due(at, 8);
        assert_eq!(deliveries.len(), 2);
        assert!(matches!(
            &deliveries[0].action,
            Action::MoveTo {
                user: 4,
                x: 100,
                y: 50
            }
        ));
        assert!(matches!(&deliveries[1].action, Action::Text { text, .. } if text == "boundary"));

        queue
            .push_motion(MotionRequest {
                user: 4,
                goal: (0, 0),
                start: (100, 50),
                origin: origin(1),
                source: 0,
                due: at,
                interval: Duration::from_millis(10),
            })
            .unwrap();
        assert_eq!(queue.snapshot().coalesced, 0);
    }

    #[test]
    fn cancellation_releases_capacity_by_origin_and_suspension_rejects_work() {
        let now = Instant::now();
        let mut queue = Queue::default();
        queue
            .push_event(action("owned"), origin(1), 0, now)
            .unwrap();
        queue
            .push_event(action("other"), origin(2), 0, now)
            .unwrap();
        let before = queue.snapshot();
        assert_eq!(before.pending_bytes, ACTION_BASE_BYTES * 2 + 10);
        assert!(queue.has_origin(origin(1)));
        queue.cancel_origin(origin(1));
        assert!(!queue.has_origin(origin(1)));
        assert!(queue.has_origin(origin(2)));
        assert_eq!(queue.snapshot().pending_events, 1);
        assert_eq!(queue.snapshot().pending_bytes, ACTION_BASE_BYTES + 5);
        queue.suspend("route failed".into());
        assert!(queue.snapshot().suspended);
        assert_eq!(queue.snapshot().pending_events, 0);
        assert!(
            queue
                .push_event(action("blocked"), origin(2), 0, now)
                .is_err()
        );
        queue.resume();
        queue
            .push_event(action("resumed"), origin(2), 0, now)
            .unwrap();
    }

    #[test]
    fn overflow_is_typed_atomic_and_keeps_an_admitted_release_event() {
        let now = Instant::now();
        let mut queue = Queue::default();
        for index in 0..QUEUE_CAPACITY - 1 {
            queue
                .push_event(
                    Action::MoveTo {
                        user: 0,
                        x: index as i32,
                        y: 0,
                    },
                    origin(1),
                    0,
                    now,
                )
                .unwrap();
        }
        let release = Action::Packet {
            user: 0,
            packet: Packet::key(0xF1000003, 0x41, true),
        };
        queue.push_event(release, origin(1), 0, now).unwrap();
        let error = queue
            .push_event(action("overflow"), origin(1), 0, now)
            .unwrap_err();
        assert!(error.downcast_ref::<Overflow>().is_some());
        assert_eq!(queue.snapshot().pending_events, QUEUE_CAPACITY);
        let delivered = queue.take_due(now, QUEUE_CAPACITY);
        assert_eq!(delivered.len(), QUEUE_CAPACITY);
        assert!(matches!(
            &delivered.last().unwrap().action,
            Action::Packet { packet, .. } if packet.kind == 1 && packet.flags & 1 == 1
        ));
    }

    #[test]
    fn byte_budget_rejects_without_mutating_admitted_actions() {
        let now = Instant::now();
        let mut queue = Queue::default();
        let payload = "x".repeat(4000);
        for _ in 0..2032 {
            queue
                .push_event(action(&payload), origin(1), 0, now)
                .unwrap();
        }
        let before = queue.snapshot();
        assert!(before.pending_bytes <= QUEUE_BYTE_CAPACITY);
        let too_large = "y".repeat(600);
        let error = queue
            .push_event(action(&too_large), origin(1), 0, now)
            .unwrap_err();
        assert!(error.downcast_ref::<Overflow>().is_some());
        assert_eq!(queue.snapshot().pending_events, before.pending_events);
        assert_eq!(queue.snapshot().pending_bytes, before.pending_bytes);
    }

    #[test]
    fn route_slots_preserve_order_across_event_components() {
        let now = Instant::now();
        let mut queue = Queue::default();
        let interval = Duration::from_millis(25);
        assert_eq!(
            queue
                .next_slot(origin(1), 0, 2, now, interval, now)
                .unwrap(),
            now
        );
        assert_eq!(
            queue
                .next_slot(origin(1), 0, 2, now, interval, now)
                .unwrap(),
            now + interval
        );
        assert_eq!(
            queue
                .next_slot(origin(1), 0, 2, now, Duration::ZERO, now)
                .unwrap(),
            now
        );
    }

    #[test]
    fn slot_batch_admission_counts_unique_keys_without_future_clock_pruning() {
        let now = Instant::now();
        let mut queue = Queue::default();
        for index in 0..QUEUE_CAPACITY {
            queue.cadence.insert(
                MotionKey {
                    origin: ProgramOrigin::Studio(index),
                    source: index,
                    target: index,
                },
                now + Duration::from_secs(1),
            );
        }
        let duplicate = (origin(0), 0, 0);
        assert!(queue.ensure_slots(&[duplicate, duplicate], now).is_ok());
        let additional = (origin(QUEUE_CAPACITY), QUEUE_CAPACITY, QUEUE_CAPACITY);
        assert!(queue.ensure_slots(&[additional], now).is_err());
        assert_eq!(queue.cadence.len(), QUEUE_CAPACITY);

        let future_request = now + Duration::from_secs(30);
        let error = queue
            .next_slot(
                ProgramOrigin::Studio(QUEUE_CAPACITY + 1),
                1,
                2,
                future_request,
                Duration::from_millis(10),
                now,
            )
            .unwrap_err();
        assert!(error.downcast_ref::<Overflow>().is_some());
        assert_eq!(queue.cadence.len(), QUEUE_CAPACITY);
    }
}
