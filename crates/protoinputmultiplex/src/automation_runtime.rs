//! Session-local orchestration of portable automation; all routing still enters the same Engine.
use crate::{
    replication_queue::Queue,
    routing_engine::{Engine, ProgramOrigin},
};
use anyhow::{Context, Result, ensure};
use mux_core::forwarding::{self, ResolvedEdge as ForwardEdge, Route as ForwardRoute};
use mux_core::replication::{Component, PositionMode, Route};
use mux_core::{
    ResolvedSeat,
    automation::{
        Action, CategoryFilter, Clip, Playback, PlaybackState, Recorder, ReplicationGraph,
    },
    emulated,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, time::Instant};

/// Raw source classification; translated mouse-to-key output is deliberately outside this gate.
pub(crate) fn keyboard_action(action: &Action) -> bool {
    matches!(
        action,
        Action::Text { .. }
            | Action::Packet {
                packet: emulated::Packet { kind: 1, .. },
                ..
            }
    )
}
pub(crate) fn keyboard_disabled(action: &Action, router: &dyn Engine) -> bool {
    keyboard_action(action) && !router.keyboard_enabled(action.user())
}
fn keyboard_release(action: &Action) -> bool {
    matches!(action, Action::Packet { packet: emulated::Packet { kind: 1, flags, .. }, .. } if flags & 1 != 0)
}
pub type Edge = mux_core::replication::ResolvedEdge;
#[derive(Clone, Serialize, Deserialize)]
pub enum Command {
    Mapper(crate::mapper_runtime::Command),
    RecordStart,
    RecordStop,
    Inspect,
    Load {
        clip: Clip,
    },
    Play {
        loops: Option<u32>,
        speed: f64,
    },
    Cancel,
    Replication {
        edges: Vec<Edge>,
    },
    Graph {
        edges: Vec<Edge>,
        expected_revision: u64,
    },
    CancelReplication {
        expected_revision: u64,
    },
    Forwarding {
        edges: Vec<ForwardEdge>,
        expected_revision: u64,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayState {
    pub state: String,
    pub events_emitted: u64,
    pub loops_completed: u32,
    pub total_loops: Option<u32>,
    pub speed: f64,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub mapper: crate::mapper_runtime::State,
    #[serde(default)]
    pub acknowledged: bool,
    #[serde(default)]
    pub macro_revision: u64,
    pub recording: bool,
    pub recorded_events: usize,
    pub loaded_events: usize,
    pub clip: Option<Clip>,
    pub playback: Option<PlayState>,
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub graph_revision: u64,
    #[serde(default)]
    pub replication: crate::replication_queue::Snapshot,
    #[serde(default)]
    pub forwarding_edges: Vec<ForwardEdge>,
    #[serde(default)]
    pub forwarding_revision: u64,
    pub error: Option<String>,
}
pub struct Runtime {
    active: Vec<bool>,
    seats: Vec<ResolvedSeat>,
    recorder: Recorder,
    record_start: Instant,
    clip: Option<Clip>,
    playback: Option<Playback>,
    last_play_tick: Instant,
    graph: ReplicationGraph,
    edges: Vec<Edge>,
    graph_revision: u64,
    event_id: u64,
    identities: Vec<String>,
    queue: Queue,
    completion_pending: bool,
    forwarding_edges: Vec<ForwardEdge>,
    forwarding_revision: u64,
    /// Source-coordinate anchor; the terminal router applies its own gain/bounds.
    forwarding_anchor: Vec<(i32, i32)>,
}
pub struct PreparedVirtualUser {
    identity: String,
    edges: Vec<Edge>,
    graph: ReplicationGraph,
    revision: u64,
}
impl Runtime {
    pub fn new(seats: Vec<ResolvedSeat>, identities: Vec<String>) -> Self {
        let count = seats.len();
        Self {
            active: vec![true; seats.len()],
            seats,
            recorder: Recorder::default(),
            record_start: Instant::now(),
            clip: None,
            playback: None,
            last_play_tick: Instant::now(),
            graph: ReplicationGraph::default(),
            edges: Vec::new(),
            graph_revision: 0,
            event_id: 0,
            identities,
            queue: Queue::default(),
            completion_pending: false,
            forwarding_edges: Vec::new(),
            forwarding_revision: 0,
            forwarding_anchor: vec![(0, 0); count],
        }
    }
    /// Read-only selection preflight: composite work shares one clock, but queued
    /// independent copies reserve only their source, destination and path users.
    pub fn require_selection_idle(&self, user: usize) -> Result<()> {
        ensure!(
            !self.recorder.is_recording()
                && !self.completion_pending
                && !self
                    .playback
                    .as_ref()
                    .is_some_and(|p| p.snapshot().state == PlaybackState::Playing),
            "Stop composite recording or playback before selecting a window"
        );
        ensure!(
            !self.queue.has_user_work(user, Instant::now()),
            "Wait for queued replication before selecting a window"
        );
        Ok(())
    }
    pub fn state(&self, include_clip: bool) -> State {
        State {
            macro_revision: 0,
            acknowledged: false,
            recording: self.recorder.is_recording(),
            recorded_events: self.recorder.event_count(),
            loaded_events: self.clip.as_ref().map_or(0, |c| c.events.len()),
            clip: include_clip.then(|| self.clip.clone()).flatten(),
            playback: self.playback.as_ref().map(|p| {
                let s = p.snapshot();
                PlayState {
                    state: match s.state {
                        PlaybackState::Playing => "playing",
                        PlaybackState::Completed if self.completion_pending => "playing",
                        PlaybackState::Completed => "completed",
                        PlaybackState::Cancelled => "cancelled",
                    }
                    .into(),
                    events_emitted: s.events_emitted,
                    loops_completed: s.loops_completed,
                    total_loops: s.total_loops,
                    speed: s.speed,
                }
            }),
            edges: self.edges.clone(),
            graph_revision: self.graph_revision,
            replication: self.queue.snapshot(),
            forwarding_edges: self.forwarding_edges.clone(),
            forwarding_revision: self.forwarding_revision,
            mapper: Default::default(),
            error: None,
        }
    }
    pub fn prepare_virtual_user(
        &mut self,
        seat: ResolvedSeat,
        identity: String,
        source: usize,
        options: mux_core::replication::EdgeOptions,
    ) -> Result<PreparedVirtualUser> {
        ensure!(
            self.forwarding_edges.is_empty(),
            "Clear input forwarding before starting a replicated virtual user"
        );
        ensure!(
            self.active.get(source) == Some(&true),
            "Unknown or removed clone source"
        );
        let revision = self
            .graph_revision
            .checked_add(1)
            .context("Replication revision exhausted")?;
        let mut seats = self.seats.clone();
        let mut identities = self.identities.clone();
        let mut active = self.active.clone();
        let mut edges = self.edges.clone();
        edges.push(Edge {
            source,
            target: seats.len(),
            mouse: true,
            keyboard: self.seats[source].keyboard.is_some(),
            text: self.seats[source].keyboard.is_some(),
            options,
        });
        seats.push(seat);
        identities.push(identity.clone());
        active.push(true);
        let graph = Self::validated_graph_for(&edges, &seats, &identities, &active)?;
        self.seats.try_reserve(1)?;
        self.identities.try_reserve(1)?;
        self.active.try_reserve(1)?;
        Ok(PreparedVirtualUser {
            identity,
            edges,
            graph,
            revision,
        })
    }
    pub fn add_virtual_user(&mut self, seat: ResolvedSeat, prepared: PreparedVirtualUser) {
        self.seats.push(seat);
        self.identities.push(prepared.identity);
        self.active.push(true);
        self.forwarding_anchor.push((0, 0));
        self.edges = prepared.edges;
        self.graph = prepared.graph;
        self.graph_revision = prepared.revision;
        // Adding a user must not implicitly resume an overflow-suspended graph.
    }
    pub fn remove_virtual_user(&mut self, user: usize, router: &mut dyn Engine) -> Result<()> {
        ensure!(
            self.active.get(user) == Some(&true),
            "Unknown or removed automation user"
        );
        let revision = self
            .graph_revision
            .checked_add(1)
            .context("Replication revision exhausted")?;
        let edges: Vec<_> = self
            .edges
            .iter()
            .filter(|edge| edge.source != user && edge.target != user)
            .cloned()
            .collect();
        let graph = self.validated_graph(&edges)?;
        let forward_edges: Vec<_> = self
            .forwarding_edges
            .iter()
            .filter(|edge| edge.source != user && edge.target != user)
            .copied()
            .collect();
        let forward_revision = self
            .forwarding_revision
            .checked_add(u64::from(
                forward_edges.len() != self.forwarding_edges.len(),
            ))
            .context("Forwarding revision exhausted")?;
        let mut hidden = vec![false; self.seats.len()];
        for edge in &forward_edges {
            hidden[edge.source] = true;
        }
        self.queue.cancel_user(user);
        if self
            .clip
            .as_ref()
            .is_some_and(|clip| clip.events.iter().any(|event| event.action.user() == user))
        {
            if let Some(playback) = &mut self.playback {
                playback.cancel();
            }
            self.completion_pending = false;
            self.queue.cancel_origin(ProgramOrigin::Composite);
            router.release_program(ProgramOrigin::Composite)?;
        }
        router.prepare_remove_virtual_user(user)?;
        if !self.forwarding_edges.is_empty() {
            router.set_forwarding_sources(&hidden)?;
        }
        self.active[user] = false;
        self.edges = edges;
        self.graph = graph;
        self.graph_revision = revision;
        self.forwarding_edges = forward_edges;
        self.forwarding_revision = forward_revision;
        Ok(())
    }
    pub(crate) fn validate(&self, action: &Action) -> Result<()> {
        ensure!(
            self.active.get(action.user()) == Some(&true),
            "Unknown or removed automation user"
        );
        let seat = self
            .seats
            .get(action.user())
            .context("Automation refers to an unknown user")?;
        match action {
            Action::Packet { packet, .. } => {
                packet.validate()?;
                ensure!(packet.kind != 2, "Use a grouped text action");
                if packet.kind == 1 {
                    ensure!(seat.keyboard.is_some(), "User has no paired keyboard");
                }
            }
            Action::Text { text, .. } => {
                emulated::text_packets(
                    seat.keyboard.context("User has no paired keyboard")?,
                    text,
                )?;
            }
            Action::MoveTo { .. } => (),
        }
        Ok(())
    }
    pub fn routes_for_action(&self, action: &Action) -> Result<Vec<Route>> {
        self.validate(action)?;
        if self.queue.snapshot().suspended {
            return Ok(Vec::new());
        }
        let component = match action {
            Action::MoveTo { .. } => Component::Motion,
            Action::Text { .. } => Component::Text,
            Action::Packet { packet, .. } if packet.kind == 1 => Component::Keyboard,
            Action::Packet { packet, .. } if packet.flags == 0 => Component::Motion,
            Action::Packet { packet, .. } if packet.flags == 0x400 => Component::Wheel,
            Action::Packet { packet, .. } if packet.flags == 0x800 => Component::HorizontalWheel,
            Action::Packet { packet, .. } => {
                Component::Button((packet.flags.trailing_zeros() / 2) as u8)
            }
        };
        mux_core::replication::routes(&self.edges, &self.identities, action.user(), component)
    }
    pub fn forwarding_route_for_action(&self, action: &Action) -> Result<Option<ForwardRoute>> {
        self.validate(action)?;
        Ok(forwarding::route(
            &self.forwarding_edges,
            action.user(),
            keyboard_action(action),
        ))
    }
    pub fn forwarding_anchor(&self, user: usize) -> Result<(i32, i32)> {
        self.forwarding_anchor
            .get(user)
            .copied()
            .context("Unknown forwarding source")
    }
    /// Replaces only primary delivery; replication is mutually exclusive with
    /// forwarding and therefore cannot accidentally copy a hidden source.
    fn deliver_primary(&mut self, action: &Action, router: &mut dyn Engine) -> Result<()> {
        let Some(route) = self.forwarding_route_for_action(action)? else {
            return self.deliver(action, router);
        };
        let source = action.user();
        let origin = router.program_origin();
        let source_seat = &self.seats[source];
        let target_seat = &self.seats[route.target];
        match action {
            Action::Packet { packet, .. } => {
                let mut raw = *packet;
                raw.device = if packet.kind == 0 {
                    source_seat.mouse
                } else {
                    source_seat.keyboard.context("Source has no keyboard")?
                };
                let mut output = raw;
                output.device = if packet.kind == 0 {
                    target_seat.mouse
                } else {
                    target_seat
                        .keyboard
                        .context("Forwarding sink needs a keyboard")?
                };
                let accepted = router.input_forwarded(origin, source, &route.path, raw, output)?;
                if accepted && packet.kind == 0 && packet.flags == 0 {
                    let anchor = &mut self.forwarding_anchor[source];
                    anchor.0 = anchor.0.saturating_add(packet.x);
                    anchor.1 = anchor.1.saturating_add(packet.y);
                }
                Ok(())
            }
            Action::MoveTo { x, y, .. } => {
                let (old_x, old_y) = self.forwarding_anchor[source];
                let dx = x.saturating_sub(old_x);
                let dy = y.saturating_sub(old_y);
                let raw = emulated::Packet::motion(source_seat.mouse, dx, dy);
                let output = emulated::Packet::motion(target_seat.mouse, dx, dy);
                if router.input_forwarded(origin, source, &route.path, raw, output)? {
                    self.forwarding_anchor[source] = (*x, *y);
                }
                Ok(())
            }
            Action::Text { text, .. } => {
                let source_keyboard = source_seat.keyboard.context("Source has no keyboard")?;
                let target_keyboard = target_seat.keyboard.context("Sink has no keyboard")?;
                crate::routing_engine::with_text_recipient(router, route.target, |router| {
                    for (raw, output) in emulated::text_packets(source_keyboard, text)?
                        .into_iter()
                        .zip(emulated::text_packets(target_keyboard, text)?)
                    {
                        let _ = router.input_forwarded(origin, source, &route.path, raw, output)?;
                    }
                    Ok(())
                })
            }
        }
    }
    fn deliver(&self, action: &Action, router: &mut dyn Engine) -> Result<()> {
        let seat = &self.seats[action.user()];
        match action {
            Action::MoveTo { user, x, y } => router.move_to(*user, *x, *y),
            Action::Packet { packet, .. } => {
                let mut p = *packet;
                p.device = if p.kind == 0 {
                    seat.mouse
                } else {
                    seat.keyboard.context("No keyboard")?
                };
                router.input(p)
            }
            Action::Text { text, .. } => {
                crate::routing_engine::with_text_recipient(router, action.user(), |router| {
                    for packet in
                        emulated::text_packets(seat.keyboard.context("No keyboard")?, text)?
                    {
                        router.input(packet)?;
                    }
                    Ok(())
                })
            }
        }
    }
    pub fn action(&mut self, action: Action, router: &mut dyn Engine, record: bool) -> Result<()> {
        self.validate(&action)?;
        if keyboard_disabled(&action, router) {
            // Only an existing owner's UP can reach ClaimEngine. No recording, new
            // event ID, playback interruption, delayed copies, or new DOWN ownership.
            if keyboard_release(&action) {
                self.deliver_primary(&action, router)?;
                for route in self.routes_for_action(&action)? {
                    self.deliver_replica(
                        &action.for_user(route.target),
                        router.program_origin(),
                        action.user(),
                        &route.path,
                        router,
                    )?;
                }
            }
            return Ok(());
        }
        if record
            && (self.completion_pending
                || self
                    .playback
                    .as_ref()
                    .is_some_and(|p| p.snapshot().state == PlaybackState::Playing))
        {
            self.cancel(router)?;
        }
        self.event_id = self
            .event_id
            .checked_add(1)
            .context("Automation event counter exhausted")?;
        let component = match &action {
            Action::MoveTo { .. } => Component::Motion,
            Action::Text { .. } => Component::Text,
            Action::Packet { packet, .. } if packet.kind == 1 => Component::Keyboard,
            Action::Packet { packet, .. } if packet.flags == 0 => Component::Motion,
            Action::Packet { packet, .. } if packet.flags == 0x400 => Component::Wheel,
            Action::Packet { packet, .. } if packet.flags == 0x800 => Component::HorizontalWheel,
            Action::Packet { packet, .. } => {
                Component::Button((packet.flags.trailing_zeros() / 2) as u8)
            }
        };
        let source = action.user();
        let origin = router.program_origin();
        let mut routes = if self.queue.snapshot().suspended {
            Vec::new()
        } else {
            mux_core::replication::routes(&self.edges, &self.identities, source, component)?
        };
        if keyboard_action(&action) && !keyboard_release(&action) {
            routes.retain(|route| route.path.iter().all(|&user| router.keyboard_enabled(user)));
        }
        let geometry = component == Component::Motion
            && (matches!(action, Action::MoveTo { .. })
                || routes.iter().any(|route| {
                    route.step_interval_ms != 0
                        || route
                            .hops
                            .iter()
                            .any(|hop| hop.options.position == PositionMode::SourceRelative)
                }));
        let before: Vec<_> = if geometry {
            (0..self.seats.len())
                .map(|user| {
                    if !self.active[user] {
                        return Ok((0, 0));
                    }
                    self.queue
                        .goal(origin, source, user)
                        .map(Ok)
                        .unwrap_or_else(|| router.pointer(user))
                })
                .collect::<Result<_>>()?
        } else {
            Vec::new()
        };
        let queued: Vec<_> = routes
            .iter()
            .filter(|route| {
                route.step_interval_ms != 0
                    || matches!(component, Component::Button(_)) && route.click_delay_ms != 0
            })
            .collect();
        let admitted = queued
            .iter()
            .filter(|route| {
                component != Component::Motion
                    || !self.queue.has_motion(origin, source, route.target)
            })
            .copied()
            .collect::<Vec<_>>();
        let count = admitted.len();
        let bytes_per_copy = match &action {
            Action::Text { text, .. } => 128 + text.len(),
            _ => 128,
        };
        let admission_clock = Instant::now();
        let admitted_bytes = admitted.iter().fold(0usize, |total, route| {
            total.saturating_add(bytes_per_copy).saturating_add(
                route
                    .path
                    .len()
                    .saturating_mul(std::mem::size_of::<usize>()),
            )
        });
        let slot_keys: Vec<_> = queued
            .iter()
            .filter(|route| component != Component::Motion && route.step_interval_ms != 0)
            .map(|route| (origin, source, route.target))
            .collect();
        if !queued.is_empty()
            && let Err(error) = self
                .queue
                .ensure_capacity(count, admitted_bytes)
                .and_then(|()| self.queue.ensure_slots(&slot_keys, admission_clock))
        {
            if error
                .downcast_ref::<crate::replication_queue::Overflow>()
                .is_some()
            {
                self.queue.suspend(error.to_string());
                router.release_replication(None)?;
                let release = matches!(&action, Action::Packet { packet, .. }
                    if packet.kind == 1 && packet.flags & 1 != 0
                        || packet.kind == 0 && packet.flags != 0 && packet.flags <= 512 && packet.flags.trailing_zeros() % 2 == 1);
                if release {
                    self.deliver_primary(&action, router)?;
                }
            }
            return Err(error);
        }
        if record && self.recorder.is_recording() {
            self.recorder
                .capture(action.clone(), self.record_start.elapsed())?;
        }
        self.deliver_primary(&action, router)?;
        let after = if geometry {
            Some(router.pointer(source)?)
        } else {
            None
        };
        let now = Instant::now();
        for route in routes {
            let modern_motion = component == Component::Motion
                && (route.step_interval_ms != 0
                    || route
                        .hops
                        .iter()
                        .any(|hop| hop.options.position == PositionMode::SourceRelative));
            let copy = if modern_motion {
                let position = Self::route_position(&route, &before, after.unwrap());
                Action::MoveTo {
                    user: route.target,
                    x: position.0,
                    y: position.1,
                }
            } else if let Action::MoveTo { x, y, .. } = &action {
                Action::MoveTo {
                    user: route.target,
                    x: before[route.target]
                        .0
                        .saturating_add(x.saturating_sub(before[source].0)),
                    y: before[route.target]
                        .1
                        .saturating_add(y.saturating_sub(before[source].1)),
                }
            } else {
                action.for_user(route.target)
            };
            self.validate(&copy)?;
            let interval = std::time::Duration::from_millis(u64::from(route.step_interval_ms));
            let delay = if matches!(component, Component::Button(_)) {
                route.click_delay_ms
            } else {
                0
            };
            if modern_motion && !interval.is_zero() {
                let Action::MoveTo { x, y, .. } = copy else {
                    unreachable!()
                };
                self.queue.push_motion_via(
                    crate::replication_queue::MotionRequest {
                        user: route.target,
                        goal: (x, y),
                        start: router.pointer(route.target)?,
                        origin,
                        source,
                        due: now + interval,
                        interval,
                    },
                    &route.path,
                )?;
            } else if !interval.is_zero() || delay != 0 {
                let due = self.queue.next_slot(
                    origin,
                    source,
                    route.target,
                    now + std::time::Duration::from_millis(u64::from(delay)),
                    interval,
                    now,
                )?;
                self.queue.barrier(origin, source, route.target, due)?;
                self.queue
                    .push_event_via(copy, origin, source, due, &route.path)?;
            } else {
                self.deliver_replica(&copy, origin, source, &route.path, router)?;
            }
        }
        Ok(())
    }
    fn route_position(
        route: &Route,
        before: &[(i32, i32)],
        mut position: (i32, i32),
    ) -> (i32, i32) {
        for hop in &route.hops {
            position = match hop.options.position {
                PositionMode::Anchored => (
                    before[hop.target]
                        .0
                        .saturating_add(position.0.saturating_sub(before[hop.source].0)),
                    before[hop.target]
                        .1
                        .saturating_add(position.1.saturating_sub(before[hop.source].1)),
                ),
                PositionMode::SourceRelative => (
                    position.0.saturating_add(hop.options.offset_x),
                    position.1.saturating_add(hop.options.offset_y),
                ),
            };
        }
        position
    }
    fn deliver_replica(
        &self,
        action: &Action,
        origin: ProgramOrigin,
        source: usize,
        path: &[usize],
        router: &mut dyn Engine,
    ) -> Result<()> {
        if keyboard_action(action)
            && !keyboard_release(action)
            && (keyboard_disabled(action, router)
                || path.iter().any(|&user| !router.keyboard_enabled(user)))
        {
            return Ok(());
        }
        let seat = &self.seats[action.user()];
        match action {
            Action::MoveTo { user, x, y } => {
                router.move_replica(origin, source, path, *user, *x, *y)
            }
            Action::Packet { packet, .. } => {
                let mut packet = *packet;
                packet.device = if packet.kind == 0 {
                    seat.mouse
                } else {
                    seat.keyboard.context("No keyboard")?
                };
                router.input_replica(origin, source, path, packet)
            }
            Action::Text { text, .. } => {
                crate::routing_engine::with_text_recipient(router, action.user(), |router| {
                    for packet in
                        emulated::text_packets(seat.keyboard.context("No keyboard")?, text)?
                    {
                        router.input_replica(origin, source, path, packet)?;
                    }
                    Ok(())
                })
            }
        }
    }
    pub fn cancel_keyboard_user(&mut self, user: usize) {
        self.queue.cancel_keyboard_user(user);
    }
    pub fn cancel_origin(&mut self, origin: ProgramOrigin) {
        self.queue.cancel_origin(origin);
    }
    pub fn has_pending(&self, origin: ProgramOrigin) -> bool {
        self.queue.has_origin(origin)
    }
    fn cancel_replication(&mut self, router: &mut dyn Engine) -> Result<()> {
        self.queue.cancel_all();
        router.release_replication(None)
    }
    pub fn cancel(&mut self, router: &mut dyn Engine) -> Result<()> {
        self.queue.cancel_all();
        self.completion_pending = false;
        if let Some(playback) = &mut self.playback {
            playback.cancel();
        }
        self.stop_recording();
        router.release_all()
    }

    /// Validates a command without mutating runtime or router state.
    pub fn preflight(&self, command: &Command, router: &dyn Engine) -> Result<()> {
        match command {
            Command::Mapper(_) => {
                anyhow::bail!("Input Mapper commands require the supervised ownership coordinator")
            }
            Command::RecordStart => {
                ensure!(!router.paused(), "Resume routing before recording");
                ensure!(!self.recorder.is_recording(), "Recording is already active");
            }
            Command::RecordStop => {
                ensure!(self.recorder.is_recording(), "Recording is not active");
                ensure!(
                    self.recorder.event_count() > 0,
                    "Recording contains no events"
                );
            }
            Command::Load { clip } => {
                clip.validate()?;
                clip.to_json()?;
                for event in &clip.events {
                    self.validate(&event.action)?;
                }
                ensure!(
                    !self.recorder.is_recording(),
                    "Stop recording before loading a macro"
                );
            }
            Command::Play { loops, speed } => {
                ensure!(
                    !router.paused() && !self.recorder.is_recording(),
                    "Resume routing and stop recording before playback"
                );
                let clip = self.clip.as_ref().context("Load or record a macro first")?;
                clip.validate()?;
                for event in &clip.events {
                    self.validate(&event.action)?;
                }
                Playback::start(clip.clone(), *loops, *speed)?;
            }
            Command::Replication { edges } => {
                ensure!(
                    self.forwarding_edges.is_empty(),
                    "Clear input forwarding before applying replication"
                );
                ensure!(
                    !self.recorder.is_recording()
                        && !self.completion_pending
                        && !self
                            .playback
                            .as_ref()
                            .is_some_and(|p| p.snapshot().state == PlaybackState::Playing),
                    "Stop composite recording or playback before applying the graph"
                );
                self.validated_graph(edges)?;
            }
            Command::Graph {
                edges,
                expected_revision,
            } => {
                ensure!(
                    self.forwarding_edges.is_empty(),
                    "Clear input forwarding before applying replication"
                );
                ensure!(
                    *expected_revision == self.graph_revision,
                    "Replication graph changed; inspect its current revision before applying"
                );
                ensure!(
                    !self.recorder.is_recording()
                        && !self.completion_pending
                        && !self
                            .playback
                            .as_ref()
                            .is_some_and(|p| p.snapshot().state == PlaybackState::Playing),
                    "Stop composite recording or playback before applying the graph"
                );
                self.validated_graph(edges)?;
                self.validate_geometry(edges, router)?;
            }
            Command::CancelReplication { expected_revision } => {
                ensure!(
                    *expected_revision == self.graph_revision,
                    "Replication graph changed; inspect its current revision before cancelling"
                );
            }
            Command::Forwarding {
                edges,
                expected_revision,
            } => {
                ensure!(
                    *expected_revision == self.forwarding_revision,
                    "Input forwarding changed; inspect its revision before applying"
                );
                ensure!(
                    router.forwarding_supported(),
                    "Input forwarding requires automatic virtual desktop routing"
                );
                ensure!(
                    self.edges.is_empty(),
                    "Clear replication before applying input forwarding"
                );
                ensure!(
                    !self.recorder.is_recording()
                        && !self.completion_pending
                        && !self
                            .playback
                            .as_ref()
                            .is_some_and(|p| p.snapshot().state == PlaybackState::Playing),
                    "Stop composite recording or playback before applying forwarding"
                );
                forwarding::validate_resolved(edges, &self.active)?;
                for edge in edges {
                    let source = &self.seats[edge.source];
                    let target = &self.seats[edge.target];
                    ensure!(
                        source.mouse & emulated::MASK == emulated::BASE
                            && target.mouse & emulated::MASK == emulated::BASE
                            && source
                                .keyboard
                                .is_none_or(|key| key & emulated::MASK == emulated::BASE)
                            && target
                                .keyboard
                                .is_none_or(|key| key & emulated::MASK == emulated::BASE),
                        "Forwarding endpoints must be virtual users"
                    );
                    ensure!(
                        !edge.keyboard || target.keyboard.is_some(),
                        "Forwarding keyboard sink needs a virtual keyboard"
                    );
                    router.pointer(edge.source)?;
                }
            }
            Command::Cancel | Command::Inspect => {}
        }
        Ok(())
    }

    fn validated_graph(&self, edges: &[Edge]) -> Result<ReplicationGraph> {
        Self::validated_graph_for(edges, &self.seats, &self.identities, &self.active)
    }
    fn validated_graph_for(
        edges: &[Edge],
        seats: &[ResolvedSeat],
        identities: &[String],
        active: &[bool],
    ) -> Result<ReplicationGraph> {
        ensure!(edges.len() <= 4096, "Replication graph has too many edges");
        let mut graph = ReplicationGraph::default();
        let mut seen = BTreeSet::new();
        for edge in edges {
            ensure!(
                edge.source < seats.len()
                    && edge.target < seats.len()
                    && active.get(edge.source) == Some(&true)
                    && active.get(edge.target) == Some(&true),
                "Replication edge refers to an unknown user"
            );
            ensure!(
                seen.insert((edge.source, edge.target)),
                "Duplicate replication edge"
            );
            ensure!(
                edge.mouse || edge.keyboard || edge.text,
                "Select at least one replicated input category"
            );
            if edge.keyboard || edge.text {
                ensure!(
                    seats[edge.target].keyboard.is_some(),
                    "Replication destination needs a keyboard"
                );
            }
            graph.add_edge(
                edge.source,
                edge.target,
                CategoryFilter {
                    mouse: edge.mouse,
                    keyboard: edge.keyboard,
                    text: edge.text,
                },
            )?;
        }
        graph.validate()?;
        mux_core::replication::validate_paths(edges, identities)?;
        Ok(graph)
    }
    fn validate_geometry(&self, edges: &[Edge], router: &dyn Engine) -> Result<()> {
        for edge in edges {
            if edge.mouse
                && edge.options.motion
                && (edge.options.position == PositionMode::SourceRelative
                    || edge.options.step_interval_ms != 0)
            {
                router.pointer(edge.source)?;
                router.pointer(edge.target)?;
            }
        }
        Ok(())
    }

    pub fn command(&mut self, command: Command, router: &mut dyn Engine) -> Result<State> {
        self.preflight(&command, router)?;
        let mut include_clip = false;
        match command {
            Command::Mapper(_) => {
                anyhow::bail!("Input Mapper commands require the supervised ownership coordinator")
            }
            Command::RecordStart => {
                self.cancel(router)?;
                self.recorder.start();
                self.record_start = Instant::now();
            }
            Command::RecordStop => {
                self.clip = Some(self.recorder.stop()?);
                router.release_all()?;
                include_clip = true;
            }
            Command::Inspect => include_clip = true,
            Command::Load { clip } => {
                self.cancel(router)?;
                self.clip = Some(clip);
                include_clip = true;
            }
            Command::Play { loops, speed } => {
                let playback = Playback::start(
                    self.clip.clone().context("Load or record a macro first")?,
                    loops,
                    speed,
                )?
                .with_max_actions_per_tick(64)?;
                self.cancel(router)?;
                self.playback = Some(playback);
                self.last_play_tick = Instant::now();
            }
            Command::Cancel => self.cancel(router)?,
            Command::CancelReplication { .. } => self.cancel_replication(router)?,
            Command::Forwarding { edges, .. } => {
                let revision = self
                    .forwarding_revision
                    .checked_add(1)
                    .context("Forwarding revision exhausted")?;
                let mut anchors = self.forwarding_anchor.clone();
                for edge in &edges {
                    if !self
                        .forwarding_edges
                        .iter()
                        .any(|previous| previous.source == edge.source)
                    {
                        anchors[edge.source] = router.pointer(edge.source)?;
                    }
                }
                for previous in &self.forwarding_edges {
                    if !edges.iter().any(|edge| edge.source == previous.source) {
                        anchors[previous.source] = (0, 0);
                    }
                }
                let mut hidden = vec![false; self.seats.len()];
                for edge in &edges {
                    hidden[edge.source] = true;
                }
                router.release_forwarding(None)?;
                router.set_forwarding_sources(&hidden)?;
                self.forwarding_edges = edges;
                self.forwarding_anchor = anchors;
                self.forwarding_revision = revision;
            }
            Command::Replication { edges } | Command::Graph { edges, .. } => {
                let graph = self.validated_graph(&edges)?;
                let revision = self
                    .graph_revision
                    .checked_add(1)
                    .context("Replication revision exhausted")?;
                self.cancel(router)?;
                self.graph = graph;
                self.edges = edges;
                self.graph_revision = revision;
                self.queue.resume();
            }
        }
        let mut state = self.state(include_clip);
        state.acknowledged = true;
        Ok(state)
    }
    pub fn tick(&mut self, router: &mut dyn Engine) -> Result<bool> {
        let now = Instant::now();
        for delivery in self.queue.take_due(now, 64) {
            self.deliver_replica(
                &delivery.action,
                delivery.origin,
                delivery.source,
                &delivery.path,
                router,
            )?;
        }
        let delta = now.saturating_duration_since(self.last_play_tick);
        self.last_play_tick = now;
        let Some(playback) = &mut self.playback else {
            return Ok(false);
        };
        if playback.snapshot().state != PlaybackState::Playing {
            if self.completion_pending && !self.queue.has_origin(ProgramOrigin::Composite) {
                router.release_program(ProgramOrigin::Composite)?;
                self.completion_pending = false;
                return Ok(true);
            }
            return Ok(false);
        }
        let due = playback.tick(delta);
        let complete = playback.snapshot().state == PlaybackState::Completed;
        for action in due {
            if let Err(error) = self.action(action, router, false) {
                if error
                    .downcast_ref::<crate::replication_queue::Overflow>()
                    .is_some()
                {
                    if let Some(playback) = &mut self.playback {
                        playback.cancel();
                    }
                    self.completion_pending = false;
                    self.queue.cancel_origin(ProgramOrigin::Composite);
                    router.release_program(ProgramOrigin::Composite)?;
                }
                return Err(error);
            }
        }
        if complete {
            self.completion_pending = true;
            if !self.queue.has_origin(ProgramOrigin::Composite) {
                router.release_program(ProgramOrigin::Composite)?;
                self.completion_pending = false;
            }
        }
        Ok(complete)
    }
    pub fn stop_recording(&mut self) {
        if self.recorder.is_recording() {
            let _ = self.recorder.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mux_core::automation::TimedAction;
    use mux_core::emulated::Packet;

    #[derive(Default)]
    struct TextScopeEngine {
        focus: usize,
        captured: Option<usize>,
        begin_users: Vec<usize>,
        deliveries: Vec<(usize, u32, u32, u32, Vec<usize>)>,
        forwarded: Vec<(usize, Vec<usize>, Packet, Packet)>,
        pointers: Vec<(i32, i32)>,
        reject_motion: bool,
    }

    impl TextScopeEngine {
        fn note_delivery(&mut self, packet: Packet, replica_path: &[usize]) {
            self.deliveries.push((
                self.captured.expect("Text recipient scope is active"),
                packet.device,
                packet.kind,
                packet.data,
                replica_path.to_vec(),
            ));
            if self.deliveries.len() == 1 || self.deliveries.len() == 3 {
                // A lazy attach or another focus change between UTF-16 packets must
                // not redirect the remainder of this logical Text action.
                self.focus = 99;
            }
        }
    }

    impl Engine for TextScopeEngine {
        fn forwarding_supported(&self) -> bool {
            true
        }
        fn input_forwarded(
            &mut self,
            _origin: ProgramOrigin,
            source: usize,
            path: &[usize],
            raw: Packet,
            output: Packet,
        ) -> Result<bool> {
            self.forwarded.push((source, path.to_vec(), raw, output));
            Ok(!(self.reject_motion && raw.kind == 0 && raw.flags == 0))
        }
        fn release_forwarding(&mut self, _origin: Option<ProgramOrigin>) -> Result<()> {
            Ok(())
        }
        fn set_forwarding_sources(&mut self, _sources: &[bool]) -> Result<()> {
            Ok(())
        }
        fn pointer(&self, user: usize) -> Result<(i32, i32)> {
            Ok(self.pointers.get(user).copied().unwrap_or((0, 0)))
        }
        fn begin_text_delivery(&mut self, user: usize) -> Result<()> {
            self.captured = Some(self.focus);
            self.begin_users.push(user);
            Ok(())
        }
        fn end_text_delivery(&mut self, _user: usize) {
            self.captured = None;
        }
        fn input(&mut self, packet: Packet) -> Result<()> {
            self.note_delivery(packet, &[]);
            Ok(())
        }
        fn input_replica(
            &mut self,
            _origin: ProgramOrigin,
            _source: usize,
            path: &[usize],
            packet: Packet,
        ) -> Result<()> {
            self.note_delivery(packet, path);
            Ok(())
        }
        fn key_held(&self, _device: u32, _key: u32) -> bool {
            false
        }
        fn button_held(&self, _device: u32, _button: u32) -> bool {
            false
        }
        fn paused(&self) -> bool {
            false
        }
        fn release_all(&mut self) -> Result<()> {
            Ok(())
        }
        fn pause(&mut self, _value: bool) -> Result<()> {
            Ok(())
        }
        fn settings(
            &mut self,
            _user: usize,
            _settings: mux_core::desktop::CursorSettings,
        ) -> Result<()> {
            Ok(())
        }
        fn tick(&mut self) -> Result<(bool, Vec<crate::router::State>)> {
            Ok((false, Vec::new()))
        }
    }

    fn runtime_with(count: usize) -> Runtime {
        let seats = (0..count)
            .map(|user| ResolvedSeat {
                name: format!("composite-{user}"),
                mouse: emulated::device(mux_core::DeviceKind::Mouse, user as u32 + 1)
                    .unwrap()
                    .handle as u32,
                keyboard: Some(
                    emulated::device(mux_core::DeviceKind::Keyboard, user as u32 + 1)
                        .unwrap()
                        .handle as u32,
                ),
                target_pid: 0,
            })
            .collect();
        Runtime::new(
            seats,
            (0..count).map(|user| format!("user-{user}")).collect(),
        )
    }

    fn runtime() -> Runtime {
        runtime_with(2)
    }

    #[test]
    fn forwarded_source_anchor_survives_reroute_and_keyboard_revision() {
        let mut runtime = runtime_with(3);
        let mut engine = TextScopeEngine {
            pointers: vec![(10, 20), (0, 0), (0, 0)],
            ..TextScopeEngine::default()
        };
        let edge = ForwardEdge {
            source: 0,
            target: 1,
            keyboard: false,
        };
        runtime
            .command(
                Command::Forwarding {
                    edges: vec![edge],
                    expected_revision: 0,
                },
                &mut engine,
            )
            .unwrap();
        runtime
            .action(
                Action::Packet {
                    user: 0,
                    packet: Packet::motion(runtime.seats[0].mouse, 5, 0),
                },
                &mut engine,
                false,
            )
            .unwrap();
        assert_eq!(runtime.forwarding_anchor(0).unwrap(), (15, 20));
        engine.reject_motion = true;
        runtime
            .action(
                Action::Packet {
                    user: 0,
                    packet: Packet::motion(runtime.seats[0].mouse, 7, 0),
                },
                &mut engine,
                false,
            )
            .unwrap();
        assert_eq!(runtime.forwarding_anchor(0).unwrap(), (15, 20));
        engine.reject_motion = false;
        runtime
            .command(
                Command::Forwarding {
                    edges: vec![ForwardEdge {
                        target: 2,
                        keyboard: true,
                        ..edge
                    }],
                    expected_revision: 1,
                },
                &mut engine,
            )
            .unwrap();
        runtime
            .action(
                Action::MoveTo {
                    user: 0,
                    x: 20,
                    y: 20,
                },
                &mut engine,
                false,
            )
            .unwrap();
        assert_eq!(engine.forwarded.last().unwrap().2.x, 5);
        assert_eq!(engine.forwarded.last().unwrap().3.x, 5);
        assert_eq!(engine.forwarded.last().unwrap().1, vec![0, 2]);
        assert_eq!(runtime.forwarding_anchor(0).unwrap(), (20, 20));
    }

    #[test]
    fn selection_idle_preserves_composite_recording_for_every_user() {
        let mut runtime = runtime();
        runtime.recorder.start();
        let before = serde_json::to_value(runtime.state(false)).unwrap();
        assert!(runtime.require_selection_idle(0).is_err());
        assert!(runtime.require_selection_idle(1).is_err());
        assert_eq!(serde_json::to_value(runtime.state(false)).unwrap(), before);
        assert!(runtime.recorder.is_recording());
    }

    #[test]
    fn selection_idle_preserves_composite_playback_and_pending_completion() {
        let mut runtime = runtime();
        let clip = Clip::new(vec![TimedAction {
            at_micros: 100_000_000,
            action: Action::Text {
                user: 0,
                text: "later".into(),
            },
        }])
        .unwrap();
        runtime.playback = Some(Playback::start(clip, Some(1), 1.0).unwrap());
        for completed in [false, true] {
            if completed {
                let playback = runtime.playback.as_mut().unwrap();
                assert_eq!(playback.tick(std::time::Duration::from_secs(101)).len(), 1);
                assert_eq!(playback.snapshot().state, PlaybackState::Completed);
                // Completion retains ownership until release_program succeeds.
                runtime.completion_pending = true;
            }
            let before = serde_json::to_value(runtime.state(false)).unwrap();
            assert!(runtime.require_selection_idle(0).is_err());
            assert!(runtime.require_selection_idle(1).is_err());
            assert_eq!(serde_json::to_value(runtime.state(false)).unwrap(), before);
        }
        runtime.completion_pending = false;
        runtime.require_selection_idle(0).unwrap();
        runtime.require_selection_idle(1).unwrap();
    }

    #[test]
    fn text_delivery_keeps_original_and_replica_packets_in_their_own_scopes() {
        let mut runtime = runtime();
        let mut engine = TextScopeEngine {
            focus: 10,
            ..TextScopeEngine::default()
        };
        runtime
            .action(
                Action::Text {
                    user: 0,
                    text: "AB".into(),
                },
                &mut engine,
                false,
            )
            .unwrap();
        assert_eq!(engine.deliveries.len(), 2);
        assert_eq!(
            engine
                .deliveries
                .iter()
                .map(|delivery| (delivery.0, delivery.1, delivery.2, delivery.3))
                .collect::<Vec<_>>(),
            vec![
                (10, runtime.seats[0].keyboard.unwrap(), 2, u32::from(b'A')),
                (10, runtime.seats[0].keyboard.unwrap(), 2, u32::from(b'B')),
            ]
        );
        assert_eq!(engine.begin_users, [0]);
        assert_eq!(engine.captured, None);

        engine.focus = 20;
        runtime
            .deliver_replica(
                &Action::Text {
                    user: 1,
                    text: "CD".into(),
                },
                ProgramOrigin::Composite,
                0,
                &[0, 1],
                &mut engine,
            )
            .unwrap();
        assert_eq!(engine.deliveries.len(), 4);
        assert_eq!(
            engine.deliveries[2..]
                .iter()
                .map(|delivery| (delivery.0, delivery.1, delivery.2, delivery.3))
                .collect::<Vec<_>>(),
            vec![
                (20, runtime.seats[1].keyboard.unwrap(), 2, u32::from(b'C')),
                (20, runtime.seats[1].keyboard.unwrap(), 2, u32::from(b'D')),
            ]
        );
        assert!(
            engine.deliveries[2..]
                .iter()
                .all(|delivery| delivery.4 == [0, 1])
        );
        assert_eq!(engine.begin_users, [0, 1]);
        assert_eq!(engine.captured, None);
    }
}
