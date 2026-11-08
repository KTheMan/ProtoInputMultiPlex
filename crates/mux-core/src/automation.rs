//! Portable macro capture/playback and deterministic action replication.
use crate::emulated::Packet;
use anyhow::{Result, bail, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::time::Duration;

pub const MAX_CLIP_EVENTS: usize = 100_000;
pub const MAX_CLIP_DURATION_MICROS: u64 = 24 * 60 * 60 * 1_000_000;
pub const MAX_CLIP_JSON_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_USER_ID: usize = 0x7F_FFFF;
pub const MAX_TEXT_UTF16_UNITS: usize = 4096;
pub const DEFAULT_ACTIONS_PER_TICK: usize = 256;
pub const MAX_REPLICATION_EDGES: usize = 4096;
const MAX_PLAYBACK_LOOPS: u32 = 1_000_000;
const MAX_EVENT_CACHE: usize = 4096;

/// An authored action. Device identity translation is deliberately left to the caller.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Packet { user: usize, packet: Packet },
    MoveTo { user: usize, x: i32, y: i32 },
    Text { user: usize, text: String },
}

impl Action {
    pub fn user(&self) -> usize {
        match self {
            Self::Packet { user, .. } | Self::MoveTo { user, .. } | Self::Text { user, .. } => {
                *user
            }
        }
    }

    /// Returns this action addressed to another user, preserving its payload.
    pub fn with_user(&self, user: usize) -> Self {
        self.for_user(user)
    }

    fn validate(&self) -> Result<()> {
        ensure!(self.user() <= MAX_USER_ID, "user ID is out of range");
        if let Self::Packet { packet, .. } = self {
            packet.validate()?;
            ensure!(
                packet.kind != 2,
                "text packets must be recorded as a text action"
            );
        }
        if let Self::Text { text, .. } = self {
            ensure!(!text.is_empty(), "text action is empty");
            ensure!(
                text.encode_utf16().count() <= MAX_TEXT_UTF16_UNITS,
                "text action is too long"
            );
            ensure!(
                text.chars()
                    .all(|character| !character.is_control() || matches!(character, '\n' | '\t')),
                "text action contains an unsupported control character"
            );
        }
        Ok(())
    }

    fn category(&self) -> Result<ActionCategory> {
        match self {
            Self::MoveTo { .. } => Ok(ActionCategory::Mouse),
            Self::Text { .. } => Ok(ActionCategory::Text),
            Self::Packet { packet, .. } => match packet.kind {
                0 => Ok(ActionCategory::Mouse),
                1 => Ok(ActionCategory::Keyboard),
                2 => Ok(ActionCategory::Text),
                kind => bail!("unsupported packet kind {kind}"),
            },
        }
    }

    pub fn for_user(&self, user: usize) -> Self {
        match self {
            Self::Packet { packet, .. } => Self::Packet {
                user,
                packet: *packet,
            },
            Self::MoveTo { x, y, .. } => Self::MoveTo { user, x: *x, y: *y },
            Self::Text { text, .. } => Self::Text {
                user,
                text: text.clone(),
            },
        }
    }
}

fn estimated_action_json_bytes(action: &Action) -> usize {
    256usize.saturating_add(match action {
        Action::Text { text, .. } => text.len().saturating_mul(2),
        Action::Packet { .. } | Action::MoveTo { .. } => 0,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimedAction {
    /// Timestamp relative to the beginning of the clip, in microseconds.
    pub at_micros: u64,
    pub action: Action,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    pub version: u32,
    pub events: Vec<TimedAction>,
}

impl Clip {
    pub fn new(events: Vec<TimedAction>) -> Result<Self> {
        let clip = Self { version: 1, events };
        clip.validate()?;
        Ok(clip)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1,
            "unsupported clip version {}",
            self.version
        );
        ensure!(!self.events.is_empty(), "clip has no events");
        ensure!(
            self.events.len() <= MAX_CLIP_EVENTS,
            "clip has too many events"
        );
        let mut previous = 0;
        let mut estimated_json_bytes = 0usize;
        for (index, event) in self.events.iter().enumerate() {
            ensure!(
                event.at_micros <= MAX_CLIP_DURATION_MICROS,
                "clip exceeds the maximum duration"
            );
            ensure!(
                index == 0 || event.at_micros >= previous,
                "clip timestamps must be ordered"
            );
            event.action.validate()?;
            estimated_json_bytes =
                estimated_json_bytes.saturating_add(estimated_action_json_bytes(&event.action));
            previous = event.at_micros;
        }
        ensure!(
            estimated_json_bytes <= MAX_CLIP_JSON_BYTES,
            "clip exceeds the maximum serialized size"
        );
        Ok(())
    }

    pub fn to_json(&self) -> Result<String> {
        self.validate()?;
        let json = serde_json::to_string(self)?;
        ensure!(
            json.len() <= MAX_CLIP_JSON_BYTES,
            "clip exceeds the maximum serialized size"
        );
        Ok(json)
    }

    pub fn from_json(json: &str) -> Result<Self> {
        ensure!(json.len() <= MAX_CLIP_JSON_BYTES, "clip JSON is too large");
        let clip: Self = serde_json::from_str(json)?;
        clip.validate()?;
        Ok(clip)
    }

    /// Returns unique users referenced by the clip, in ascending order.
    pub fn affected_users(&self) -> Vec<usize> {
        self.events
            .iter()
            .map(|event| event.action.user())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn period_micros(&self) -> u64 {
        self.events.last().map_or(1, |e| e.at_micros.max(1))
    }
}

#[derive(Debug, Default)]
pub struct Recorder {
    recording: bool,
    events: Vec<TimedAction>,
    estimated_json_bytes: usize,
}

impl Recorder {
    pub fn start(&mut self) {
        self.events.clear();
        self.estimated_json_bytes = 0;
        self.recording = true;
    }

    pub fn is_recording(&self) -> bool {
        self.recording
    }

    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    /// Validate recording capacity without appending an event or changing state.
    pub fn check_capture(&self, action: &Action, elapsed: Duration) -> Result<()> {
        ensure!(self.recording, "recording is not active");
        ensure!(
            self.events.len() < MAX_CLIP_EVENTS,
            "clip has too many events"
        );
        action.validate()?;
        let micros = u64::try_from(elapsed.as_micros())
            .map_err(|_| anyhow::anyhow!("timestamp overflow"))?;
        ensure!(
            micros <= MAX_CLIP_DURATION_MICROS,
            "clip exceeds the maximum duration"
        );
        ensure!(
            self.events
                .last()
                .is_none_or(|previous| previous.at_micros <= micros),
            "recorded timestamps must be ordered"
        );
        let estimated_json_bytes = self
            .estimated_json_bytes
            .saturating_add(estimated_action_json_bytes(action));
        ensure!(
            estimated_json_bytes <= MAX_CLIP_JSON_BYTES,
            "clip exceeds the maximum serialized size"
        );
        Ok(())
    }

    pub fn capture(&mut self, action: Action, elapsed: Duration) -> Result<()> {
        self.check_capture(&action, elapsed)?;
        let micros = u64::try_from(elapsed.as_micros())?;
        self.estimated_json_bytes += estimated_action_json_bytes(&action);
        self.events.push(TimedAction {
            at_micros: micros,
            action,
        });
        Ok(())
    }

    pub fn stop(&mut self) -> Result<Clip> {
        ensure!(self.recording, "recording is not active");
        self.recording = false;
        self.estimated_json_bytes = 0;
        Clip::new(std::mem::take(&mut self.events))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Playing,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackSnapshot {
    pub state: PlaybackState,
    pub elapsed: Duration,
    pub remaining_delay: Duration,
    pub speed: f64,
    pub loops_completed: u32,
    pub total_loops: Option<u32>,
    pub events_emitted: u64,
}

#[derive(Debug)]
pub struct Playback {
    clip: Clip,
    speed: f64,
    total_loops: Option<u32>,
    elapsed: Duration,
    delay: Duration,
    cycle: u32,
    index: usize,
    state: PlaybackState,
    emitted_users: BTreeSet<usize>,
    events_emitted: u64,
    max_per_tick: usize,
}

impl Playback {
    pub fn start(clip: Clip, total_loops: Option<u32>, speed: f64) -> Result<Self> {
        Self::start_with_delay(clip, total_loops, speed, Duration::ZERO)
    }

    /// Starts playback after an unscaled wall-clock delay.
    pub fn start_with_delay(
        clip: Clip,
        total_loops: Option<u32>,
        speed: f64,
        delay: Duration,
    ) -> Result<Self> {
        clip.validate()?;
        ensure!(
            delay <= Duration::from_micros(MAX_CLIP_DURATION_MICROS),
            "playback delay exceeds the maximum"
        );
        ensure!(
            speed.is_finite() && (0.01..=100.0).contains(&speed),
            "playback speed must be between 0.01 and 100"
        );
        if let Some(loops) = total_loops {
            ensure!(
                (1..=MAX_PLAYBACK_LOOPS).contains(&loops),
                "playback loop count is out of range"
            );
        }
        Ok(Self {
            clip,
            speed,
            total_loops,
            elapsed: Duration::ZERO,
            delay,
            cycle: 0,
            index: 0,
            state: PlaybackState::Playing,
            emitted_users: BTreeSet::new(),
            events_emitted: 0,
            max_per_tick: DEFAULT_ACTIONS_PER_TICK,
        })
    }

    pub fn with_max_actions_per_tick(mut self, limit: usize) -> Result<Self> {
        self.set_max_actions_per_tick(limit)?;
        Ok(self)
    }

    /// Changes the bounded action budget for subsequent scheduler ticks.
    pub fn set_max_actions_per_tick(&mut self, limit: usize) -> Result<()> {
        ensure!(
            limit > 0 && limit <= 100_000,
            "per-tick action limit is out of range"
        );
        self.max_per_tick = limit;
        Ok(())
    }

    /// Advances by wall-clock time and returns up to the configured number of due actions.
    pub fn tick(&mut self, delta: Duration) -> Vec<Action> {
        if self.state != PlaybackState::Playing {
            return Vec::new();
        }
        self.elapsed = self.elapsed.saturating_add(delta);
        if self.elapsed < self.delay {
            return Vec::new();
        }
        let active_elapsed = self.elapsed.saturating_sub(self.delay);
        let played_micros =
            (active_elapsed.as_secs_f64() * self.speed * 1_000_000.0).min(u64::MAX as f64) as u64;
        let period = self.clip.period_micros();
        let mut due = Vec::with_capacity(self.max_per_tick.min(self.clip.events.len()));
        while due.len() < self.max_per_tick && self.state == PlaybackState::Playing {
            let event = &self.clip.events[self.index];
            let scheduled = u64::from(self.cycle)
                .saturating_mul(period)
                .saturating_add(event.at_micros);
            if scheduled > played_micros {
                break;
            }
            due.push(event.action.clone());
            self.emitted_users.insert(event.action.user());
            self.events_emitted = self.events_emitted.saturating_add(1);
            self.index += 1;
            if self.index == self.clip.events.len() {
                self.index = 0;
                self.cycle = self.cycle.saturating_add(1);
                if self.total_loops.is_some_and(|loops| self.cycle >= loops) {
                    self.state = PlaybackState::Completed;
                }
            }
        }
        due
    }

    /// Stops playback and returns users that received at least one action, for input release.
    pub fn cancel(&mut self) -> Vec<usize> {
        if self.state == PlaybackState::Playing {
            self.state = PlaybackState::Cancelled;
        }
        self.emitted_users.iter().copied().collect()
    }

    pub fn snapshot(&self) -> PlaybackSnapshot {
        PlaybackSnapshot {
            state: self.state,
            elapsed: self.elapsed,
            remaining_delay: self.delay.saturating_sub(self.elapsed),
            speed: self.speed,
            loops_completed: self.cycle.min(self.total_loops.unwrap_or(u32::MAX)),
            total_loops: self.total_loops,
            events_emitted: self.events_emitted,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ActionCategory {
    Mouse,
    Keyboard,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CategoryFilter {
    pub mouse: bool,
    pub keyboard: bool,
    pub text: bool,
}

impl CategoryFilter {
    pub const ALL: Self = Self {
        mouse: true,
        keyboard: true,
        text: true,
    };

    fn allows(self, category: ActionCategory) -> bool {
        match category {
            ActionCategory::Mouse => self.mouse,
            ActionCategory::Keyboard => self.keyboard,
            ActionCategory::Text => self.text,
        }
    }
}

impl Default for CategoryFilter {
    fn default() -> Self {
        Self::ALL
    }
}

#[derive(Debug, Default, Serialize)]
pub struct ReplicationGraph {
    edges: BTreeMap<usize, BTreeMap<usize, CategoryFilter>>,
    #[serde(skip)]
    seen_events: BTreeSet<u64>,
    #[serde(skip)]
    event_order: VecDeque<u64>,
    #[serde(skip)]
    emitted_targets: BTreeSet<(u64, usize)>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplicationGraphConfig {
    edges: BTreeMap<usize, BTreeMap<usize, CategoryFilter>>,
}

impl<'de> Deserialize<'de> for ReplicationGraph {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let config = ReplicationGraphConfig::deserialize(deserializer)?;
        let graph = Self {
            edges: config.edges,
            ..Self::default()
        };
        graph.validate().map_err(D::Error::custom)?;
        Ok(graph)
    }
}

impl ReplicationGraph {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.edges.values().map(BTreeMap::len).sum::<usize>() <= MAX_REPLICATION_EDGES,
            "replication graph has too many edges"
        );
        for (&source, targets) in &self.edges {
            ensure!(source <= MAX_USER_ID, "user ID is out of range");
            for &target in targets.keys() {
                ensure!(target <= MAX_USER_ID, "user ID is out of range");
                ensure!(
                    source != target,
                    "replication edge cannot target its source"
                );
            }
        }
        self.topological_order()?;
        Ok(())
    }

    pub fn add_edge(
        &mut self,
        source: usize,
        target: usize,
        categories: CategoryFilter,
    ) -> Result<()> {
        ensure!(
            source <= MAX_USER_ID && target <= MAX_USER_ID,
            "user ID is out of range"
        );
        ensure!(
            source != target,
            "replication edge cannot target its source"
        );
        let old = self
            .edges
            .get(&source)
            .and_then(|targets| targets.get(&target))
            .copied();
        ensure!(
            old.is_some()
                || self.edges.values().map(BTreeMap::len).sum::<usize>() < MAX_REPLICATION_EDGES,
            "replication graph has too many edges"
        );
        self.edges
            .entry(source)
            .or_default()
            .insert(target, categories);
        if self.topological_order().is_err() {
            if let Some(previous) = old {
                self.edges
                    .entry(source)
                    .or_default()
                    .insert(target, previous);
            } else {
                self.edges
                    .get_mut(&source)
                    .expect("inserted source")
                    .remove(&target);
                if self.edges.get(&source).is_some_and(BTreeMap::is_empty) {
                    self.edges.remove(&source);
                }
            }
            bail!("replication edge would create a cycle");
        }
        Ok(())
    }

    pub fn remove_edge(&mut self, source: usize, target: usize) -> bool {
        let removed = self
            .edges
            .get_mut(&source)
            .and_then(|targets| targets.remove(&target))
            .is_some();
        if self.edges.get(&source).is_some_and(BTreeMap::is_empty) {
            self.edges.remove(&source);
        }
        removed
    }

    /// Replicates a single authored event to every reachable user once, in stable topological order.
    /// Event IDs should be unique while events may be routed; a bounded 4096-ID window prevents
    /// feedback loops without allowing runtime telemetry to grow without limit.
    pub fn replicate(
        &mut self,
        event_id: u64,
        source: usize,
        action: &Action,
    ) -> Result<Vec<Action>> {
        ensure!(source <= MAX_USER_ID, "user ID is out of range");
        action.validate()?;
        let category = action.category()?;
        self.remember_event(event_id);
        let mut reachable = HashSet::new();
        let mut stack = vec![source];
        while let Some(user) = stack.pop() {
            if let Some(targets) = self.edges.get(&user) {
                for (&target, filter) in targets.iter().rev() {
                    if filter.allows(category) && reachable.insert(target) {
                        stack.push(target);
                    }
                }
            }
        }
        let order = self.topological_order()?;
        let mut result = Vec::new();
        for target in order {
            if reachable.contains(&target) && self.emitted_targets.insert((event_id, target)) {
                result.push(action.for_user(target));
            }
        }
        Ok(result)
    }

    fn remember_event(&mut self, event_id: u64) {
        if self.seen_events.insert(event_id) {
            self.event_order.push_back(event_id);
        }
        if self.event_order.len() > MAX_EVENT_CACHE
            && let Some(expired) = self.event_order.pop_front()
        {
            self.seen_events.remove(&expired);
            self.emitted_targets.retain(|(id, _)| *id != expired);
        }
    }

    fn topological_order(&self) -> Result<Vec<usize>> {
        let mut nodes = BTreeSet::new();
        let mut indegree = BTreeMap::<usize, usize>::new();
        for (&source, targets) in &self.edges {
            nodes.insert(source);
            indegree.entry(source).or_default();
            for &target in targets.keys() {
                nodes.insert(target);
                *indegree.entry(target).or_default() += 1;
            }
        }
        let mut ready = BTreeSet::new();
        for (&node, &degree) in &indegree {
            if degree == 0 {
                ready.insert(node);
            }
        }
        let mut order = Vec::with_capacity(nodes.len());
        while let Some(node) = ready.pop_first() {
            order.push(node);
            if let Some(targets) = self.edges.get(&node) {
                for target in targets.keys() {
                    let degree = indegree.get_mut(target).expect("known graph node");
                    *degree -= 1;
                    if *degree == 0 {
                        ready.insert(*target);
                    }
                }
            }
        }
        ensure!(
            order.len() == nodes.len(),
            "replication graph contains a cycle"
        );
        Ok(order)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulated;

    fn clip() -> Clip {
        let mouse = emulated::device(crate::DeviceKind::Mouse, 1)
            .unwrap()
            .handle as u32;
        Clip::new(vec![
            TimedAction {
                at_micros: 0,
                action: Action::Packet {
                    user: 0,
                    packet: Packet::motion(mouse, 1, 0),
                },
            },
            TimedAction {
                at_micros: 100,
                action: Action::MoveTo {
                    user: 1,
                    x: 20,
                    y: 30,
                },
            },
        ])
        .unwrap()
    }

    #[test]
    fn playback_schedules_and_limits_catch_up() {
        let mut playback = Playback::start(clip(), Some(2), 1.0)
            .unwrap()
            .with_max_actions_per_tick(1)
            .unwrap();
        assert_eq!(playback.tick(Duration::from_micros(0)).len(), 1);
        assert_eq!(playback.tick(Duration::from_micros(100)).len(), 1);
        assert_eq!(playback.tick(Duration::from_micros(0)).len(), 1);
        assert_eq!(playback.tick(Duration::from_micros(100)).len(), 1);
        assert_eq!(playback.snapshot().state, PlaybackState::Completed);
        assert_eq!(playback.snapshot().events_emitted, 4);
    }

    #[test]
    fn playback_delay_is_wall_clock_and_does_not_scale() {
        let delayed_clip = Clip::new(vec![
            TimedAction {
                at_micros: 0,
                action: Action::MoveTo {
                    user: 0,
                    x: 1,
                    y: 0,
                },
            },
            TimedAction {
                at_micros: 100_000,
                action: Action::MoveTo {
                    user: 0,
                    x: 2,
                    y: 0,
                },
            },
        ])
        .unwrap();
        let mut playback =
            Playback::start_with_delay(delayed_clip, Some(1), 100.0, Duration::from_millis(100))
                .unwrap();
        assert!(playback.tick(Duration::from_millis(99)).is_empty());
        assert_eq!(
            playback.snapshot().remaining_delay,
            Duration::from_millis(1)
        );
        assert_eq!(playback.tick(Duration::from_millis(1)).len(), 1);
        assert!(playback.tick(Duration::from_micros(999)).is_empty());
        assert_eq!(playback.tick(Duration::from_micros(1)).len(), 1);
        assert_eq!(playback.snapshot().remaining_delay, Duration::ZERO);
        assert_eq!(playback.snapshot().state, PlaybackState::Completed);
    }

    #[test]
    fn cancellation_reports_only_users_that_received_actions() {
        let mut playback = Playback::start(clip(), None, 1.0).unwrap();
        assert_eq!(playback.tick(Duration::ZERO).len(), 1);
        assert_eq!(playback.cancel(), vec![0]);
        assert_eq!(playback.snapshot().state, PlaybackState::Cancelled);
        assert!(playback.tick(Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn recorder_capacity_preflight_does_not_append_or_change_prior_events() {
        let mut recorder = Recorder::default();
        recorder.start();
        let action = Action::MoveTo {
            user: 0,
            x: 2,
            y: 3,
        };
        recorder
            .check_capture(&action, Duration::from_micros(1))
            .unwrap();
        recorder
            .check_capture(&action, Duration::from_micros(1))
            .unwrap();
        assert_eq!(recorder.event_count(), 0);
        recorder
            .capture(action.clone(), Duration::from_micros(1))
            .unwrap();
        assert!(recorder.check_capture(&action, Duration::ZERO).is_err());
        assert!(
            recorder
                .check_capture(&action, Duration::from_micros(MAX_CLIP_DURATION_MICROS + 1))
                .is_err()
        );
        assert_eq!(recorder.stop().unwrap().events.len(), 1);
    }

    #[test]
    fn recorder_rejects_budget_overflow_without_discarding_prior_events() {
        let mut recorder = Recorder::default();
        recorder.start();
        recorder
            .capture(
                Action::MoveTo {
                    user: 0,
                    x: 2,
                    y: 3,
                },
                Duration::ZERO,
            )
            .unwrap();
        let text = "x".repeat(MAX_TEXT_UTF16_UNITS);
        let mut rejected = false;
        for _ in 0..MAX_CLIP_EVENTS {
            if recorder
                .capture(
                    Action::Text {
                        user: 0,
                        text: text.clone(),
                    },
                    Duration::from_micros(1),
                )
                .is_err()
            {
                rejected = true;
                break;
            }
        }
        assert!(
            rejected,
            "serialized byte budget should reject another event"
        );
        let retained = recorder.event_count();
        assert!(
            recorder
                .capture(
                    Action::Text {
                        user: 0,
                        text: text.clone(),
                    },
                    Duration::from_micros(2),
                )
                .is_err()
        );
        assert_eq!(recorder.event_count(), retained);

        recorder.start();
        assert_eq!(recorder.event_count(), 0);
        recorder
            .capture(Action::Text { user: 0, text }, Duration::from_micros(2))
            .unwrap();
        assert_eq!(recorder.stop().unwrap().events.len(), 1);
    }

    #[test]
    fn clips_reject_bad_timing_packets_and_speed() {
        let mut bad = clip();
        bad.events[0].at_micros = 101;
        assert!(bad.validate().is_err());
        assert!(Playback::start(clip(), None, f64::NAN).is_err());
        assert!(Clip::from_json("{\"version\":1,\"events\":[]}").is_err());
        let json = clip().to_json().unwrap();
        assert_eq!(Clip::from_json(&json).unwrap().events.len(), 2);
        assert!(
            Clip::new(vec![TimedAction {
                at_micros: 0,
                action: Action::Text {
                    user: 0,
                    text: "bad\u{1b}".into()
                },
            }])
            .is_err()
        );
        assert!(
            Clip::new(vec![TimedAction {
                at_micros: 0,
                action: Action::Text {
                    user: 0,
                    text: "valid 😀\ntext".into()
                },
            }])
            .is_ok()
        );
    }

    #[test]
    fn graph_fans_out_once_in_topological_order_and_rejects_cycles() {
        let mut graph = ReplicationGraph::default();
        for (a, b) in [(0, 1), (0, 2), (1, 3), (2, 3)] {
            graph.add_edge(a, b, CategoryFilter::ALL).unwrap();
        }
        let action = Action::MoveTo {
            user: 0,
            x: 9,
            y: 4,
        };
        let replicated = graph.replicate(42, 0, &action).unwrap();
        let users: Vec<_> = replicated.iter().map(Action::user).collect();
        assert_eq!(users, vec![1, 2, 3]);
        assert!(graph.replicate(42, 0, &action).unwrap().is_empty());
        assert!(graph.add_edge(3, 0, CategoryFilter::ALL).is_err());
        let encoded = serde_json::to_string(&graph).unwrap();
        let mut restored: ReplicationGraph = serde_json::from_str(&encoded).unwrap();
        assert_eq!(restored.replicate(43, 0, &action).unwrap().len(), 3);
    }
}
