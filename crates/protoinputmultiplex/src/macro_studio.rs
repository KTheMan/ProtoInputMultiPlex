//! Independent authored programs share the routing engine, with input claims per origin.
mod mapper_delivery;
mod user_pause;
use crate::{
    automation_runtime,
    router::State as RoutingState,
    routing_engine::{AgentStudioOwner, Engine},
};
use anyhow::{Context, Result, ensure};
use mux_core::{
    ResolvedSeat,
    automation::{Action, Clip, Playback, PlaybackState, Recorder},
    desktop::CursorSettings,
    emulated::Packet,
    macro_authoring::{self, Edit, Generator},
    routing::{Mode, Window},
};
use mux_windows::power::{KeepAwake, Status as PowerStatus};
use rmcp::schemars;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct PlaybackOptions {
    pub loops: Option<u32>,
    pub speed: f64,
    pub delay_ms: u64,
    pub stop_on_escape: bool,
    pub keep_awake: bool,
}
impl Default for PlaybackOptions {
    fn default() -> Self {
        Self {
            loops: Some(1),
            speed: 1.0,
            delay_ms: 0,
            stop_on_escape: true,
            keep_awake: false,
        }
    }
}
impl PlaybackOptions {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.speed.is_finite() && (0.01..=100.0).contains(&self.speed),
            "playback speed must be between 0.01 and 100"
        );
        ensure!(
            self.loops.is_none_or(|n| (1..=1_000_000).contains(&n)),
            "playback loop count is out of range"
        );
        ensure!(
            self.delay_ms <= 86_400_000,
            "playback delay exceeds 24 hours"
        );
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Inspect {
        user: Option<usize>,
    },
    Record {
        user: usize,
        active: bool,
    },
    Load {
        user: usize,
        clip: Clip,
    },
    Edit {
        user: usize,
        edit: Edit,
    },
    Generate {
        user: usize,
        settings: Generator,
    },
    Reset {
        user: usize,
    },
    Play {
        user: usize,
        options: PlaybackOptions,
    },
    Cancel {
        user: usize,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayState {
    pub state: String,
    pub events_emitted: u64,
    pub loops_completed: u32,
    pub total_loops: Option<u32>,
    pub speed: f64,
    pub remaining_delay_ms: u64,
    pub stop_on_escape: bool,
    pub keep_awake: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserState {
    pub user: usize,
    pub agent_client: Option<u64>,
    pub recording_client: Option<u64>,
    pub recording_error: Option<String>,
    pub recording: bool,
    pub recorded_events: usize,
    pub loaded_events: usize,
    pub revision: u64,
    pub clip: Option<Clip>,
    pub playback: Option<PlayState>,
    pub keep_awake: bool,
    pub power: Option<PowerStatus>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub acknowledged: bool,
    pub users: Vec<UserState>,
    pub error: Option<String>,
}
struct Lane {
    agent_owner: Option<AgentStudioOwner>,
    agent_recording: Option<AgentRecording>,
    recording_error: Option<String>,
    recorder: Recorder,
    record_start: Instant,
    clip: Option<Clip>,
    revision: u64,
    playback: Option<Playback>,
    options: PlaybackOptions,
    last_tick: Instant,
    power: Option<KeepAwake>,
}
impl Default for Lane {
    fn default() -> Self {
        Self {
            agent_owner: None,
            agent_recording: None,
            recording_error: None,
            recorder: Recorder::default(),
            record_start: Instant::now(),
            clip: None,
            revision: 0,
            playback: None,
            options: PlaybackOptions::default(),
            last_tick: Instant::now(),
            power: None,
        }
    }
}
struct AgentRecording {
    client: u64,
    recorder: Recorder,
    started: Instant,
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Origin {
    Manual(usize),
    Agent(u64, usize),
    AgentStudio(AgentStudioOwner),
    Composite,
    Studio(usize),
    Replica(crate::routing_engine::ProgramOrigin, usize),
    Forward(crate::routing_engine::ProgramOrigin, usize),
    /// A sink is an independent logical owner even when several sinks are
    /// driven by the same raw source. Native output tokens remain shared.
    Mapper(crate::routing_engine::ProgramOrigin, u64, usize),
}

/// An Agent Access delivery was denied at the last point before routing input.
#[derive(Debug, Clone)]
pub struct AgentScopeViolation {
    pub client: u64,
    pub message: String,
}
impl fmt::Display for AgentScopeViolation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Agent Access delivery denied: {}", self.message)
    }
}
impl Error for AgentScopeViolation {}

#[derive(Clone, Copy)]
struct AgentGrant {
    mouse: u32,
    keyboard: Option<u32>,
}
#[derive(Clone, Default)]
struct AgentAccess {
    enabled: bool,
    allowed_pids: BTreeSet<u32>,
    users: BTreeMap<usize, AgentGrant>,
    session_control: bool,
    capture: bool,
    generation: u64,
    clients: BTreeSet<u64>,
    active_users: BTreeSet<usize>,
}
impl AgentAccess {
    fn violation(client: u64, message: impl Into<String>) -> anyhow::Error {
        AgentScopeViolation {
            client,
            message: message.into(),
        }
        .into()
    }
    fn check_users(&self, client: u64, source: usize, path: &[usize], target: usize) -> Result<()> {
        if !self.enabled {
            return Err(Self::violation(client, "Agent Access is disabled"));
        }
        for user in std::iter::once(&source)
            .chain(path)
            .chain(std::iter::once(&target))
        {
            ensure!(
                self.users.contains_key(user) && self.active_users.contains(user),
                Self::violation(
                    client,
                    format!("user {user} or its replication path is not permitted")
                )
            );
        }
        Ok(())
    }
    fn check_target(&self, client: u64, window: Option<Window>) -> Result<()> {
        let window = window.ok_or_else(|| {
            Self::violation(client, "there is no eligible target at this input location")
        })?;
        ensure!(
            self.allowed_pids.contains(&window.pid),
            Self::violation(
                client,
                format!(
                    "target process {} is outside Agent Access permissions",
                    window.pid
                )
            )
        );
        Ok(())
    }
    fn check_motion_target(&self, client: u64, window: Option<Window>) -> Result<()> {
        if let Some(window) = window {
            self.check_target(client, Some(window))?;
        }
        // Motion can update only the virtual cursor when the router has no native
        // recipient yet. That lets an agent position it before selecting a window.
        Ok(())
    }
    fn check_pending_attachment(
        &self,
        client: u64,
        router: &dyn Engine,
        user: usize,
    ) -> Result<()> {
        let pending = router.pending_attachment_window(user).map_err(|error| {
            Self::violation(
                client,
                format!("cannot verify pending attachment target: {error:#}"),
            )
        })?;
        if let Some(window) = pending {
            self.check_target(client, Some(window))?;
        }
        Ok(())
    }
    fn check_forwarding_sink(&self, client: u64, router: &dyn Engine, user: usize) -> Result<()> {
        let selected = router.selected_window(user).map_err(|error| {
            Self::violation(
                client,
                format!("cannot verify forwarding sink selection: {error:#}"),
            )
        })?;
        self.check_target(client, selected)?;
        self.check_pending_attachment(client, router, user)
    }
}

fn agent_window_for_packet(
    router: &dyn Engine,
    user: usize,
    packet: Packet,
) -> Result<Option<Window>> {
    router.packet_window(user, packet)
}

#[allow(clippy::too_many_arguments)]
fn check_agent_packet(
    access: &AgentAccess,
    outputs: &crate::output_runtime::Rules,
    router: &dyn Engine,
    client: u64,
    source: usize,
    path: &[usize],
    user: usize,
    packet: Packet,
) -> Result<()> {
    let releasing = if packet.kind == 1 {
        packet.flags & 1 != 0
    } else if packet.kind == 0 {
        matches!(packet.flags, 2 | 8 | 32 | 128 | 512)
    } else {
        false
    };
    if releasing {
        return Ok(());
    }
    access.check_users(client, source, path, user)?;
    let grant = access
        .users
        .get(&user)
        .ok_or_else(|| AgentAccess::violation(client, format!("user {user} is not permitted")))?;
    let expected = if packet.kind == 0 {
        Some(grant.mouse)
    } else {
        grant.keyboard
    };
    ensure!(
        expected == Some(packet.device),
        AgentAccess::violation(
            client,
            "input device is not the permitted virtual device for this user"
        )
    );
    if matches!(packet.kind, 1 | 2) && !router.keyboard_enabled(user) {
        return Ok(());
    }
    let Some(packet) = outputs.translate(packet).map_err(|error| {
        AgentAccess::violation(client, format!("cannot resolve output: {error:#}"))
    })?
    else {
        return Ok(());
    };
    if router.annotation_clears_all(user, packet) {
        ensure!(
            access.session_control,
            AgentAccess::violation(
                client,
                "clearing all annotations requires the session-changes grant"
            )
        );
        ensure!(
            access
                .active_users
                .iter()
                .all(|active_user| access.users.contains_key(active_user)),
            AgentAccess::violation(
                client,
                "clearing all annotations requires permission for every active user"
            )
        );
    }
    let window = agent_window_for_packet(router, user, packet).map_err(|error| {
        AgentAccess::violation(
            client,
            format!("cannot verify current input target: {error:#}"),
        )
    })?;
    access.check_pending_attachment(client, router, user)?;
    if packet.kind == 0 && packet.flags == 0 {
        access.check_motion_target(client, window)
    } else {
        access.check_target(client, window)
    }
}

fn preflight_agent_action(
    access: &AgentAccess,
    outputs: &crate::output_runtime::Rules,
    router: &dyn Engine,
    client: u64,
    action: &Action,
    routes: &[mux_core::replication::Route],
) -> Result<()> {
    let source = action.user();
    if let Action::Packet { packet, .. } = action {
        let release = (packet.kind == 1 && packet.flags & 1 != 0)
            || (packet.kind == 0 && matches!(packet.flags, 2 | 8 | 32 | 128 | 512));
        if release {
            return Ok(());
        }
        let grant = access.users.get(&source).ok_or_else(|| {
            AgentAccess::violation(client, format!("source user {source} is not permitted"))
        })?;
        let expected = if packet.kind == 0 {
            Some(grant.mouse)
        } else {
            grant.keyboard
        };
        ensure!(
            expected == Some(packet.device),
            AgentAccess::violation(
                client,
                "input device is not paired with the permitted source user"
            )
        );
        check_agent_packet(
            access,
            outputs,
            router,
            client,
            source,
            &[],
            source,
            *packet,
        )?;
    } else if let Action::MoveTo { user, x, y } = action {
        access.check_users(client, source, &[], *user)?;
        let window = router.movement_window(*user, *x, *y).map_err(|error| {
            AgentAccess::violation(client, format!("cannot verify movement target: {error:#}"))
        })?;
        access.check_motion_target(client, window)?;
        access.check_pending_attachment(client, router, *user)?;
    } else if let Action::Text { user, text } = action {
        access.check_users(client, source, &[], *user)?;
        let keyboard = access
            .users
            .get(user)
            .and_then(|grant| grant.keyboard)
            .ok_or_else(|| {
                AgentAccess::violation(client, "user has no permitted virtual keyboard")
            })?;
        for packet in mux_core::emulated::text_packets(keyboard, text)? {
            check_agent_packet(access, outputs, router, client, source, &[], *user, packet)?;
        }
    }
    if automation_runtime::keyboard_disabled(action, router) {
        return Ok(());
    }
    for route in routes {
        if automation_runtime::keyboard_action(action)
            && route
                .path
                .iter()
                .any(|&user| !router.keyboard_enabled(user))
        {
            continue;
        }
        access.check_users(client, source, &route.path, route.target)?;
        match action {
            Action::MoveTo { .. } => {
                let point = router.pointer(route.target).map_err(|error| {
                    AgentAccess::violation(
                        client,
                        format!("cannot verify replication cursor: {error:#}"),
                    )
                })?;
                access.check_motion_target(
                    client,
                    router
                        .movement_window(route.target, point.0, point.1)
                        .map_err(|error| {
                            AgentAccess::violation(
                                client,
                                format!("cannot verify replication target: {error:#}"),
                            )
                        })?,
                )?;
                access.check_pending_attachment(client, router, route.target)?;
            }
            Action::Packet { packet, .. } => {
                let grant = access.users.get(&route.target).ok_or_else(|| {
                    AgentAccess::violation(client, "replication target is not permitted")
                })?;
                let mut packet = *packet;
                packet.device = if packet.kind == 0 {
                    grant.mouse
                } else {
                    grant.keyboard.ok_or_else(|| {
                        AgentAccess::violation(
                            client,
                            "replication target has no permitted keyboard",
                        )
                    })?
                };
                check_agent_packet(
                    access,
                    outputs,
                    router,
                    client,
                    source,
                    &route.path,
                    route.target,
                    packet,
                )?;
            }
            Action::Text { text, .. } => {
                let keyboard = access
                    .users
                    .get(&route.target)
                    .and_then(|grant| grant.keyboard)
                    .ok_or_else(|| {
                        AgentAccess::violation(
                            client,
                            "replication target has no permitted keyboard",
                        )
                    })?;
                for packet in mux_core::emulated::text_packets(keyboard, text)? {
                    check_agent_packet(
                        access,
                        outputs,
                        router,
                        client,
                        source,
                        &route.path,
                        route.target,
                        packet,
                    )?;
                }
            }
        }
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn preflight_forwarded_action(
    access: &AgentAccess,
    outputs: &crate::output_runtime::Rules,
    router: &dyn Engine,
    client: u64,
    action: &Action,
    route: &mux_core::forwarding::Route,
    anchor: (i32, i32),
    seats: &[ResolvedSeat],
) -> Result<()> {
    let source = action.user();
    let release = matches!(action, Action::Packet { packet, .. } if Token::parse(*packet).is_some_and(|(_, up)| up));
    if release {
        // Saved UP cleanup cannot acquire a new recipient and remains available
        // after a grant is revoked. The ClaimEngine checks its retained owner.
        return Ok(());
    }
    if automation_runtime::keyboard_disabled(action, router) {
        return Ok(());
    }
    let source_grant = access
        .users
        .get(&source)
        .ok_or_else(|| AgentAccess::violation(client, "forwarding source user is not permitted"))?;
    access.check_users(client, source, &route.path, route.target)?;
    access.check_forwarding_sink(client, router, route.target)?;
    let target = seats
        .get(route.target)
        .context("Forwarding sink is not an active user")?;
    let source_seat = seats
        .get(source)
        .context("Forwarding source is not an active user")?;
    match action {
        Action::Packet { packet, .. } => {
            let expected = if packet.kind == 0 {
                Some(source_grant.mouse)
            } else {
                source_grant.keyboard
            };
            ensure!(
                expected == Some(packet.device),
                AgentAccess::violation(client, "source device is not permitted")
            );
            let mut output = *packet;
            output.device = if packet.kind == 0 {
                target.mouse
            } else {
                target
                    .keyboard
                    .context("Forwarding sink needs a keyboard")?
            };
            check_agent_packet(
                access,
                outputs,
                router,
                client,
                source,
                &route.path,
                route.target,
                output,
            )
        }
        Action::MoveTo { x, y, .. } => {
            ensure!(
                source_grant.mouse == source_seat.mouse,
                AgentAccess::violation(client, "source mouse is not permitted")
            );
            check_agent_packet(
                access,
                outputs,
                router,
                client,
                source,
                &route.path,
                route.target,
                Packet::motion(
                    target.mouse,
                    x.saturating_sub(anchor.0),
                    y.saturating_sub(anchor.1),
                ),
            )
        }
        Action::Text { text, .. } => {
            ensure!(
                source_grant.keyboard == source_seat.keyboard && source_seat.keyboard.is_some(),
                AgentAccess::violation(client, "source keyboard is not permitted")
            );
            let keyboard = target
                .keyboard
                .context("Forwarding sink needs a keyboard")?;
            for packet in mux_core::emulated::text_packets(keyboard, text)? {
                check_agent_packet(
                    access,
                    outputs,
                    router,
                    client,
                    source,
                    &route.path,
                    route.target,
                    packet,
                )?;
            }
            Ok(())
        }
    }
}
impl Origin {
    fn program(self) -> crate::routing_engine::ProgramOrigin {
        use crate::routing_engine::ProgramOrigin;
        match self {
            Self::Manual(user) => ProgramOrigin::Manual(user),
            Self::Agent(client, user) => ProgramOrigin::Agent(client, user),
            Self::AgentStudio(owner) => ProgramOrigin::AgentStudio(owner),
            Self::Composite => ProgramOrigin::Composite,
            Self::Studio(user) => ProgramOrigin::Studio(user),
            Self::Replica(program, _) => program,
            Self::Forward(program, _) => program,
            Self::Mapper(program, _, _) => program,
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Token {
    device: u32,
    kind: u32,
    code: u32,
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Owner {
    origin: Origin,
    source: Token,
    scan: i32,
    key_flags: u32,
}
struct Claim {
    down: Packet,
    owners: BTreeSet<Owner>,
}
struct Pair {
    output: Option<Packet>,
    path: BTreeSet<usize>,
}
#[derive(Default)]
struct Claims(
    BTreeMap<Token, Claim>,
    BTreeMap<Owner, Pair>,
    BTreeSet<Owner>,
    /// Consumed original DOWNs outlive graph retirement until their actual UP.
    /// They have no native output and must suppress repeat DOWNs after Disarm.
    BTreeMap<Owner, BTreeSet<usize>>,
);
impl Token {
    fn parse(packet: Packet) -> Option<(Self, bool)> {
        if packet.kind == 1 {
            Some((
                Self {
                    device: packet.device,
                    kind: 1,
                    code: packet.data,
                },
                packet.flags & 1 != 0,
            ))
        } else if packet.kind == 0
            && packet.flags != 0
            && packet.flags <= 512
            && packet.flags.is_power_of_two()
        {
            let bit = packet.flags.trailing_zeros();
            Some((
                Self {
                    device: packet.device,
                    kind: 0,
                    code: bit / 2,
                },
                bit % 2 == 1,
            ))
        } else {
            None
        }
    }
    fn held(self, router: &dyn Engine) -> bool {
        if self.kind == 1 {
            router.key_held(self.device, self.code)
        } else {
            router.button_held(self.device, self.code)
        }
    }
}
fn released(mut packet: Packet) -> Packet {
    if packet.kind == 1 {
        packet.flags |= 1;
    } else {
        packet.flags *= 2;
    }
    packet
}
impl Claims {
    /// Resolve the raw DOWN/UP identity without translating its output mapping.
    fn raw_owner(origin: Origin, packet: Packet) -> Option<(Owner, bool)> {
        let (source, up) = Token::parse(packet)?;
        Some((
            Owner {
                origin,
                source,
                scan: if packet.kind == 1 { packet.x } else { 0 },
                key_flags: if packet.kind == 1 {
                    packet.flags & !1
                } else {
                    0
                },
            },
            up,
        ))
    }

    /// Decide only original delivery; graph evaluation is admitted separately.
    /// An already-forwarded DOWN keeps that disposition until its UP. Consumed
    /// DOWNs suppress repeats even after the mapper is disarmed or replaced.
    fn consume_mapper_source(
        &mut self,
        origin: Origin,
        raw: Packet,
        consume_new: bool,
        path: &[usize],
    ) -> Result<bool> {
        let Some((owner, up)) = Self::raw_owner(origin, raw) else {
            return Ok(consume_new);
        };
        if let Some(provenance) = self.3.get_mut(&owner) {
            provenance.extend(path.iter().copied());
            if up {
                self.3.remove(&owner);
            }
            return Ok(true);
        }
        if !up && consume_new && !self.1.contains_key(&owner) {
            ensure!(
                self.1.len().saturating_add(self.3.len()) < mux_core::automation::MAX_CLIP_EVENTS,
                "Input source claim capacity reached"
            );
            self.3.insert(owner, path.iter().copied().collect());
            return Ok(true);
        }
        Ok(false)
    }

    fn would_consume_mapper_source(&self, origin: Origin, raw: Packet, consume_new: bool) -> bool {
        let Some((owner, up)) = Self::raw_owner(origin, raw) else {
            return consume_new;
        };
        self.3.contains_key(&owner) || (!up && consume_new && !self.1.contains_key(&owner))
    }

    fn retire_mapper_sources(&mut self, client: u64) {
        self.3.retain(|owner, _| {
            owner
                .origin
                .program()
                .agent()
                .is_none_or(|(id, _)| id != client)
        });
    }

    fn release_mapper(&mut self, generation: u64, router: &mut dyn Engine) -> Result<()> {
        self.release_matching(
            router,
            |owner, _| matches!(owner.origin, Origin::Mapper(_, run, _) if run == generation),
        )
    }

    fn release_mapper_sink(
        &mut self,
        program: crate::routing_engine::ProgramOrigin,
        generation: u64,
        sink: usize,
        router: &mut dyn Engine,
    ) -> Result<()> {
        self.release_matching(router, |owner, _| {
            owner.origin == Origin::Mapper(program, generation, sink)
        })
    }

    fn require_selection_idle(&self, user: usize, seat: &ResolvedSeat) -> Result<()> {
        ensure!(
            !self.1.iter().any(|(owner, pair)| {
                owner.source.device == seat.mouse
                    || Some(owner.source.device) == seat.keyboard
                    || pair.output.is_some_and(|packet| {
                        packet.device == seat.mouse || Some(packet.device) == seat.keyboard
                    })
                    || pair.path.contains(&user)
            }) && !self.3.iter().any(|(owner, path)| {
                owner.source.device == seat.mouse
                    || Some(owner.source.device) == seat.keyboard
                    || path.contains(&user)
            }),
            "Release held input before selecting a window"
        );
        Ok(())
    }
    #[cfg(test)]
    fn input(&mut self, origin: Origin, packet: Packet, router: &mut dyn Engine) -> Result<()> {
        self.input_via(origin, packet, &[], router)
    }
    #[cfg(test)]
    fn input_via(
        &mut self,
        origin: Origin,
        packet: Packet,
        path: &[usize],
        router: &mut dyn Engine,
    ) -> Result<()> {
        self.input_output(origin, packet, Some(packet), path, router)
    }
    fn prune(&mut self, router: &dyn Engine) {
        self.0.retain(|token, _| token.held(router));
        self.1.retain(|_, pair| {
            pair.output.is_none_or(|packet| {
                Token::parse(packet).is_some_and(|(token, _)| self.0.contains_key(&token))
            })
        });
    }
    fn source_button_mask(&self, device: u32) -> u8 {
        self.1
            .keys()
            .chain(self.3.keys())
            .filter(|owner| owner.source.kind == 0 && owner.source.device == device)
            .fold(0, |mask, owner| mask | (1 << owner.source.code))
    }
    /// Keep raw source identity separate from the output token: two buttons and a
    /// remapped key in the same program may all contribute to one native key.
    fn input_output(
        &mut self,
        origin: Origin,
        raw: Packet,
        mut output: Option<Packet>,
        path: &[usize],
        router: &mut dyn Engine,
    ) -> Result<()> {
        self.prune(router);
        let Some((source, up)) = Token::parse(raw) else {
            let source_buttons = self.source_button_mask(raw.device);
            return output.map_or(Ok(()), |packet| {
                router.input_with_source_buttons(packet, source_buttons)
            });
        };
        let owner = Owner {
            origin,
            source,
            scan: if raw.kind == 1 { raw.x } else { 0 },
            key_flags: if raw.kind == 1 { raw.flags & !1 } else { 0 },
        };
        if up {
            return self.release_owner(owner, router);
        }
        if let Some(pair) = self.1.get_mut(&owner) {
            pair.path.extend(path.iter().copied());
            return Ok(());
        }
        // A resource bound, independent of seat/licensing count. Reject before delivery.
        ensure!(
            self.1.len().saturating_add(self.3.len()) < mux_core::automation::MAX_CLIP_EVENTS,
            "Input source claim capacity reached"
        );
        if let Some(packet) = output {
            let (token, _) =
                Token::parse(packet).context("Converted held output must be a button or key")?;
            if let Some(claim) = self.0.get_mut(&token) {
                claim.owners.insert(owner);
            } else {
                if let Err(error) = router.input(packet) {
                    if token.held(router) {
                        // Native delivery may fail after a DOWN was retained.
                        // Preserve this owner's exact output for a later UP.
                        self.0.insert(
                            token,
                            Claim {
                                down: packet,
                                owners: BTreeSet::from([owner]),
                            },
                        );
                        self.1.insert(
                            owner,
                            Pair {
                                output: Some(packet),
                                path: path.iter().copied().collect(),
                            },
                        );
                    }
                    return Err(error);
                }
                if !token.held(router) {
                    if router.button_output_disabled(packet) {
                        // Interceptors such as annotation see the down first. Only an
                        // intentionally dropped legacy output becomes a source-only pair.
                        output = None;
                    } else {
                        return Ok(());
                    }
                } else {
                    self.0.insert(
                        token,
                        Claim {
                            down: packet,
                            owners: BTreeSet::from([owner]),
                        },
                    );
                }
            }
        }
        self.1.insert(
            owner,
            Pair {
                output,
                path: path.iter().copied().collect(),
            },
        );
        Ok(())
    }
    /// A graph change may release a DOWN before its source sends the matching UP.
    /// Ignore that stale UP (and any premature repeated DOWN) even if a new graph
    /// now maps the same source token to another recipient.
    fn consume_forwarding_tombstone(
        &mut self,
        program: crate::routing_engine::ProgramOrigin,
        source_user: usize,
        raw: Packet,
    ) -> bool {
        let Some((source, up)) = Token::parse(raw) else {
            return false;
        };
        let owner = Owner {
            origin: Origin::Forward(program, source_user),
            source,
            scan: if raw.kind == 1 { raw.x } else { 0 },
            key_flags: if raw.kind == 1 { raw.flags & !1 } else { 0 },
        };
        if up {
            self.2.remove(&owner)
        } else {
            self.2.contains(&owner)
        }
    }
    fn release_owner(&mut self, owner: Owner, router: &mut dyn Engine) -> Result<()> {
        let Some(pair) = self.1.get(&owner) else {
            return Ok(());
        };
        let Some((token, _)) = pair.output.and_then(Token::parse) else {
            self.1.remove(&owner);
            return Ok(());
        };
        // Routing may record a delivery failure in its status and return Ok.
        // Retain the last owner until its saved Up actually clears held state.
        if let Some(claim) = self.0.get(&token)
            && claim.owners.len() == 1
            && claim.owners.contains(&owner)
            && token.held(router)
        {
            router.input(released(claim.down))?;
            ensure!(!token.held(router), "Input release remains pending");
        }
        self.1.remove(&owner);
        let Some(claim) = self.0.get_mut(&token) else {
            return Ok(());
        };
        claim.owners.remove(&owner);
        if claim.owners.is_empty() {
            self.0.remove(&token);
        }
        Ok(())
    }
    fn release_matching(
        &mut self,
        router: &mut dyn Engine,
        matches: impl Fn(&Owner, &Pair) -> bool,
    ) -> Result<()> {
        self.prune(router);
        let owners: Vec<_> = self
            .1
            .iter()
            .filter(|(owner, pair)| matches(owner, pair))
            .map(|(owner, _)| *owner)
            .collect();
        let mut first_error = None;
        let mut failures = 0;
        for owner in owners {
            if let Err(error) = self.release_owner(owner, router) {
                failures += 1;
                first_error.get_or_insert(error);
            } else if matches!(owner.origin, Origin::Forward(_, _)) {
                self.2.insert(owner);
            }
        }
        first_error.map_or(Ok(()), |error| {
            Err(error.context(format!(
                "{failures} input owner release(s) failed; first failure"
            )))
        })
    }
    fn release(&mut self, origin: Origin, router: &mut dyn Engine) -> Result<()> {
        self.release_matching(router, |owner, _| owner.origin == origin || matches!(owner.origin, Origin::Replica(program, _) | Origin::Forward(program, _) | Origin::Mapper(program, _, _) if program == origin.program()))
    }
    fn release_replication(
        &mut self,
        program: Option<crate::routing_engine::ProgramOrigin>,
        router: &mut dyn Engine,
    ) -> Result<()> {
        self.release_matching(router, |owner, _| matches!(owner.origin, Origin::Replica(owner_program, _) if program.is_none_or(|program| program == owner_program)))
    }
    fn release_forwarding(
        &mut self,
        program: Option<crate::routing_engine::ProgramOrigin>,
        router: &mut dyn Engine,
    ) -> Result<()> {
        self.release_matching(router, |owner, _| matches!(owner.origin, Origin::Forward(owner_program, _) if program.is_none_or(|program| program == owner_program)))
    }
    fn retire_forwarding_tombstones(&mut self, client: u64) {
        self.2.retain(|owner| {
            !matches!(owner.origin, Origin::Forward(program, _) if program.agent().is_some_and(|(id, _)| id == client))
        });
    }
    fn require_forwarding_source_idle(&self, mouse: u32, keyboard: Option<u32>) -> Result<()> {
        ensure!(
            !self.1.keys().any(|owner| {
                (owner.source.device == mouse || Some(owner.source.device) == keyboard)
                    && !matches!(owner.origin, Origin::Forward(_, _))
            }) && !self.3.keys().any(|owner| {
                owner.source.device == mouse || Some(owner.source.device) == keyboard
            }),
            "Release ordinary source input before applying forwarding"
        );
        Ok(())
    }
    fn release_user_input_claims(
        &mut self,
        seat: &ResolvedSeat,
        router: &mut dyn Engine,
    ) -> Result<()> {
        self.release_matching(router, |owner, _| {
            owner.source.device == seat.mouse || Some(owner.source.device) == seat.keyboard
        })
    }
    fn release_retiring_user(
        &mut self,
        user: usize,
        seat: &ResolvedSeat,
        router: &mut dyn Engine,
    ) -> Result<()> {
        self.release_matching(router, |owner, pair| {
            owner.source.device == seat.mouse || Some(owner.source.device) == seat.keyboard
                || matches!(owner.origin, Origin::Manual(u) | Origin::Agent(_, u) | Origin::Studio(u) if u == user)
                || matches!(owner.origin, Origin::AgentStudio(agent) if agent.user == user)
                || matches!(owner.origin, Origin::Replica(program, source) if source == user || matches!(program, crate::routing_engine::ProgramOrigin::Manual(u) | crate::routing_engine::ProgramOrigin::Agent(_, u) | crate::routing_engine::ProgramOrigin::Studio(u) if u == user))
                || matches!(owner.origin, Origin::Forward(program, source) if source == user || matches!(program, crate::routing_engine::ProgramOrigin::Manual(u) | crate::routing_engine::ProgramOrigin::Agent(_, u) | crate::routing_engine::ProgramOrigin::Studio(u) if u == user))
                || pair.path.contains(&user)
        })
    }
    fn forget_retired_sources(&mut self, seat: &ResolvedSeat) {
        self.2.retain(|owner| {
            owner.source.device != seat.mouse && Some(owner.source.device) != seat.keyboard
        });
        self.3.retain(|owner, _| {
            owner.source.device != seat.mouse && Some(owner.source.device) != seat.keyboard
        });
    }
    fn rules_changed(
        &mut self,
        seat: &ResolvedSeat,
        old: &mux_core::output::Settings,
        new: &mux_core::output::Settings,
        router: &mut dyn Engine,
    ) -> Result<()> {
        self.release_matching(router, |owner, _| {
            (owner.source.device == seat.mouse || Some(owner.source.device) == seat.keyboard)
                && old.source_changed(new, owner.source.kind, owner.source.code)
        })
    }
    fn clear(&mut self) {
        self.0.clear();
        self.1.clear();
        self.2.clear();
        // A general program Cancel/Pause can clear native holds while the raw
        // key is still down. Keep consumed-source disposition until actual UP;
        // worker exit, client retirement and user retirement end that actor.
    }
}
struct ClaimEngine<'a> {
    inner: &'a mut dyn Engine,
    claims: &'a mut Claims,
    origin: Origin,
    agent_access: Arc<AgentAccess>,
    outputs: &'a crate::output_runtime::Rules,
}
impl ClaimEngine<'_> {
    fn check_studio_recipient(
        &self,
        origin: crate::routing_engine::ProgramOrigin,
        user: usize,
        window: Option<Window>,
    ) -> Result<()> {
        if let crate::routing_engine::ProgramOrigin::AgentStudio(owner) = origin {
            let pending = self
                .inner
                .pending_attachment_window(user)
                .map_err(|error| {
                    AgentAccess::violation(
                        owner.client,
                        format!("cannot verify macro attachment target: {error:#}"),
                    )
                })?;
            if let Some(window) = pending {
                self.agent_access.check_target(owner.client, Some(window))?;
            }
            if user == owner.user {
                for recipient in [window, pending].into_iter().flatten() {
                    ensure!(
                        recipient.pid == owner.pid,
                        AgentAccess::violation(
                            owner.client,
                            "macro input would leave its pinned application"
                        )
                    );
                }
            }
        }
        Ok(())
    }
    fn check_studio_packet(
        &self,
        origin: crate::routing_engine::ProgramOrigin,
        packet: Packet,
    ) -> Result<()> {
        if let crate::routing_engine::ProgramOrigin::AgentStudio(owner) = origin {
            let user = self.outputs.user(packet).map_err(|error| {
                AgentAccess::violation(
                    owner.client,
                    format!("cannot resolve macro input: {error:#}"),
                )
            })?;
            if let Some(packet) = self.outputs.translate(packet).map_err(|error| {
                AgentAccess::violation(
                    owner.client,
                    format!("cannot resolve macro output: {error:#}"),
                )
            })? {
                let window = self.inner.packet_window(user, packet).map_err(|error| {
                    AgentAccess::violation(
                        owner.client,
                        format!("cannot verify macro recipient: {error:#}"),
                    )
                })?;
                self.check_studio_recipient(origin, user, window)?;
            }
        }
        Ok(())
    }
    fn check_studio_motion(
        &self,
        origin: crate::routing_engine::ProgramOrigin,
        user: usize,
        x: i32,
        y: i32,
    ) -> Result<()> {
        if let crate::routing_engine::ProgramOrigin::AgentStudio(owner) = origin {
            let window = self.inner.movement_window(user, x, y).map_err(|error| {
                AgentAccess::violation(
                    owner.client,
                    format!("cannot verify macro movement: {error:#}"),
                )
            })?;
            self.check_studio_recipient(origin, user, window)?;
        }
        Ok(())
    }
    fn check_studio_scope(&self, origin: crate::routing_engine::ProgramOrigin) -> Result<()> {
        let crate::routing_engine::ProgramOrigin::AgentStudio(owner) = origin else {
            return Ok(());
        };
        self.agent_access
            .check_users(owner.client, owner.user, &[], owner.user)?;
        ensure!(
            self.agent_access.session_control,
            AgentAccess::violation(
                owner.client,
                "Macro Studio requires the session-changes grant"
            )
        );
        let window = Window {
            hwnd: owner.hwnd,
            pid: owner.pid,
        };
        self.agent_access.check_target(owner.client, Some(window))?;
        let selected = self.inner.selected_window(owner.user).map_err(|error| {
            AgentAccess::violation(
                owner.client,
                format!("cannot verify macro target: {error:#}"),
            )
        })?;
        ensure!(
            selected == Some(window),
            AgentAccess::violation(owner.client, "the macro's selected target changed")
        );
        Ok(())
    }
    fn check_agent_input(
        &self,
        client: u64,
        source: usize,
        path: &[usize],
        packet: Packet,
    ) -> Result<()> {
        let user = self.outputs.user(packet).map_err(|error| {
            AgentAccess::violation(client, format!("cannot resolve input user: {error:#}"))
        })?;
        check_agent_packet(
            &self.agent_access,
            self.outputs,
            self.inner,
            client,
            source,
            path,
            user,
            packet,
        )
    }
    fn check_agent_move(
        &self,
        client: u64,
        source: usize,
        path: &[usize],
        target: usize,
        x: i32,
        y: i32,
    ) -> Result<()> {
        self.agent_access
            .check_users(client, source, path, target)?;
        let window = self.inner.movement_window(target, x, y).map_err(|error| {
            AgentAccess::violation(client, format!("cannot verify movement target: {error:#}"))
        })?;
        self.agent_access.check_motion_target(client, window)?;
        self.agent_access
            .check_pending_attachment(client, self.inner, target)
    }
}
impl Engine for ClaimEngine<'_> {
    fn begin_text_delivery(&mut self, user: usize) -> Result<()> {
        self.inner.begin_text_delivery(user)
    }
    fn end_text_delivery(&mut self, user: usize) {
        self.inner.end_text_delivery(user)
    }
    fn pending_attachment_window(&self, user: usize) -> Result<Option<Window>> {
        self.inner.pending_attachment_window(user)
    }
    fn select_window(&mut self, selection: &crate::window_selection::Selection) -> Result<()> {
        self.claims
            .require_selection_idle(selection.user, self.outputs.resolved(selection.user)?)?;
        self.inner.select_window(selection)
    }
    fn validate_mode_change(&self, multi_keyboard: bool) -> Result<()> {
        self.inner.validate_mode_change(multi_keyboard)
    }

    fn keyboard_enabled(&self, user: usize) -> bool {
        self.inner.keyboard_enabled(user)
    }
    fn poll_motion(&mut self) -> Result<()> {
        self.inner.poll_motion()
    }
    fn motion_deadline(&self) -> Option<Instant> {
        self.inner.motion_deadline()
    }
    fn shutdown(&mut self) -> Result<()> {
        let result = self.inner.shutdown();
        self.claims.clear();
        result
    }
    fn program_origin(&self) -> crate::routing_engine::ProgramOrigin {
        self.origin.program()
    }
    fn forwarding_supported(&self) -> bool {
        self.inner.forwarding_supported()
    }
    fn input_replica(
        &mut self,
        origin: crate::routing_engine::ProgramOrigin,
        source: usize,
        path: &[usize],
        packet: Packet,
    ) -> Result<()> {
        self.check_studio_scope(origin)?;
        self.check_studio_packet(origin, packet)?;
        if let Some((client, agent_source)) = origin.agent() {
            if source != agent_source {
                return Err(AgentAccess::violation(
                    client,
                    "replication source does not match its Agent Access origin",
                ));
            }
            self.check_agent_input(client, agent_source, path, packet)?;
        }
        if matches!(packet.kind, 1 | 2)
            && packet.flags & 1 == 0
            && !self.inner.keyboard_enabled(self.outputs.user(packet)?)
        {
            return Ok(());
        }
        if packet.kind == 0 {
            self.inner.note_mouse_activity(self.outputs.user(packet)?)?;
        }
        let output = if Token::parse(packet).is_some_and(|(_, up)| up) {
            None
        } else {
            self.outputs.translate(packet)?
        };
        self.claims.input_output(
            Origin::Replica(origin, source),
            packet,
            output,
            path,
            self.inner,
        )
    }
    fn input_forwarded(
        &mut self,
        origin: crate::routing_engine::ProgramOrigin,
        source: usize,
        path: &[usize],
        raw: Packet,
        packet: Packet,
    ) -> Result<bool> {
        ensure!(
            self.outputs.user(raw)? == source,
            "forwarding source packet is not paired with its user"
        );
        let owner = Token::parse(raw).map(|(token, _)| Owner {
            origin: Origin::Forward(origin, source),
            source: token,
            scan: if raw.kind == 1 { raw.x } else { 0 },
            key_flags: if raw.kind == 1 { raw.flags & !1 } else { 0 },
        });
        if self
            .claims
            .consume_forwarding_tombstone(origin, source, raw)
        {
            return Ok(false);
        }
        if Token::parse(raw).is_some_and(|(_, up)| up) {
            // Saved recipient cleanup is allowed after grants, settings, or
            // target selection change. It cannot create a new output owner.
            let retained = owner.is_some_and(|owner| self.claims.1.contains_key(&owner));
            self.claims.input_output(
                Origin::Forward(origin, source),
                raw,
                None,
                path,
                self.inner,
            )?;
            return Ok(retained);
        }
        self.check_studio_scope(origin)?;
        self.check_studio_packet(origin, packet)?;
        if let Some((client, agent_source)) = origin.agent() {
            ensure!(
                source == agent_source,
                AgentAccess::violation(client, "forwarding source changed")
            );
            let grant = self.agent_access.users.get(&source).ok_or_else(|| {
                AgentAccess::violation(client, "forwarding source is not permitted")
            })?;
            ensure!(
                Some(raw.device)
                    == if raw.kind == 0 {
                        Some(grant.mouse)
                    } else {
                        grant.keyboard
                    },
                AgentAccess::violation(client, "forwarding source device is not permitted")
            );
            self.agent_access.check_forwarding_sink(
                client,
                self.inner,
                self.outputs.user(packet)?,
            )?;
            self.check_agent_input(client, source, path, packet)?;
        }
        if matches!(packet.kind, 1 | 2)
            && packet.flags & 1 == 0
            && !self.inner.keyboard_enabled(self.outputs.user(packet)?)
        {
            return Ok(false);
        }
        let output = if Token::parse(packet).is_some_and(|(_, up)| up) {
            None
        } else {
            self.outputs.translate(packet)?
        };
        if packet.kind == 0 && packet.flags == 0 {
            let Some(output) = output else {
                return Ok(false);
            };
            let source_buttons = self.claims.source_button_mask(raw.device);
            let accepted = self.inner.forwarded_motion(output, source_buttons)?;
            if accepted {
                self.inner.note_mouse_activity(self.outputs.user(packet)?)?;
            }
            return Ok(accepted);
        }
        if packet.kind == 0 {
            self.inner.note_mouse_activity(self.outputs.user(packet)?)?;
        }
        self.claims.input_output(
            Origin::Forward(origin, source),
            raw,
            output,
            path,
            self.inner,
        )?;
        Ok(owner.is_none_or(|owner| self.claims.1.contains_key(&owner)))
    }
    fn move_replica(
        &mut self,
        origin: crate::routing_engine::ProgramOrigin,
        source: usize,
        path: &[usize],
        user: usize,
        x: i32,
        y: i32,
    ) -> Result<()> {
        self.check_studio_scope(origin)?;
        self.check_studio_motion(origin, user, x, y)?;
        if let Some((client, agent_source)) = origin.agent() {
            if source != agent_source {
                return Err(AgentAccess::violation(
                    client,
                    "movement source does not match its Agent Access origin",
                ));
            }
            self.check_agent_move(client, agent_source, path, user, x, y)?;
        }
        self.inner.move_to(user, x, y)
    }
    fn release_replication(
        &mut self,
        origin: Option<crate::routing_engine::ProgramOrigin>,
    ) -> Result<()> {
        self.claims.release_replication(origin, self.inner)
    }
    fn release_forwarding(
        &mut self,
        origin: Option<crate::routing_engine::ProgramOrigin>,
    ) -> Result<()> {
        self.claims.release_forwarding(origin, self.inner)
    }
    fn prepare_remove_virtual_user(&mut self, user: usize) -> Result<()> {
        self.inner.prepare_remove_virtual_user(user)
    }
    fn set_forwarding_sources(&mut self, sources: &[bool]) -> Result<()> {
        self.inner.set_forwarding_sources(sources)
    }
    fn release_program(&mut self, origin: crate::routing_engine::ProgramOrigin) -> Result<()> {
        use crate::routing_engine::ProgramOrigin;
        let origin = match origin {
            ProgramOrigin::Manual(user) => Origin::Manual(user),
            ProgramOrigin::Agent(client, user) => Origin::Agent(client, user),
            ProgramOrigin::AgentStudio(owner) => Origin::AgentStudio(owner),
            ProgramOrigin::Composite => Origin::Composite,
            ProgramOrigin::Studio(user) => Origin::Studio(user),
        };
        self.claims.release(origin, self.inner)
    }
    fn input(&mut self, packet: Packet) -> Result<()> {
        let source_user = self.outputs.user(packet)?;
        if self
            .claims
            .consume_forwarding_tombstone(self.origin.program(), source_user, packet)
        {
            return Ok(());
        }
        self.check_studio_scope(self.origin.program())?;
        self.check_studio_packet(self.origin.program(), packet)?;
        if let Some((client, source)) = self.origin.program().agent() {
            self.check_agent_input(client, source, &[], packet)?;
        }
        if matches!(packet.kind, 1 | 2)
            && packet.flags & 1 == 0
            && !self.inner.keyboard_enabled(self.outputs.user(packet)?)
        {
            return Ok(());
        }
        if packet.kind == 0 {
            self.inner.note_mouse_activity(self.outputs.user(packet)?)?;
        }
        let output = if Token::parse(packet).is_some_and(|(_, up)| up) {
            None
        } else {
            self.outputs.translate(packet)?
        };
        self.claims
            .input_output(self.origin, packet, output, &[], self.inner)
    }
    fn button_output_disabled(&self, packet: Packet) -> bool {
        self.inner.button_output_disabled(packet)
    }
    fn pointer(&self, user: usize) -> Result<(i32, i32)> {
        self.inner.pointer(user)
    }
    fn selected_window(&self, user: usize) -> Result<Option<Window>> {
        self.inner.selected_window(user)
    }
    fn mouse_window(&self, user: usize) -> Result<Option<Window>> {
        self.inner.mouse_window(user)
    }
    fn keyboard_window(&self, user: usize) -> Result<Option<Window>> {
        self.inner.keyboard_window(user)
    }
    fn movement_window(&self, user: usize, x: i32, y: i32) -> Result<Option<Window>> {
        self.inner.movement_window(user, x, y)
    }
    fn packet_window(&self, user: usize, packet: Packet) -> Result<Option<Window>> {
        self.inner.packet_window(user, packet)
    }
    fn annotation_clears_all(&self, user: usize, packet: Packet) -> bool {
        self.inner.annotation_clears_all(user, packet)
    }
    fn key_held(&self, device: u32, key: u32) -> bool {
        self.inner.key_held(device, key)
    }
    fn button_held(&self, device: u32, button: u32) -> bool {
        self.inner.button_held(device, button)
    }
    fn paused(&self) -> bool {
        self.inner.paused()
    }
    fn release_all(&mut self) -> Result<()> {
        self.inner.release_all()?;
        self.claims.clear();
        Ok(())
    }
    fn pause(&mut self, value: bool) -> Result<()> {
        self.inner.pause(value)
    }
    fn settings(&mut self, user: usize, settings: CursorSettings) -> Result<()> {
        self.inner.settings(user, settings)
    }
    fn tick(&mut self) -> Result<(bool, Vec<RoutingState>)> {
        self.inner.tick()
    }
    fn mode(&mut self, mode: Mode, keyboard: bool) -> Result<()> {
        self.inner.mode(mode, keyboard)
    }
    fn move_to(&mut self, user: usize, x: i32, y: i32) -> Result<()> {
        self.check_studio_scope(self.origin.program())?;
        self.check_studio_motion(self.origin.program(), user, x, y)?;
        if let Some((client, source)) = self.origin.program().agent() {
            self.check_agent_move(client, source, &[], user, x, y)?;
        }
        self.inner.move_to(user, x, y)
    }
}
pub struct PreparedVirtualUser {
    inner: automation_runtime::PreparedVirtualUser,
    identity: String,
}

pub struct Runtime {
    active: Vec<bool>,
    inner: automation_runtime::Runtime,
    macro_revision: u64,
    seats: Vec<ResolvedSeat>,
    lanes: Vec<Lane>,
    claims: Claims,
    next_lane: usize,
    agent_access: Arc<AgentAccess>,
    outputs: crate::output_runtime::Rules,
    mapper: crate::mapper_runtime::Runtime,
    identities: Vec<String>,
    mapper_virtual_users: Vec<bool>,
    mapper_capture_pids: BTreeSet<u32>,
    sources: user_pause::Sources,
}
impl Runtime {
    #[cfg(test)]
    pub fn new(seats: Vec<ResolvedSeat>) -> Self {
        let identities = (0..seats.len())
            .map(|index| format!("dense-{index:08}"))
            .collect();
        Self::new_with_ids(seats, identities)
    }
    pub fn new_with_ids(seats: Vec<ResolvedSeat>, identities: Vec<String>) -> Self {
        let mapper = crate::mapper_runtime::Runtime::new(identities.clone(), seats.clone())
            .expect("validated session user identities");
        let mapper_virtual_users = vec![false; seats.len()];
        Self {
            active: vec![true; seats.len()],
            inner: automation_runtime::Runtime::new(seats.clone(), identities.clone()),
            macro_revision: 0,
            lanes: (0..seats.len()).map(|_| Lane::default()).collect(),
            outputs: crate::output_runtime::Rules::defaults(seats.clone()),
            seats,
            claims: Claims::default(),
            next_lane: 0,
            agent_access: Arc::new(AgentAccess::default()),
            mapper,
            identities,
            mapper_virtual_users,
            mapper_capture_pids: BTreeSet::new(),
            sources: Default::default(),
        }
    }
    pub fn initialize_output(&mut self, rules: crate::output_runtime::Rules) {
        self.outputs = rules;
    }

    fn with_mapper<T>(
        &mut self,
        router: &mut dyn Engine,
        record: bool,
        operation: impl FnOnce(
            &mut crate::mapper_runtime::Runtime,
            &mut dyn crate::mapper_runtime::Backend,
        ) -> Result<T>,
    ) -> Result<T> {
        let independent_busy = (0..self.lanes.len()).any(|user| self.studio_busy(user));
        let mut backend = mapper_delivery::Adapter {
            router,
            claims: &mut self.claims,
            raw_sources: &self.sources,
            legacy: &mut self.inner,
            outputs: &self.outputs,
            access: self.agent_access.clone(),
            seats: &self.seats,
            identities: &self.identities,
            active: &self.active,
            virtual_users: &self.mapper_virtual_users,
            capture_pids: &self.mapper_capture_pids,
            independent_busy,
            record,
            strict_original: self.mapper.is_armed(),
            checked_outputs: Vec::new(),
        };
        operation(&mut self.mapper, &mut backend)
    }

    /// Called once from the validated worker profile, after output rules exist.
    /// Capability flags come from stable interface paths, not Windows handles.
    pub fn initialize_mapper_profile(
        &mut self,
        profile: &mux_core::Profile,
        router: &mut dyn Engine,
    ) -> Result<()> {
        ensure!(
            profile.seats.len() == self.seats.len(),
            "Mapper profile seats changed during startup"
        );
        ensure!(
            profile
                .seats
                .iter()
                .map(mux_core::Seat::identity)
                .eq(self.identities.iter().cloned()),
            "Mapper profile identities changed during startup"
        );
        self.mapper_virtual_users = profile
            .seats
            .iter()
            .map(|seat| -> Result<bool> {
                let mouse = mux_core::emulated::parse(&seat.mouse)?
                    .is_some_and(|device| device.kind == mux_core::DeviceKind::Mouse);
                let keyboard = seat
                    .keyboard
                    .as_deref()
                    .map(mux_core::emulated::parse)
                    .transpose()?
                    .flatten();
                Ok(mouse
                    && (seat.keyboard.is_none()
                        || keyboard
                            .is_some_and(|device| device.kind == mux_core::DeviceKind::Keyboard)))
            })
            .collect::<Result<_>>()?;
        self.mapper_capture_pids = profile
            .desktop
            .as_ref()
            .map(|desktop| desktop.allowed_pids.iter().copied().collect())
            .unwrap_or_default();
        profile.mapper.validate()?;
        if profile.mapper.nodes.is_empty() && profile.mapper.edges.is_empty() {
            return Ok(());
        }
        let reply = self.with_mapper(router, false, |mapper, backend| {
            mapper.command(
                crate::mapper_runtime::Command::Apply {
                    definition: profile.mapper.clone(),
                    expected_revision: 0,
                    gui: None,
                },
                backend,
            )
        })?;
        ensure!(
            reply.error.is_none(),
            "Input Mapper profile could not load: {}",
            reply.error.as_deref().unwrap_or_default()
        );
        Ok(())
    }

    fn mapper_references_user(&self, user: usize) -> bool {
        let Some(identity) = self.identities.get(user) else {
            return false;
        };
        self.mapper.snapshot().definition.nodes.iter().any(|node| {
            node.kind.source().is_some_and(|(source, _)| source.user() == identity)
                || matches!(&node.kind, mux_core::mapper::NodeKind::Sink { user, .. } if user == identity)
                || matches!(&node.kind, mux_core::mapper::NodeKind::PixelColor { capture_user, .. } | mux_core::mapper::NodeKind::ImageMatch { capture_user, .. } | mux_core::mapper::NodeKind::Ocr { capture_user, .. } if capture_user == identity)
        })
    }

    fn retire_mapper_for_user(&mut self, user: usize, router: &mut dyn Engine) -> Result<()> {
        let state = self.mapper.snapshot();
        if (state.armed || state.cleanup_pending) && self.mapper_references_user(user) {
            self.with_mapper(router, false, |mapper, backend| mapper.retire(backend))?;
        }
        Ok(())
    }
    pub fn output_state(&self) -> crate::output_runtime::State {
        self.outputs.state()
    }
    pub fn output_command(
        &mut self,
        command: crate::output_runtime::Command,
        router: &mut dyn Engine,
    ) -> Result<crate::output_runtime::State> {
        let user = match command {
            crate::output_runtime::Command::Inspect => None,
            crate::output_runtime::Command::Configure { user, settings } => {
                self.require_active(user)?;
                self.outputs.validate(user, &settings)?;
                let old = self.outputs.read_settings(user)?.clone();
                self.retire_mapper_for_user(user, router)?;
                // Validate the entire candidate before touching any held source pair.
                self.claims.release_matching(router, |owner, pair| {
                    matches!(owner.origin, Origin::Forward(_, _)) && pair.path.contains(&user)
                })?;
                self.claims
                    .rules_changed(self.outputs.resolved(user)?, &old, &settings, router)?;
                self.outputs.set(user, settings)?;
                Some(user)
            }
        };
        let mut state = self.outputs.state();
        state.acknowledged = true;
        state.user = user;
        Ok(state)
    }
    pub(crate) fn mapper_monitor_snapshot(
        &self,
        actor: crate::mapper_runtime::Actor,
    ) -> crate::mapper_runtime::MonitorSnapshot {
        self.mapper.monitor_snapshot(actor)
    }

    pub fn state(&self, include_clip: bool) -> automation_runtime::State {
        let mut state = self.inner.state(include_clip);
        state.macro_revision = self.macro_revision();
        state.mapper = self.mapper.snapshot();
        state
    }
    pub fn macro_revision(&self) -> u64 {
        self.macro_revision
    }
    fn next_macro_revision(&self) -> Result<u64> {
        self.macro_revision
            .checked_add(1)
            .context("Macro revision exhausted")
    }
    pub fn agent_policy(
        &mut self,
        policy: &crate::agent_runtime::Policy,
        users: &crate::virtual_users::Runtime,
    ) -> Result<()> {
        let slots = users.state().slots;
        let mut candidate = AgentAccess {
            enabled: policy.enabled,
            allowed_pids: policy.allowed_pids.iter().copied().collect(),
            users: BTreeMap::new(),
            session_control: policy.session_control,
            capture: policy.capture,
            generation: self
                .agent_access
                .generation
                .checked_add(1)
                .context("Mapper capture policy generation exhausted")?,
            clients: BTreeSet::new(),
            active_users: slots
                .iter()
                .filter(|slot| slot.active)
                .map(|slot| slot.user)
                .collect(),
        };
        if policy.enabled {
            ensure!(
                !policy.user_ids.is_empty() && !policy.allowed_pids.is_empty(),
                "Agent Access needs at least one permitted user and target process"
            );
            ensure!(
                candidate.allowed_pids.len() == policy.allowed_pids.len()
                    && candidate.allowed_pids.iter().all(|pid| *pid != 0),
                "Agent Access process permissions contain duplicates or invalid PIDs"
            );
            let identities: BTreeSet<_> = policy.user_ids.iter().collect();
            ensure!(
                identities.len() == policy.user_ids.len(),
                "Agent Access user permissions contain duplicates"
            );
            for identity in &policy.user_ids {
                let slot = slots
                    .iter()
                    .find(|slot| slot.active && slot.seat.identity() == identity.as_str())
                    .with_context(|| format!("Unknown or removed permitted user {identity}"))?;
                let resolved = users.resolved(slot.user)?;
                ensure!(
                    resolved.mouse & mux_core::emulated::MASK == mux_core::emulated::BASE
                        && resolved.keyboard.is_none_or(|key| {
                            key & mux_core::emulated::MASK == mux_core::emulated::BASE
                        }),
                    "Agent Access only permits virtual input devices"
                );
                candidate.users.insert(
                    slot.user,
                    AgentGrant {
                        mouse: resolved.mouse,
                        keyboard: resolved.keyboard,
                    },
                );
            }
        }
        self.agent_access = Arc::new(candidate);
        Ok(())
    }
    pub(crate) fn mapper_agent_connected(&mut self, client: u64) {
        Arc::make_mut(&mut self.agent_access).clients.insert(client);
    }
    pub(crate) fn mapper_agent_disconnected(&mut self, client: u64) {
        Arc::make_mut(&mut self.agent_access)
            .clients
            .remove(&client);
        self.mapper
            .cancel_capture_actor(crate::mapper_runtime::Actor::Agent(client));
    }
    pub fn prepare_virtual_user(
        &mut self,
        seat: ResolvedSeat,
        identity: String,
        source: usize,
        options: mux_core::replication::EdgeOptions,
    ) -> Result<PreparedVirtualUser> {
        ensure!(
            !self.sources.paused.contains(&source),
            "Resume the clone source before creating a replicated user"
        );
        self.mapper.prepare_user(&identity)?;
        self.seats.try_reserve(1)?;
        self.lanes.try_reserve(1)?;
        self.active.try_reserve(1)?;
        self.identities.try_reserve(1)?;
        self.mapper_virtual_users.try_reserve(1)?;
        self.outputs.prepare_user()?;
        let inner = self
            .inner
            .prepare_virtual_user(seat, identity.clone(), source, options)?;
        Ok(PreparedVirtualUser { inner, identity })
    }
    pub fn add_virtual_user(
        &mut self,
        seat: ResolvedSeat,
        prepared: PreparedVirtualUser,
        output: mux_core::output::Settings,
    ) {
        self.outputs
            .append_user(seat.clone(), output)
            .expect("prepared virtual output rules");
        self.mapper
            .add_user(prepared.identity.clone(), seat.clone())
            .expect("prepared mapper user");
        self.identities.push(prepared.identity);
        self.mapper_virtual_users.push(true);
        self.inner.add_virtual_user(seat.clone(), prepared.inner);
        self.seats.push(seat);
        self.lanes.push(Lane::default());
        self.active.push(true);
        Arc::make_mut(&mut self.agent_access)
            .active_users
            .insert(self.active.len() - 1);
    }
    pub fn remove_virtual_user(&mut self, user: usize, router: &mut dyn Engine) -> Result<()> {
        self.require_active(user)?;
        self.outputs.read_settings(user)?;
        let mapper_sequence = self.with_mapper(router, false, |mapper, backend| {
            mapper.prepare_remove_user(user, backend)
        })?;
        self.stop_user(user, router)?;
        self.claims
            .release_retiring_user(user, &self.seats[user], router)?;
        let mut engine = ClaimEngine {
            inner: router,
            claims: &mut self.claims,
            origin: Origin::Composite,
            agent_access: self.agent_access.clone(),
            outputs: &self.outputs,
        };
        self.inner.remove_virtual_user(user, &mut engine)?;

        // The remaining changes only retire slots already validated in this command.
        self.mapper.commit_remove_user(user, mapper_sequence);
        self.claims.forget_retired_sources(&self.seats[user]);
        self.lanes[user] = Lane::default();
        self.active[user] = false;
        self.sources.retire_user(user);
        Arc::make_mut(&mut self.agent_access)
            .active_users
            .remove(&user);
        self.outputs.commit_remove_user(user);
        router.commit_remove_virtual_user(user);
        Ok(())
    }
    fn require_active(&self, user: usize) -> Result<()> {
        ensure!(
            self.active.get(user) == Some(&true),
            "Unknown or removed macro user"
        );
        Ok(())
    }
    /// Selection must preserve every raw claim, program and scheduled copy.
    pub fn require_selection_idle(&self, user: usize) -> Result<()> {
        self.require_active(user)?;
        ensure!(
            !self.mapper.is_armed() || !self.mapper_references_user(user),
            "Disarm Input Mapper before selecting a window for a mapped user"
        );
        self.claims
            .require_selection_idle(user, &self.seats[user])?;
        ensure!(
            !self.studio_busy(user),
            "Stop this user's program or recording before selecting a window"
        );
        self.inner.require_selection_idle(user)
    }
    /// Reject annotation-setting changes without partially releasing claims.
    pub fn require_idle_user_input(&self, user: usize) -> Result<()> {
        self.require_active(user)?;
        let seat = &self.seats[user];
        ensure!(
            !self.claims.1.iter().any(|(owner, pair)| {
                owner.source.device == seat.mouse
                    || Some(owner.source.device) == seat.keyboard
                    || pair.output.is_some_and(|packet| {
                        packet.device == seat.mouse || Some(packet.device) == seat.keyboard
                    })
                    || pair.path.contains(&user)
            }),
            "Release held input before changing annotation settings"
        );
        Ok(())
    }
    pub fn studio_state(&self, include_clip: Option<usize>) -> State {
        State {
            acknowledged: false,
            error: None,
            users: self
                .lanes
                .iter()
                .enumerate()
                .map(|(user, lane)| UserState {
                    user,
                    agent_client: lane.agent_owner.map(|owner| owner.client),
                    recording_client: lane
                        .agent_recording
                        .as_ref()
                        .map(|recording| recording.client),
                    recording_error: lane.recording_error.clone(),
                    recording: lane.recorder.is_recording() || lane.agent_recording.is_some(),
                    recorded_events: lane.agent_recording.as_ref().map_or_else(
                        || lane.recorder.event_count(),
                        |recording| recording.recorder.event_count(),
                    ),
                    loaded_events: lane.clip.as_ref().map_or(0, |clip| clip.events.len()),
                    revision: lane.revision,
                    clip: (include_clip == Some(user))
                        .then(|| lane.clip.clone())
                        .flatten(),
                    playback: lane.playback.as_ref().map(|playback| {
                        let state = playback.snapshot();
                        PlayState {
                            state: match state.state {
                                PlaybackState::Playing => "playing",
                                PlaybackState::Completed
                                    if self
                                        .inner
                                        .has_pending(self.studio_origin(user).program()) =>
                                {
                                    "playing"
                                }
                                PlaybackState::Completed => "completed",
                                PlaybackState::Cancelled => "cancelled",
                            }
                            .into(),
                            events_emitted: state.events_emitted,
                            loops_completed: state.loops_completed,
                            total_loops: state.total_loops,
                            speed: state.speed,
                            remaining_delay_ms: state
                                .remaining_delay
                                .as_millis()
                                .min(u128::from(u64::MAX))
                                as u64,
                            stop_on_escape: lane.options.stop_on_escape,
                            keep_awake: lane.power.is_some(),
                        }
                    }),
                    keep_awake: lane.power.is_some(),
                    power: lane.power.as_ref().map(KeepAwake::status),
                })
                .collect(),
        }
    }
    fn validate_clip(&self, user: usize, clip: &Clip) -> Result<()> {
        self.require_active(user)?;
        let seat = self.seats.get(user).context("Unknown macro user")?;
        clip.validate()?;
        for event in &clip.events {
            ensure!(
                event.action.user() == user,
                "an independent macro must belong to its selected user; use Load or Copy to remap it"
            );
            if matches!(
                &event.action,
                Action::Text { .. }
                    | Action::Packet {
                        packet: Packet { kind: 1, .. },
                        ..
                    }
            ) {
                ensure!(seat.keyboard.is_some(), "User has no paired keyboard");
            }
        }
        Ok(())
    }
    fn studio_origin(&self, user: usize) -> Origin {
        self.lanes[user]
            .agent_owner
            .map_or(Origin::Studio(user), Origin::AgentStudio)
    }
    fn studio_busy(&self, user: usize) -> bool {
        let lane = &self.lanes[user];
        lane.recorder.is_recording()
            || lane.agent_recording.is_some()
            || lane
                .playback
                .as_ref()
                .is_some_and(|p| p.snapshot().state == PlaybackState::Playing)
            || self.inner.has_pending(self.studio_origin(user).program())
    }
    fn stop_user_work(&mut self, user: usize) -> Result<()> {
        self.lanes.get(user).context("Unknown macro user")?;
        let origin = self.studio_origin(user);
        self.inner.cancel_origin(origin.program());
        let lane = self.lanes.get_mut(user).context("Unknown macro user")?;
        lane.agent_recording = None;
        if let Some(playback) = &mut lane.playback {
            playback.cancel();
        }
        if lane.recorder.is_recording() {
            let _ = lane.recorder.stop();
        }
        lane.power = None;
        Ok(())
    }
    fn stop_user(&mut self, user: usize, router: &mut dyn Engine) -> Result<()> {
        self.stop_user_work(user)?;
        let origin = self.studio_origin(user);
        self.claims.release(origin, router)?;
        self.lanes[user].agent_owner = None;
        Ok(())
    }
    fn replace(&mut self, user: usize, clip: Option<Clip>, router: &mut dyn Engine) -> Result<()> {
        ensure!(
            !self.lanes[user].recorder.is_recording() && self.lanes[user].agent_recording.is_none(),
            "Stop this user's recording before editing or loading"
        );
        if let Some(clip) = &clip {
            self.validate_clip(user, clip)?;
        }
        self.stop_user(user, router)?;
        self.lanes[user].clip = clip;
        self.lanes[user].revision += 1;
        self.lanes[user].playback = None;
        self.lanes[user].recording_error = None;
        Ok(())
    }
    fn interrupt_composite(&mut self, router: &mut dyn Engine) -> Result<()> {
        let _ = router;
        ensure!(
            !self.inner.state(false).recording
                && self
                    .inner
                    .state(false)
                    .playback
                    .is_none_or(|p| p.state != "playing"),
            "Stop the synchronized composite program before starting independent user programs"
        );
        Ok(())
    }
    pub fn studio_command(&mut self, command: Command, router: &mut dyn Engine) -> Result<State> {
        let next = (!matches!(&command, Command::Inspect { .. }))
            .then(|| self.next_macro_revision())
            .transpose()?;
        let state = self.studio_command_inner(command, router)?;
        if let Some(next) = next {
            self.macro_revision = next;
        }
        Ok(state)
    }
    fn studio_command_inner(&mut self, command: Command, router: &mut dyn Engine) -> Result<State> {
        self.validate_paused_studio(&command, router)?;
        ensure!(
            !self.mapper.is_armed()
                || !matches!(
                    &command,
                    Command::Record { active: true, .. } | Command::Play { .. }
                ),
            "Disarm Input Mapper before recording or playback"
        );
        match &command {
            Command::Inspect { user } => {
                if let Some(user) = user {
                    self.require_active(*user)?;
                }
            }
            Command::Record { user, .. }
            | Command::Load { user, .. }
            | Command::Edit { user, .. }
            | Command::Generate { user, .. }
            | Command::Reset { user }
            | Command::Play { user, .. }
            | Command::Cancel { user } => {
                self.require_active(*user)?;
                ensure!(
                    matches!(&command, Command::Cancel { .. })
                        || self.lanes[*user].agent_recording.is_none(),
                    "A connected agent is recording this lane; stop from that client or discard its recording locally"
                );
            }
        }
        if let Command::Edit {
            edit: Edit::Remap { to, .. },
            ..
        } = &command
        {
            self.require_active(*to)?;
            ensure!(
                self.lanes[*to].agent_recording.is_none(),
                "A connected agent is recording the destination lane"
            );
        }
        let mut include = None;
        let mut copied_source = None;
        match command {
            Command::Inspect { user } => {
                if let Some(user) = user {
                    self.lanes.get(user).context("Unknown macro user")?;
                }
                include = user;
            }
            Command::Record { user, active } => {
                self.lanes.get(user).context("Unknown macro user")?;
                if active {
                    ensure!(!router.paused(), "Resume routing before recording");
                    ensure!(
                        !self.lanes[user].recorder.is_recording(),
                        "User recording is already active"
                    );
                    self.interrupt_composite(router)?;
                    self.stop_user(user, router)?;
                    self.claims.release(Origin::Manual(user), router)?;
                    let lane = &mut self.lanes[user];
                    lane.recorder.start();
                    lane.record_start = Instant::now();
                } else {
                    ensure!(
                        self.lanes[user].recorder.is_recording(),
                        "User is not recording"
                    );
                    let empty = self.lanes[user].recorder.event_count() == 0;
                    let result = self.lanes[user].recorder.stop();
                    self.claims.release(Origin::Manual(user), router)?;
                    if !empty {
                        let clip = result?;
                        self.validate_clip(user, &clip)?;
                        self.lanes[user].clip = Some(clip);
                        self.lanes[user].revision += 1;
                    }
                    include = Some(user);
                }
            }
            Command::Load { user, clip } => {
                self.lanes.get(user).context("Unknown macro user")?;
                let clip = macro_authoring::remap(&clip, user)?;
                self.replace(user, Some(clip), router)?;
                include = Some(user);
            }
            Command::Edit { user, edit } => {
                self.lanes.get(user).context("Unknown macro user")?;
                if let Edit::Remap { from, to } = edit {
                    ensure!(from == user, "Copy source must be the selected user");
                    self.lanes
                        .get(to)
                        .context("Unknown macro destination user")?;
                    let clip = self.lanes[user]
                        .clip
                        .as_ref()
                        .context("Load or record a macro before copying")?;
                    let candidate = macro_authoring::remap(clip, to)?;
                    self.replace(to, Some(candidate), router)?;
                    include = Some(to);
                    copied_source = Some(user);
                } else {
                    let candidate = macro_authoring::apply(self.lanes[user].clip.as_ref(), &edit)?;
                    self.replace(user, candidate, router)?;
                    include = Some(user);
                }
            }
            Command::Generate { user, settings } => {
                self.lanes.get(user).context("Unknown macro user")?;
                ensure!(
                    user == settings.user,
                    "Generator must belong to the selected user"
                );
                let clip = macro_authoring::generate(&settings)?;
                self.replace(user, Some(clip), router)?;
                include = Some(user);
            }
            Command::Reset { user } => {
                self.lanes.get(user).context("Unknown macro user")?;
                self.stop_user(user, router)?;
                self.claims.release(Origin::Manual(user), router)?;
                self.lanes[user].clip = None;
                self.lanes[user].revision += 1;
                self.lanes[user].playback = None;
                self.lanes[user].recording_error = None;
                include = Some(user);
            }
            Command::Play { user, options } => {
                self.lanes.get(user).context("Unknown macro user")?;
                options.validate()?;
                ensure!(
                    !router.paused() && !self.lanes[user].recorder.is_recording(),
                    "Resume routing and stop this user's recording before playback"
                );
                let clip = self.lanes[user]
                    .clip
                    .clone()
                    .context("Load or record a macro first")?;
                self.validate_clip(user, &clip)?;
                if clip
                    .events
                    .iter()
                    .any(|event| matches!(event.action, Action::MoveTo { .. }))
                {
                    router.pointer(user)?;
                }
                let playback = Playback::start_with_delay(
                    clip,
                    options.loops,
                    options.speed,
                    Duration::from_millis(options.delay_ms),
                )?
                .with_max_actions_per_tick(32)?;
                // Acquire before cancelling anything, so a power API failure leaves the prior program intact.
                let power = if options.keep_awake {
                    Some(KeepAwake::new()?)
                } else {
                    None
                };
                self.interrupt_composite(router)?;
                self.stop_user(user, router)?;
                self.claims.release(Origin::Manual(user), router)?;
                let lane = &mut self.lanes[user];
                lane.playback = Some(playback);
                lane.options = options;
                lane.last_tick = Instant::now();
                lane.power = power;
            }
            Command::Cancel { user } => {
                self.stop_user(user, router)?;
                include = Some(user);
            }
        }
        let mut state = self.studio_state(include);
        if let Some(source) = copied_source {
            state.users[source].clip = self.lanes[source].clip.clone();
        }
        state.acknowledged = true;
        Ok(state)
    }
    /// Connected authoring shares the clip, while playback has its own origin
    /// and a pinned application. It cannot replace another actor's live work.
    pub fn agent_studio_command(
        &mut self,
        client: u64,
        command: Command,
        router: &mut dyn Engine,
    ) -> Result<State> {
        let next = (!matches!(&command, Command::Inspect { .. }))
            .then(|| self.next_macro_revision())
            .transpose()?;
        let state = self.agent_studio_command_inner(client, command, router)?;
        if let Some(next) = next {
            self.macro_revision = next;
        }
        Ok(state)
    }
    fn agent_studio_command_inner(
        &mut self,
        client: u64,
        command: Command,
        router: &mut dyn Engine,
    ) -> Result<State> {
        self.validate_paused_studio(&command, router)?;
        ensure!(
            !self.mapper.is_armed()
                || !matches!(
                    &command,
                    Command::Record { active: true, .. } | Command::Play { .. }
                ),
            "Disarm Input Mapper before recording or playback"
        );
        let user = match &command {
            Command::Inspect { user } => {
                if let Some(user) = user {
                    self.require_active(*user)?;
                    self.agent_access.check_users(client, *user, &[], *user)?;
                }
                return Ok(self.studio_state(*user));
            }
            Command::Record { user, .. }
            | Command::Load { user, .. }
            | Command::Edit { user, .. }
            | Command::Generate { user, .. }
            | Command::Reset { user }
            | Command::Play { user, .. }
            | Command::Cancel { user } => *user,
        };
        self.require_active(user)?;
        self.agent_access.check_users(client, user, &[], user)?;
        ensure!(
            self.agent_access.session_control,
            "Macro Studio requires the session-changes grant"
        );
        if let Command::Record { active, .. } = command {
            return self.agent_record(client, user, active, router);
        }
        let destination = if let Command::Edit {
            edit: Edit::Remap { from, to },
            ..
        } = &command
        {
            ensure!(*from == user, "Copy source must be the selected user");
            self.require_active(*to)?;
            self.agent_access.check_users(client, user, &[], *to)?;
            *to
        } else {
            user
        };
        if matches!(command, Command::Cancel { .. }) {
            ensure!(
                self.lanes[user].agent_recording.is_none(),
                "Stop recording from its owning client; playback Cancel does not commit or discard a recording"
            );
            if self.studio_busy(user) || self.lanes[user].agent_owner.is_some() {
                ensure!(
                    self.lanes[user]
                        .agent_owner
                        .is_some_and(|owner| owner.client == client),
                    "This macro lane belongs to another actor; use its owner or local GUI to stop it"
                );
                self.stop_user(user, router)?;
            }
            let mut state = self.studio_state(Some(user));
            state.acknowledged = true;
            return Ok(state);
        }
        ensure!(
            !self.studio_busy(user)
                && !self.studio_busy(destination)
                && self.lanes[user].agent_owner.is_none()
                && self.lanes[destination].agent_owner.is_none(),
            "Stop this user's active Macro Studio program before authoring or starting playback"
        );
        if let Command::Play { options, .. } = command {
            options.validate()?;
            ensure!(!router.paused(), "Resume routing before playback");
            self.require_idle_user_input(user)
                .context("Release held input before starting Macro Studio playback")?;
            let window = router
                .selected_window(user)?
                .context("Select a target before macro playback")?;
            self.agent_access.check_target(client, Some(window))?;
            let clip = self.lanes[user]
                .clip
                .clone()
                .context("Load or record a macro first")?;
            let clip = self.bind_agent_clip(user, clip)?;
            if clip
                .events
                .iter()
                .any(|event| matches!(event.action, Action::MoveTo { .. }))
            {
                router.pointer(user)?;
            }
            let playback = Playback::start_with_delay(
                clip,
                options.loops,
                options.speed,
                Duration::from_millis(options.delay_ms),
            )?
            .with_max_actions_per_tick(32)?;
            self.interrupt_composite(router)?;
            let power = if options.keep_awake {
                Some(KeepAwake::new()?)
            } else {
                None
            };
            self.stop_user(user, router)?;
            let lane = &mut self.lanes[user];
            lane.playback = Some(playback);
            lane.options = options;
            lane.last_tick = Instant::now();
            lane.power = power;
            let owner = AgentStudioOwner {
                client,
                user,
                hwnd: window.hwnd,
                pid: window.pid,
            };
            self.lanes[user].agent_owner = Some(owner);
            let mut state = self.studio_state(None);
            state.acknowledged = true;
            return Ok(state);
        }
        let (candidate, copied) = match command {
            Command::Load { clip, .. } => (Some(macro_authoring::remap(&clip, user)?), false),
            Command::Edit {
                edit: Edit::Remap { .. },
                ..
            } => {
                let clip = self.lanes[user]
                    .clip
                    .as_ref()
                    .context("Load or record a macro before copying")?;
                (Some(macro_authoring::remap(clip, destination)?), true)
            }
            Command::Edit { edit, .. } => (
                macro_authoring::apply(self.lanes[user].clip.as_ref(), &edit)?,
                false,
            ),
            Command::Generate { settings, .. } => {
                ensure!(
                    settings.user == user,
                    "Generator must belong to the selected user"
                );
                (Some(macro_authoring::generate(&settings)?), false)
            }
            Command::Reset { .. } => (None, false),
            _ => unreachable!("inspect/record/play/cancel handled above"),
        };
        let candidate = candidate
            .map(|clip| self.bind_agent_clip(destination, clip))
            .transpose()?;
        // Unlike local Reset, replacing a connected clip does not release manual input.
        self.replace(destination, candidate, router)?;
        let mut state = self.studio_state(Some(destination));
        if copied {
            state.users[user].clip = self.lanes[user].clip.clone();
        }
        state.acknowledged = true;
        Ok(state)
    }
    fn agent_record(
        &mut self,
        client: u64,
        user: usize,
        active: bool,
        router: &mut dyn Engine,
    ) -> Result<State> {
        if active {
            ensure!(!router.paused(), "Resume routing before recording");
            ensure!(
                !self.studio_busy(user) && self.lanes[user].agent_owner.is_none(),
                "Stop this user's active program or recording before starting a recording"
            );
            self.require_idle_user_input(user)
                .context("Release held input before starting recording")?;
            let window = router
                .selected_window(user)?
                .context("Select a target before recording")?;
            self.agent_access.check_target(client, Some(window))?;
            self.interrupt_composite(router)?;
            let mut recorder = Recorder::default();
            recorder.start();
            self.lanes[user].agent_recording = Some(AgentRecording {
                client,
                recorder,
                started: Instant::now(),
            });
            self.lanes[user].recording_error = None;
            self.lanes[user].playback = None;
        } else {
            let recording = self.lanes[user]
                .agent_recording
                .as_ref()
                .context("This user has no connected recording")?;
            ensure!(
                recording.client == client,
                "Only the recording's connected owner can stop and commit it"
            );
            let mut recording = self.lanes[user]
                .agent_recording
                .take()
                .expect("verified recording");
            let result = recording
                .recorder
                .stop()
                .and_then(|clip| self.bind_agent_clip(user, clip));
            match result {
                Ok(clip) => {
                    self.lanes[user].clip = Some(clip);
                    self.lanes[user].revision += 1;
                    self.lanes[user].recording_error = None;
                }
                Err(error) => {
                    self.lanes[user].recording_error = Some(format!(
                        "Recording discarded; previous clip preserved: {error:#}"
                    ));
                    return Err(error);
                }
            }
        }
        let mut state = self.studio_state(Some(user));
        state.acknowledged = true;
        Ok(state)
    }
    fn bind_agent_clip(&self, user: usize, mut clip: Clip) -> Result<Clip> {
        self.validate_clip(user, &clip)?;
        let seat = &self.seats[user];
        for event in &mut clip.events {
            if let Action::Packet { packet, .. } = &mut event.action {
                packet.device = if packet.kind == 0 {
                    seat.mouse
                } else {
                    seat.keyboard.context("User has no paired keyboard")?
                };
            }
        }
        self.validate_clip(user, &clip)?;
        Ok(clip)
    }
    pub fn action(&mut self, action: Action, router: &mut dyn Engine, record: bool) -> Result<()> {
        let user = action.user();
        self.require_active(user)?;
        self.seats.get(user).context("Unknown macro user")?;
        self.inner.validate(&action)?;
        if self.source_gate(Origin::Manual(user), &action, router)? {
            return Ok(());
        }
        let record = record && !automation_runtime::keyboard_disabled(&action, router);
        if record {
            let escape = matches!(&action, Action::Packet { packet: Packet { kind: 1, data: 27, flags, .. }, .. } if flags & 1 == 0);
            let playing = self.inner.has_pending(self.studio_origin(user).program())
                || self.lanes[user]
                    .playback
                    .as_ref()
                    .is_some_and(|p| p.snapshot().state == PlaybackState::Playing);
            // Escape is an explicit opt-in stop; key-up never stops a program. Other manual actions override its owner.
            if playing
                && (!matches!(
                    &action,
                    Action::Packet {
                        packet: Packet {
                            kind: 1,
                            data: 27,
                            ..
                        },
                        ..
                    }
                ) || (escape && self.lanes[user].options.stop_on_escape))
            {
                self.stop_user(user, router)?;
            }
            if self.lanes[user].recorder.is_recording() {
                let elapsed = self.lanes[user].record_start.elapsed();
                self.lanes[user].recorder.capture(action.clone(), elapsed)?;
            }
        }
        self.with_mapper(router, record, |mapper, backend| {
            mapper.action(
                crate::routing_engine::ProgramOrigin::Manual(user),
                action,
                backend,
            )
        })
    }
    pub fn agent_action(
        &mut self,
        client: u64,
        action: Action,
        router: &mut dyn Engine,
    ) -> Result<()> {
        self.agent_action_with_clock(client, action, router, Instant::now)
    }
    fn agent_action_with_clock(
        &mut self,
        client: u64,
        action: Action,
        router: &mut dyn Engine,
        now: impl FnOnce() -> Instant,
    ) -> Result<()> {
        let user = action.user();
        self.require_active(user)?;
        self.preflight_agent_action(client, &action, router)?;
        if self.source_gate(Origin::Agent(client, user), &action, router)? {
            return Ok(());
        }
        let recording_time = if !automation_runtime::keyboard_disabled(&action, router)
            && self.lanes[user]
                .agent_recording
                .as_ref()
                .is_some_and(|recording| recording.client == client)
        {
            ensure!(
                self.agent_access.session_control,
                "Recording requires the session-changes grant"
            );
            let recipient = self
                .inner
                .forwarding_route_for_action(&action)?
                .map_or(user, |route| route.target);
            self.agent_access
                .check_users(client, user, &[], recipient)?;
            self.agent_access
                .check_target(client, router.selected_window(recipient)?)?;
            let recording = self.lanes[user]
                .agent_recording
                .as_ref()
                .expect("verified recording");
            let elapsed = now().duration_since(recording.started);
            if let Err(error) = recording.recorder.check_capture(&action, elapsed) {
                self.lanes[user].agent_recording = None;
                self.lanes[user].recording_error = Some(format!(
                    "Recording discarded before input delivery; previous clip preserved: {error:#}"
                ));
                return Err(
                    error.context("Recording capacity validation failed; input was not delivered")
                );
            }
            Some(elapsed)
        } else {
            None
        };
        let recorded_action = recording_time.map(|_| action.clone());
        self.with_mapper(router, false, |mapper, backend| {
            mapper.action(
                crate::routing_engine::ProgramOrigin::Agent(client, user),
                action,
                backend,
            )
        })?;
        if let Some((elapsed, action)) = recording_time.zip(recorded_action) {
            let recording = self.lanes[user]
                .agent_recording
                .as_mut()
                .expect("recording survives synchronous delivery");
            if let Err(error) = recording.recorder.capture(action, elapsed) {
                self.lanes[user].agent_recording = None;
                self.lanes[user].recording_error = Some(format!(
                    "Recording discarded after delivery; previous clip preserved: {error:#}"
                ));
                return Err(error.context(
                    "Input delivery succeeded but recording failed; do not resend the input",
                ));
            }
        }
        Ok(())
    }
    pub fn preflight_agent_action(
        &self,
        client: u64,
        action: &Action,
        router: &dyn Engine,
    ) -> Result<()> {
        self.require_active(action.user())?;
        self.inner.validate(action)?;
        if self.user_pause_suppresses(Origin::Agent(client, action.user()), action, router) {
            return self.authorize_paused_source(client, action);
        }
        let disposition = self.mapper.source_disposition(action);
        let consumed = if let Action::Packet { packet, user } = action {
            self.claims.would_consume_mapper_source(
                Origin::Agent(client, *user),
                *packet,
                disposition == mux_core::mapper::Disposition::Consume,
            )
        } else {
            disposition == mux_core::mapper::Disposition::Consume
        };
        if consumed {
            let user = action.user();
            self.agent_access.check_users(client, user, &[], user)?;
            if let Action::Packet { packet, .. } = action {
                let grant = self
                    .agent_access
                    .users
                    .get(&user)
                    .context("Mapper source user is not permitted")?;
                let expected = if packet.kind == 0 {
                    Some(grant.mouse)
                } else {
                    grant.keyboard
                };
                ensure!(
                    expected == Some(packet.device),
                    "Mapper input device does not match the permitted source"
                );
            }
            // No original native recipient is needed for consumed input. The
            // mapper separately preauthorizes every reachable output recipient.
            return Ok(());
        }
        if let Some(route) = self.inner.forwarding_route_for_action(action)? {
            return preflight_forwarded_action(
                &self.agent_access,
                &self.outputs,
                router,
                client,
                action,
                &route,
                self.inner.forwarding_anchor(action.user())?,
                &self.seats,
            );
        }
        let routes = self.inner.routes_for_action(action)?;
        preflight_agent_action(
            &self.agent_access,
            &self.outputs,
            router,
            client,
            action,
            &routes,
        )
    }
    /// Saved source-owner obligations, including replicas, still owned by a client.
    /// Several owners can share one native held token without producing extra Ups.
    pub fn agent_pending_owners(&self, client: u64) -> usize {
        self.claims
            .1
            .keys()
            .filter(|owner| {
                owner
                    .origin
                    .program()
                    .agent()
                    .is_some_and(|(id, _)| id == client)
            })
            .count()
    }
    pub fn cancel_agent(&mut self, client: u64, router: &mut dyn Engine) -> Result<()> {
        // Stop every owned producer before attempting any fallible native release.
        // Cleanup uses saved ownership, independent of the current Agent Access grant.
        self.claims.retire_mapper_sources(client);
        self.sources.retire_client(client);
        for user in 0..self.seats.len() {
            if self.lanes[user]
                .agent_recording
                .as_ref()
                .is_some_and(|recording| recording.client == client)
            {
                self.lanes[user].agent_recording = None;
            }
            if self.lanes[user]
                .agent_owner
                .is_some_and(|owner| owner.client == client)
            {
                self.stop_user_work(user)?;
            }
            let origin = crate::routing_engine::ProgramOrigin::Agent(client, user);
            self.inner.cancel_origin(origin);
        }
        let mapper_cleanup = self.with_mapper(router, false, |mapper, backend| {
            mapper.retire_actor(crate::mapper_runtime::Actor::Agent(client), backend)
        });
        // One exhaustive sweep covers ordinary, Studio, and replicated claims.
        // Failed last owners remain saved; unrelated owners of a shared token survive.
        let result = self.claims.release_matching(router, |owner, _| {
            owner
                .origin
                .program()
                .agent()
                .is_some_and(|(id, _)| id == client)
        });
        let result = mapper_cleanup.and(result);
        for lane in &mut self.lanes {
            if let Some(owner) = lane.agent_owner
                && owner.client == client
                && !self.claims.1.keys().any(|claim| {
                    claim.origin.program()
                        == crate::routing_engine::ProgramOrigin::AgentStudio(owner)
                })
            {
                lane.agent_owner = None;
            }
        }
        result
    }
    /// EOF cleanup can discard stale-UP markers: the retired client cannot send
    /// another packet. ReleaseAll keeps them because its client stays connected.
    pub fn retire_agent_forwarding_tombstones(&mut self, client: u64) {
        self.claims.retire_forwarding_tombstones(client);
    }
    pub fn packet(&mut self, packet: Packet, router: &mut dyn Engine, record: bool) -> Result<()> {
        packet.validate()?;
        let user = self
            .seats
            .iter()
            .position(|s| {
                if packet.kind == 0 {
                    s.mouse == packet.device
                } else {
                    s.keyboard == Some(packet.device)
                }
            })
            .context("Unassigned automation device")?;
        self.action(Action::Packet { user, packet }, router, record)
    }
    pub fn tick(&mut self, router: &mut dyn Engine) -> Result<bool> {
        self.retry_user_pause_claims(router);
        let mapper_sequence = self.mapper.sequence();
        // The mapper retains and reports failed release obligations itself.
        // A disconnected actor's retry must not trigger Session's global cancel.
        let _ = self.with_mapper(router, false, |mapper, backend| {
            let _ = mapper.poll_cleanup(backend);
            mapper.poll_timers(Instant::now(), backend)
        });
        let mapper_changed = self.mapper.sequence() != mapper_sequence;
        let mut engine = ClaimEngine {
            inner: router,
            claims: &mut self.claims,
            origin: Origin::Composite,
            agent_access: self.agent_access.clone(),
            outputs: &self.outputs,
        };
        let mut complete = self.inner.tick(&mut engine)?;
        let now = Instant::now();
        let count = self.lanes.len();
        let mut remaining: usize = 64;
        for offset in 0..count {
            let user = (self.next_lane + offset) % count;
            let origin = self.studio_origin(user);
            let lane = &mut self.lanes[user];
            let Some(playback) = &mut lane.playback else {
                continue;
            };
            if playback.snapshot().state == PlaybackState::Completed {
                if !self.inner.has_pending(origin.program()) {
                    self.claims.release(origin, router)?;
                    lane.power = None;
                    lane.agent_owner = None;
                }
                continue;
            }
            if playback.snapshot().state != PlaybackState::Playing || remaining == 0 {
                continue;
            }
            let delta = now.saturating_duration_since(lane.last_tick);
            lane.last_tick = now;
            playback.set_max_actions_per_tick(remaining.min(32))?;
            let due = playback.tick(delta);
            remaining = remaining.saturating_sub(due.len());
            let finished = playback.snapshot().state == PlaybackState::Completed;
            for action in due {
                if let Some((client, _)) = origin.program().agent()
                    && let Err(error) = self.preflight_agent_action(client, &action, router)
                {
                    self.stop_user(user, router)?;
                    return Err(error);
                }
                let mut engine = ClaimEngine {
                    inner: router,
                    claims: &mut self.claims,
                    origin,
                    agent_access: self.agent_access.clone(),
                    outputs: &self.outputs,
                };
                if let Err(error) = self.inner.action(action, &mut engine, false) {
                    self.stop_user(user, router)?;
                    return Err(error);
                }
            }
            if finished && !self.inner.has_pending(origin.program()) {
                self.claims.release(origin, router)?;
                self.lanes[user].power = None;
                self.lanes[user].agent_owner = None;
                complete = true;
            }
        }
        if count > 0 {
            self.next_lane = (self.next_lane + 1) % count;
        }
        Ok(complete || mapper_changed)
    }
    pub fn cancel(&mut self, router: &mut dyn Engine) -> Result<()> {
        let mut first_error = self
            .with_mapper(router, false, |mapper, backend| mapper.retire(backend))
            .err();
        for user in 0..self.lanes.len() {
            if let Err(error) = self.stop_user(user, router) {
                first_error.get_or_insert(error);
            }
        }
        let mut engine = ClaimEngine {
            inner: router,
            claims: &mut self.claims,
            origin: Origin::Composite,
            agent_access: self.agent_access.clone(),
            outputs: &self.outputs,
        };
        // A failed lane release must not starve other lanes or the global
        // manual/agent claims. Each layer retains its failed owners for retry.
        let global = self.inner.cancel(&mut engine);
        first_error.map_or(global, Err)
    }
    pub fn stop_recording(&mut self) {
        self.inner.stop_recording();
        for lane in &mut self.lanes {
            lane.agent_recording = None;
            if lane.recorder.is_recording() {
                let _ = lane.recorder.stop();
            }
        }
    }
    pub fn interrupt_user(&mut self, user: usize, router: &mut dyn Engine) -> Result<bool> {
        self.lanes.get(user).context("Unknown macro user")?;
        ensure!(
            self.lanes[user].agent_recording.is_none(),
            "Stop or discard the connected recording before changing this user's settings"
        );
        self.retire_mapper_for_user(user, router)?;
        let global = self.inner.state(false).recording
            || self
                .inner
                .state(false)
                .playback
                .is_some_and(|p| p.state == "playing");
        if global {
            self.cancel(router)?;
        } else {
            self.stop_user(user, router)?;
        }
        Ok(global)
    }
    /// Retire keyboard paths at a settings boundary, including copies already held
    /// on other users. Failure retains the owner and prevents configuration commit.
    pub fn keyboard_settings_change(&mut self, user: usize, router: &mut dyn Engine) -> Result<()> {
        self.require_active(user)?;
        self.retire_mapper_for_user(user, router)?;
        self.inner.cancel_keyboard_user(user);
        let keyboard = self.seats[user].keyboard;
        self.claims.release_matching(router, |owner, pair| {
            owner.source.kind == 1
                && (Some(owner.source.device) == keyboard || pair.path.contains(&user))
        })
    }
    /// Retires claims made from this user's devices before committing new settings.
    /// Replicas held on another user's destination devices remain owned by their source path.
    pub fn release_user_input_claims(
        &mut self,
        user: usize,
        router: &mut dyn Engine,
    ) -> Result<()> {
        self.require_active(user)?;
        self.retire_mapper_for_user(user, router)?;
        self.claims.release_matching(router, |owner, pair| {
            matches!(owner.origin, Origin::Forward(_, _)) && pair.path.contains(&user)
        })?;
        self.claims
            .release_user_input_claims(&self.seats[user], router)
    }
    pub fn command(
        &mut self,
        command: automation_runtime::Command,
        router: &mut dyn Engine,
    ) -> Result<automation_runtime::State> {
        if let automation_runtime::Command::Mapper(command) = command {
            let reply = self.with_mapper(router, false, |mapper, backend| {
                mapper.command(command, backend)
            })?;
            let mut state = self.state(false);
            state.acknowledged = true;
            state.error = reply.error.clone();
            state.mapper = reply;
            return Ok(state);
        }
        let next = (!matches!(&command, automation_runtime::Command::Inspect))
            .then(|| self.next_macro_revision())
            .transpose()?;
        let mut state = self.command_inner(command, router)?;
        if let Some(next) = next {
            self.macro_revision = next;
        }
        state.macro_revision = self.macro_revision;
        Ok(state)
    }
    pub fn load_if_revision(
        &mut self,
        expected_revision: u64,
        clip: Clip,
        router: &mut dyn Engine,
    ) -> Result<automation_runtime::State> {
        ensure!(
            expected_revision == self.macro_revision,
            "Macro state changed while the file was loading"
        );
        self.command(automation_runtime::Command::Load { clip }, router)
    }
    fn command_inner(
        &mut self,
        command: automation_runtime::Command,
        router: &mut dyn Engine,
    ) -> Result<automation_runtime::State> {
        ensure!(
            !self.mapper.is_armed()
                || matches!(
                    &command,
                    automation_runtime::Command::Inspect
                        | automation_runtime::Command::Cancel
                        | automation_runtime::Command::RecordStop
                ),
            "Disarm Input Mapper before changing legacy automation"
        );
        self.validate_paused_automation(&command, router)?;
        // The legacy composite workflow remains a single synchronized clock. Independent programs are separate clocks.
        if !matches!(command, automation_runtime::Command::Inspect) {
            if let automation_runtime::Command::Forwarding { edges, .. } = &command {
                for edge in edges {
                    if let Some(source) = self.seats.get(edge.source) {
                        self.claims
                            .require_forwarding_source_idle(source.mouse, source.keyboard)?;
                    }
                }
            }
            // Validate legacy mutations in the old runtime first; malformed commands must preserve live lanes.
            self.inner.preflight(&command, router)?;
            if matches!(command, automation_runtime::Command::Cancel) {
                self.cancel(router)?;
            } else if !matches!(
                command,
                automation_runtime::Command::CancelReplication { .. }
            ) {
                ensure!(
                    !self
                        .lanes
                        .iter()
                        .enumerate()
                        .any(|(user, _)| self.studio_busy(user)),
                    "Stop independent user programs before changing synchronized composite automation"
                );
            }
        }
        let mut engine = ClaimEngine {
            inner: router,
            claims: &mut self.claims,
            origin: Origin::Composite,
            agent_access: self.agent_access.clone(),
            outputs: &self.outputs,
        };
        self.inner.command(command, &mut engine)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("macro_studio/mapper_delivery_tests.rs");
    include!("macro_studio/user_pause_tests.rs");

    #[test]
    fn mapper_sinks_fan_out_one_source_and_retire_without_releasing_other_owners() {
        use crate::routing_engine::ProgramOrigin;
        let (mut runtime, mut engine, _, keyboards) = two_user_runtime_and_engine();
        let raw = Packet::key(keyboards[0], 0x41, false);
        let first = Packet::key(keyboards[1], 0x42, false);
        let shared = Packet::key(keyboards[1], 0x43, false);
        runtime
            .claims
            .input(Origin::Manual(1), shared, &mut engine)
            .unwrap();
        let program = ProgramOrigin::Agent(7, 0);
        for (sink, output) in [(1, first), (2, shared)] {
            runtime
                .claims
                .input_output(
                    Origin::Mapper(program, 9, sink),
                    raw,
                    Some(output),
                    &[0, 1],
                    &mut engine,
                )
                .unwrap();
        }
        assert!(engine.key_held(keyboards[1], 0x42));
        assert!(engine.key_held(keyboards[1], 0x43));
        // A gate closing from another source releases the saved sink owner,
        // without recomputing it from the closing event's raw token or user.
        runtime
            .claims
            .release_mapper_sink(program, 9, 1, &mut engine)
            .unwrap();
        assert!(!engine.key_held(keyboards[1], 0x42));
        assert!(engine.key_held(keyboards[1], 0x43));
        runtime.claims.release_mapper(9, &mut engine).unwrap();
        assert!(engine.key_held(keyboards[1], 0x43));
        runtime
            .claims
            .release(Origin::Manual(1), &mut engine)
            .unwrap();
        assert!(!engine.key_held(keyboards[1], 0x43));
    }

    #[test]
    fn consumed_down_disposition_survives_disarm_repeats_until_matching_up() {
        let (mut runtime, mut engine, _, keyboards) = two_user_runtime_and_engine();
        let down = Packet::key(keyboards[0], 0x41, false);
        let up = released(down);
        let origin = Origin::Agent(7, 0);
        assert!(
            runtime
                .claims
                .consume_mapper_source(origin, down, true, &[0])
                .unwrap()
        );
        runtime.claims.clear(); // program retirement does not retire the input actor
        assert!(
            runtime
                .claims
                .consume_mapper_source(origin, down, false, &[0])
                .unwrap()
        );
        assert!(
            runtime
                .claims
                .consume_mapper_source(origin, up, false, &[0])
                .unwrap()
        );
        assert!(
            !runtime
                .claims
                .consume_mapper_source(origin, down, false, &[0])
                .unwrap()
        );
        assert!(engine.packets.is_empty());
        // A DOWN already sent through the ordinary route retains that route's UP.
        runtime.claims.input(origin, down, &mut engine).unwrap();
        assert!(
            !runtime
                .claims
                .consume_mapper_source(origin, down, true, &[0])
                .unwrap()
        );
        assert!(
            !runtime
                .claims
                .consume_mapper_source(origin, up, true, &[0])
                .unwrap()
        );
        runtime
            .claims
            .input_output(origin, up, None, &[], &mut engine)
            .unwrap();
        assert!(!engine.key_held(keyboards[0], 0x41));
    }

    #[test]
    fn selection_idle_blocks_raw_destination_and_path_without_delegating_or_releasing() {
        for path_only in [false, true] {
            let (mut runtime, mut engine, mice, keyboards) = two_user_runtime_and_engine();
            let output_user = usize::from(!path_only);
            let path = if path_only { vec![0, 1] } else { vec![] };
            runtime
                .claims
                .input_output(
                    Origin::Manual(0),
                    Packet::button(mice[0], 0, false),
                    Some(Packet::key(keyboards[output_user], 0x41, false)),
                    &path,
                    &mut engine,
                )
                .unwrap();
            let before = serde_json::to_value(&engine.packets).unwrap();
            assert!(runtime.require_selection_idle(0).is_err());
            assert!(runtime.require_selection_idle(1).is_err());
            let selection = crate::window_selection::Selection {
                request_id: "claim-test".into(),
                user: 1,
                pid: 101,
                hwnd: 10,
                creation_token: "100".into(),
            };
            let mut guarded = ClaimEngine {
                inner: &mut engine,
                claims: &mut runtime.claims,
                origin: Origin::Manual(0),
                agent_access: runtime.agent_access.clone(),
                outputs: &runtime.outputs,
            };
            assert!(guarded.select_window(&selection).is_err());
            assert_eq!(serde_json::to_value(&engine.packets).unwrap(), before);
            assert!(engine.selections.is_empty());
            assert_eq!(runtime.claims.1.len(), 1);
            runtime
                .claims
                .release(Origin::Manual(0), &mut engine)
                .unwrap();
            runtime.require_selection_idle(1).unwrap();
            let mut guarded = ClaimEngine {
                inner: &mut engine,
                claims: &mut runtime.claims,
                origin: Origin::Manual(0),
                agent_access: runtime.agent_access.clone(),
                outputs: &runtime.outputs,
            };
            guarded.select_window(&selection).unwrap();
            assert_eq!(engine.selections, vec![1]);
        }
    }

    #[test]
    fn selection_idle_rejects_suppressed_raw_claim_without_publishing_or_retiring_it() {
        let (mut runtime, mut engine, mice, _) = two_user_runtime_and_engine();
        runtime
            .claims
            .input_output(
                Origin::Agent(7, 0),
                Packet::button(mice[0], 3, false),
                None,
                &[],
                &mut engine,
            )
            .unwrap();
        assert!(runtime.claims.0.is_empty());
        assert_eq!(runtime.claims.1.len(), 1);
        assert!(runtime.claims.1.values().next().unwrap().output.is_none());
        runtime.require_selection_idle(1).unwrap();
        assert!(runtime.require_selection_idle(0).is_err());
        assert!(engine.packets.is_empty());
        assert_eq!(runtime.claims.source_button_mask(mice[0]), 1 << 3);
        assert_eq!(runtime.claims.1.len(), 1);
        runtime
            .claims
            .release(Origin::Agent(7, 0), &mut engine)
            .unwrap();
        runtime.require_selection_idle(0).unwrap();
    }

    #[test]
    fn selection_idle_preserves_failed_and_retained_saved_release_claims() {
        for retained in [false, true] {
            let (mut runtime, mut engine, _, keyboards) = two_user_runtime_and_engine();
            let origin = Origin::Agent(7, 0);
            let key = (keyboards[0], 0x41);
            runtime
                .claims
                .input(origin, Packet::key(key.0, key.1, false), &mut engine)
                .unwrap();
            if retained {
                engine.retained_key_releases.insert(key);
            } else {
                engine.fail_key_releases.insert(key);
            }
            // Retired clients no longer have a grant, but their failed saved
            // releases must still block a different caller selecting this user.
            assert!(runtime.cancel_agent(7, &mut engine).is_err());
            assert_eq!(runtime.agent_pending_owners(7), 1);
            let attempts = engine.key_release_attempts.len();
            let owners = runtime.claims.0.values().next().unwrap().owners.clone();
            assert!(runtime.require_selection_idle(0).is_err());
            runtime.require_selection_idle(1).unwrap();
            assert_eq!(engine.key_release_attempts.len(), attempts);
            assert_eq!(runtime.claims.1.len(), 1);
            assert!(runtime.claims.0.values().next().unwrap().owners == owners);
            assert!(engine.key_held(key.0, key.1));
            engine.retained_key_releases.clear();
            engine.fail_key_releases.clear();
            runtime.cancel_agent(7, &mut engine).unwrap();
            runtime.require_selection_idle(0).unwrap();
        }
    }

    #[test]
    fn selection_idle_preserves_local_and_agent_recordings_and_ignores_other_lanes() {
        let (mut runtime, engine, _, _) = two_user_runtime_and_engine();
        runtime.lanes[0].recorder.start();
        let before = serde_json::to_value(runtime.studio_state(None)).unwrap();
        assert!(runtime.require_selection_idle(0).is_err());
        runtime.require_selection_idle(1).unwrap();
        assert_eq!(
            serde_json::to_value(runtime.studio_state(None)).unwrap(),
            before
        );
        runtime.lanes[0] = Lane::default();
        let mut recorder = Recorder::default();
        recorder.start();
        runtime.lanes[0].agent_recording = Some(AgentRecording {
            client: 7,
            recorder,
            started: Instant::now(),
        });
        let before = serde_json::to_value(runtime.studio_state(None)).unwrap();
        assert!(runtime.require_selection_idle(0).is_err());
        runtime.require_selection_idle(1).unwrap();
        assert_eq!(
            serde_json::to_value(runtime.studio_state(None)).unwrap(),
            before
        );
        assert!(engine.packets.is_empty());
    }

    #[test]
    fn selection_idle_preserves_local_and_agent_playing_lanes_before_first_input() {
        for owner in [
            None,
            Some(AgentStudioOwner {
                client: 7,
                user: 0,
                hwnd: 10,
                pid: 101,
            }),
        ] {
            let (mut runtime, engine, _, keyboards) = two_user_runtime_and_engine();
            runtime.lanes[0].agent_owner = owner;
            runtime.lanes[0].playback =
                Some(Playback::start(key_clip(0, keyboards[0]), Some(1), 1.0).unwrap());
            assert!(runtime.claims.1.is_empty());
            let before = serde_json::to_value(runtime.studio_state(None)).unwrap();
            assert!(runtime.require_selection_idle(0).is_err());
            runtime.require_selection_idle(1).unwrap();
            assert_eq!(
                serde_json::to_value(runtime.studio_state(None)).unwrap(),
                before
            );
            assert!(engine.packets.is_empty());
        }
    }

    #[test]
    fn selection_idle_blocks_completed_lane_with_future_replication_and_no_holds() {
        use crate::routing_engine::ProgramOrigin;
        let (mut runtime, mut engine, mice, keyboards) = two_user_runtime_and_engine();
        runtime
            .command(
                automation_runtime::Command::Graph {
                    edges: vec![automation_runtime::Edge {
                        source: 0,
                        target: 1,
                        mouse: true,
                        keyboard: false,
                        text: false,
                        options: mux_core::replication::EdgeOptions {
                            click_delay_ms: 60_000,
                            ..Default::default()
                        },
                    }],
                    expected_revision: 0,
                },
                &mut engine,
            )
            .unwrap();
        {
            let mut guarded = ClaimEngine {
                inner: &mut engine,
                claims: &mut runtime.claims,
                origin: Origin::Studio(0),
                agent_access: runtime.agent_access.clone(),
                outputs: &runtime.outputs,
            };
            for up in [false, true] {
                runtime
                    .inner
                    .action(
                        Action::Packet {
                            user: 0,
                            packet: Packet::button(mice[0], 0, up),
                        },
                        &mut guarded,
                        false,
                    )
                    .unwrap();
            }
        }
        let mut playback = Playback::start(key_clip(0, keyboards[0]), Some(1), 1.0).unwrap();
        let _ = playback.tick(Duration::from_secs(101));
        assert_eq!(playback.snapshot().state, PlaybackState::Completed);
        runtime.lanes[0].playback = Some(playback);
        assert!(runtime.claims.1.is_empty());
        assert!(runtime.inner.has_pending(ProgramOrigin::Studio(0)));
        let before = serde_json::to_value((
            runtime.studio_state(None),
            runtime.inner.state(false),
            &engine.packets,
        ))
        .unwrap();
        assert!(runtime.require_selection_idle(0).is_err());
        assert!(runtime.require_selection_idle(1).is_err());
        assert_eq!(
            serde_json::to_value((
                runtime.studio_state(None),
                runtime.inner.state(false),
                &engine.packets
            ))
            .unwrap(),
            before
        );
    }

    #[test]
    fn source_button_mask_tracks_disabled_and_key_mapped_claims_by_origin() {
        use crate::routing_engine::ProgramOrigin;

        let mouse = mux_core::emulated::device(mux_core::DeviceKind::Mouse, 11)
            .unwrap()
            .handle as u32;
        let keyboard = mux_core::emulated::device(mux_core::DeviceKind::Keyboard, 11)
            .unwrap()
            .handle as u32;
        let raw = Packet::button(mouse, 3, false);
        let mapped = Packet::key_scanned(keyboard, 0x75, 0x3f, 0, false);
        let (mut claims, mut engine) = (Claims::default(), MockEngine::default());

        claims
            .input_output(Origin::Manual(0), raw, None, &[], &mut engine)
            .unwrap();
        claims
            .input_output(
                Origin::Replica(ProgramOrigin::Agent(7, 0), 0),
                raw,
                Some(mapped),
                &[],
                &mut engine,
            )
            .unwrap();
        assert_eq!(claims.source_button_mask(mouse), 1 << 3);

        claims
            .release_replication(Some(ProgramOrigin::Agent(7, 0)), &mut engine)
            .unwrap();
        assert_eq!(claims.source_button_mask(mouse), 1 << 3);
        claims.release(Origin::Manual(0), &mut engine).unwrap();
        assert_eq!(claims.source_button_mask(mouse), 0);
    }

    #[test]
    fn legacy_disabled_mapping_keeps_raw_source_button_for_conditionals() {
        let mouse = mux_core::emulated::device(mux_core::DeviceKind::Mouse, 12)
            .unwrap()
            .handle as u32;
        let primary = Packet::button(mouse, 0, false);
        let mapped_secondary = Packet::button(mouse, 1, false);
        let (mut claims, mut engine) = (Claims::default(), MockEngine::default());
        // The legacy cursor map disables the output button after output-rule mapping.
        engine
            .disabled_buttons
            .insert((mouse, mapped_secondary.flags));
        claims
            .input_output(
                Origin::Manual(1),
                primary,
                Some(mapped_secondary),
                &[],
                &mut engine,
            )
            .unwrap();

        assert_eq!(claims.source_button_mask(mouse), 1);
        assert!(engine.packets.is_empty());

        claims
            .input_output(
                Origin::Manual(1),
                Packet::button(mouse, 0, true),
                None,
                &[],
                &mut engine,
            )
            .unwrap();
        assert_eq!(claims.source_button_mask(mouse), 0);
    }

    #[test]
    fn legacy_disabled_mapping_still_reaches_earlier_button_interceptor() {
        let mouse = mux_core::emulated::device(mux_core::DeviceKind::Mouse, 14)
            .unwrap()
            .handle as u32;
        let down = Packet::button(mouse, 0, false);
        let up = Packet::button(mouse, 0, true);
        let (mut claims, mut engine) = (Claims::default(), MockEngine::default());
        engine.disabled_buttons.insert((mouse, down.flags));
        engine.intercepted_buttons.insert((mouse, down.flags));
        claims
            .input_output(Origin::Manual(0), down, Some(down), &[], &mut engine)
            .unwrap();
        assert_eq!(claims.source_button_mask(mouse), 1);
        assert!(engine.button_held(mouse, 0));
        claims
            .input_output(Origin::Manual(0), up, None, &[], &mut engine)
            .unwrap();
        assert_eq!(claims.source_button_mask(mouse), 0);
        assert!(!engine.button_held(mouse, 0));
        assert_eq!(engine.packets.len(), 2);
        assert_eq!(engine.packets[0].flags, down.flags);
        assert_eq!(engine.packets[1].flags, up.flags);
    }

    #[test]
    fn mapped_button_claim_keeps_the_unmodified_source_predicate() {
        let mouse = mux_core::emulated::device(mux_core::DeviceKind::Mouse, 13)
            .unwrap()
            .handle as u32;
        let primary = Packet::button(mouse, 0, false);
        let mapped_secondary = Packet::button(mouse, 1, false);
        let (mut claims, mut engine) = (Claims::default(), MockEngine::default());
        claims
            .input_output(
                Origin::Manual(2),
                primary,
                Some(mapped_secondary),
                &[],
                &mut engine,
            )
            .unwrap();

        assert_eq!(claims.source_button_mask(mouse), 1);
        assert!(engine.button_held(mouse, 1));
        assert!(!engine.button_held(mouse, 0));
        claims
            .input_output(
                Origin::Manual(2),
                Packet::button(mouse, 0, true),
                None,
                &[],
                &mut engine,
            )
            .unwrap();
        assert_eq!(claims.source_button_mask(mouse), 0);
        assert!(!engine.button_held(mouse, 1));
    }

    #[test]
    fn settings_cleanup_releases_only_claims_from_selected_devices() {
        use crate::routing_engine::ProgramOrigin;

        let selected_mouse = mux_core::emulated::device(mux_core::DeviceKind::Mouse, 21)
            .unwrap()
            .handle as u32;
        let selected_keyboard = mux_core::emulated::device(mux_core::DeviceKind::Keyboard, 21)
            .unwrap()
            .handle as u32;
        let other_mouse = mux_core::emulated::device(mux_core::DeviceKind::Mouse, 22)
            .unwrap()
            .handle as u32;
        let other_keyboard = mux_core::emulated::device(mux_core::DeviceKind::Keyboard, 22)
            .unwrap()
            .handle as u32;
        let selected_key = Packet::key_scanned(selected_keyboard, 0x75, 0x3f, 0, false);
        let other_key = Packet::key_scanned(other_keyboard, 0x41, 0x1e, 0, false);
        let (mut claims, mut engine) = (Claims::default(), MockEngine::default());

        // A raw-only legacy-disabled Back hold survives ordinary router pruning.
        claims
            .input_output(
                Origin::Agent(7, 0),
                Packet::button(selected_mouse, 4, false),
                None,
                &[],
                &mut engine,
            )
            .unwrap();
        // A key output sourced from this user's mouse must be released with its device claims.
        claims
            .input_output(
                Origin::Replica(ProgramOrigin::Agent(8, 0), 0),
                Packet::button(selected_mouse, 0, false),
                Some(selected_key),
                &[],
                &mut engine,
            )
            .unwrap();
        // Other-user raw ownership, including a destination replica, is outside the cleanup.
        claims
            .input_output(
                Origin::Replica(ProgramOrigin::Manual(1), 1),
                Packet::button(other_mouse, 3, false),
                None,
                &[],
                &mut engine,
            )
            .unwrap();
        claims
            .input(Origin::Manual(1), other_key, &mut engine)
            .unwrap();

        claims
            .release_user_input_claims(
                &ResolvedSeat {
                    name: "selected".into(),
                    mouse: selected_mouse,
                    keyboard: Some(selected_keyboard),
                    target_pid: 0,
                },
                &mut engine,
            )
            .unwrap();

        assert_eq!(claims.source_button_mask(selected_mouse), 0);
        assert_eq!(claims.source_button_mask(other_mouse), 1 << 3);
        assert!(!engine.key_held(selected_keyboard, 0x75));
        assert!(engine.key_held(other_keyboard, 0x41));
    }

    #[test]
    fn cancelling_replicas_preserves_ordinary_and_other_program_claims() {
        use crate::routing_engine::ProgramOrigin;
        let keyboard = mux_core::emulated::device(mux_core::DeviceKind::Keyboard, 2)
            .unwrap()
            .handle as u32;
        let down = Packet::key_scanned(keyboard, 0x41, 0x1e, 0, false);
        let (mut claims, mut engine) = (Claims::default(), MockEngine::default());
        claims.input(Origin::Manual(1), down, &mut engine).unwrap();
        claims
            .input(
                Origin::Replica(ProgramOrigin::Studio(0), 0),
                down,
                &mut engine,
            )
            .unwrap();
        claims
            .input(
                Origin::Replica(ProgramOrigin::Manual(2), 2),
                down,
                &mut engine,
            )
            .unwrap();
        claims
            .release_replication(Some(ProgramOrigin::Studio(0)), &mut engine)
            .unwrap();
        assert!(engine.key_held(keyboard, 0x41));
        claims.release_replication(None, &mut engine).unwrap();
        assert!(engine.key_held(keyboard, 0x41));
        assert_eq!(engine.packets.len(), 1);
        claims.release(Origin::Manual(1), &mut engine).unwrap();
        assert!(!engine.key_held(keyboard, 0x41));
        assert_eq!(engine.packets.len(), 2);
    }

    #[test]
    fn replica_up_preserves_another_source_in_the_same_composite_program() {
        use crate::routing_engine::ProgramOrigin;
        let keyboard = mux_core::emulated::device(mux_core::DeviceKind::Keyboard, 2)
            .unwrap()
            .handle as u32;
        let down = Packet::key_scanned(keyboard, 0x41, 0x1e, 0, false);
        let (mut claims, mut engine) = (Claims::default(), MockEngine::default());
        for source in [0, 2] {
            claims
                .input(
                    Origin::Replica(ProgramOrigin::Composite, source),
                    down,
                    &mut engine,
                )
                .unwrap();
        }
        claims
            .input(
                Origin::Replica(ProgramOrigin::Composite, 0),
                released(down),
                &mut engine,
            )
            .unwrap();
        assert!(engine.key_held(keyboard, 0x41));
        claims
            .input(
                Origin::Replica(ProgramOrigin::Composite, 2),
                released(down),
                &mut engine,
            )
            .unwrap();
        assert!(!engine.key_held(keyboard, 0x41));
        assert_eq!(engine.packets.len(), 2);
    }

    #[test]
    fn converged_forwarded_holds_and_stale_up_do_not_release_another_source() {
        use crate::routing_engine::ProgramOrigin;
        let mouse = |index| {
            mux_core::emulated::device(mux_core::DeviceKind::Mouse, index)
                .unwrap()
                .handle as u32
        };
        let (source_a, source_b, sink) = (mouse(1), mouse(2), mouse(3));
        let (raw_a, raw_b, output) = (
            Packet::button(source_a, 0, false),
            Packet::button(source_b, 0, false),
            Packet::button(sink, 0, false),
        );
        let (mut claims, mut engine) = (Claims::default(), MockEngine::default());
        for (source, raw) in [(0, raw_a), (1, raw_b)] {
            claims
                .input_output(
                    Origin::Forward(ProgramOrigin::Composite, source),
                    raw,
                    Some(output),
                    &[source, 2],
                    &mut engine,
                )
                .unwrap();
        }
        assert_eq!(engine.packets.len(), 1);
        claims
            .release_matching(&mut engine, |owner, _| {
                owner.origin == Origin::Forward(ProgramOrigin::Composite, 0)
            })
            .unwrap();
        assert!(engine.button_held(sink, 0));
        assert!(claims.consume_forwarding_tombstone(
            ProgramOrigin::Composite,
            0,
            Packet::button(source_a, 0, true)
        ));
        assert!(engine.button_held(sink, 0));
        claims
            .input_output(
                Origin::Forward(ProgramOrigin::Composite, 1),
                Packet::button(source_b, 0, true),
                None,
                &[1, 2],
                &mut engine,
            )
            .unwrap();
        assert!(!engine.button_held(sink, 0));
        assert_eq!(engine.packets.len(), 2);
    }

    #[test]
    fn repeated_disconnected_clients_do_not_accumulate_forwarding_tombstones() {
        use crate::routing_engine::ProgramOrigin;
        let source = mux_core::emulated::device(mux_core::DeviceKind::Mouse, 1)
            .unwrap()
            .handle as u32;
        let sink = mux_core::emulated::device(mux_core::DeviceKind::Mouse, 2)
            .unwrap()
            .handle as u32;
        let (raw, output) = (
            Packet::button(source, 0, false),
            Packet::button(sink, 0, false),
        );
        let (mut claims, mut engine) = (Claims::default(), MockEngine::default());
        for client in 1..=64 {
            let program = ProgramOrigin::Agent(client, 0);
            claims
                .input_output(
                    Origin::Forward(program, 0),
                    raw,
                    Some(output),
                    &[0, 1],
                    &mut engine,
                )
                .unwrap();
            claims
                .release_forwarding(Some(program), &mut engine)
                .unwrap();
            assert_eq!(claims.2.len(), 1);
            claims.retire_forwarding_tombstones(client);
            assert!(claims.2.is_empty());
        }
    }

    #[derive(Default)]
    struct MockEngine {
        selections: Vec<usize>,
        pending_window: Option<Window>,
        moves: Vec<(usize, i32, i32)>,
        mouse_activity: Vec<usize>,
        disabled_keyboard_users: BTreeSet<usize>,
        keys: BTreeSet<(u32, u32)>,
        buttons: BTreeSet<(u32, u32)>,
        disabled_buttons: BTreeSet<(u32, u32)>,
        intercepted_buttons: BTreeSet<(u32, u32)>,
        packets: Vec<Packet>,
        clear_all_keys: BTreeSet<(usize, u32, u32)>,
        paused: bool,
        window: Option<Window>,
        keyboard_override: Option<Window>,
        fail_after: Option<usize>,
        fail_key_releases: BTreeSet<(u32, u32)>,
        retained_key_releases: BTreeSet<(u32, u32)>,
        key_release_attempts: Vec<(u32, u32)>,
        fail_release_all: bool,
        release_all_attempts: usize,
        user_pauses: BTreeSet<usize>,
    }

    impl Engine for MockEngine {
        fn user_pause_state(&self, user: usize) -> crate::routing_engine::UserPauseState {
            crate::routing_engine::UserPauseState {
                user,
                supported: true,
                paused: self.user_pauses.contains(&user),
                pending_releases: 0,
                error: None,
            }
        }
        fn pause_user(&mut self, user: usize, paused: bool) -> Result<()> {
            ensure!(paused || !self.paused, "Global pause overrides resume");
            if paused {
                self.user_pauses.insert(user);
            } else {
                self.user_pauses.remove(&user);
            }
            Ok(())
        }
        fn forwarding_supported(&self) -> bool {
            true
        }
        fn set_forwarding_sources(&mut self, _sources: &[bool]) -> Result<()> {
            Ok(())
        }
        fn pending_attachment_window(&self, _: usize) -> Result<Option<Window>> {
            Ok(self.pending_window)
        }
        fn select_window(&mut self, selection: &crate::window_selection::Selection) -> Result<()> {
            self.selections.push(selection.user);
            Ok(())
        }
        fn keyboard_enabled(&self, user: usize) -> bool {
            !self.disabled_keyboard_users.contains(&user)
        }
        fn note_mouse_activity(&mut self, user: usize) -> Result<()> {
            self.mouse_activity.push(user);
            Ok(())
        }
        fn input(&mut self, packet: Packet) -> Result<()> {
            if packet.kind == 1 && packet.flags & 1 != 0 {
                let key = (packet.device, packet.data);
                self.key_release_attempts.push(key);
                ensure!(
                    !self.fail_key_releases.contains(&key),
                    "controlled key release failure"
                );
                // Desktop routing can return Ok while retaining a failed Up.
                if self.retained_key_releases.contains(&key) {
                    return Ok(());
                }
            }
            ensure!(
                self.fail_after
                    .is_none_or(|limit| self.packets.len() < limit),
                "controlled delivery failure"
            );
            if self.button_output_disabled(packet)
                && !self
                    .intercepted_buttons
                    .contains(&(packet.device, packet.flags))
            {
                return Ok(());
            }
            if let Some((token, up)) = Token::parse(packet) {
                let held = if token.kind == 1 {
                    &mut self.keys
                } else {
                    &mut self.buttons
                };
                if up {
                    held.remove(&(token.device, token.code));
                } else {
                    held.insert((token.device, token.code));
                }
            }
            self.packets.push(packet);
            Ok(())
        }

        fn key_held(&self, device: u32, key: u32) -> bool {
            self.keys.contains(&(device, key))
        }

        fn button_held(&self, device: u32, button: u32) -> bool {
            self.buttons.contains(&(device, button))
        }

        fn button_output_disabled(&self, packet: Packet) -> bool {
            packet.kind == 0
                && self
                    .disabled_buttons
                    .contains(&(packet.device, packet.flags))
        }

        fn paused(&self) -> bool {
            self.paused
        }

        fn release_all(&mut self) -> Result<()> {
            self.release_all_attempts += 1;
            ensure!(!self.fail_release_all, "controlled global release failure");
            self.keys.clear();
            self.buttons.clear();
            Ok(())
        }

        fn pause(&mut self, value: bool) -> Result<()> {
            self.paused = value;
            Ok(())
        }

        fn settings(&mut self, _: usize, _: CursorSettings) -> Result<()> {
            Ok(())
        }

        fn tick(&mut self) -> Result<(bool, Vec<RoutingState>)> {
            Ok((false, Vec::new()))
        }

        fn pointer(&self, _: usize) -> Result<(i32, i32)> {
            Ok((0, 0))
        }

        fn selected_window(&self, _: usize) -> Result<Option<Window>> {
            Ok(self.window)
        }

        fn mouse_window(&self, user: usize) -> Result<Option<Window>> {
            self.selected_window(user)
        }

        fn keyboard_window(&self, user: usize) -> Result<Option<Window>> {
            Ok(self.keyboard_override.or(self.selected_window(user)?))
        }

        fn movement_window(&self, user: usize, _: i32, _: i32) -> Result<Option<Window>> {
            self.selected_window(user)
        }

        fn packet_window(&self, user: usize, packet: Packet) -> Result<Option<Window>> {
            if packet.kind == 0 {
                self.mouse_window(user)
            } else {
                self.keyboard_window(user)
            }
        }

        fn annotation_clears_all(&self, user: usize, packet: Packet) -> bool {
            packet.kind == 1
                && packet.flags & 1 == 0
                && self
                    .clear_all_keys
                    .contains(&(user, packet.device, packet.data))
        }

        fn move_to(&mut self, user: usize, x: i32, y: i32) -> Result<()> {
            self.moves.push((user, x, y));
            Ok(())
        }
    }

    fn runtime_and_engine() -> (Runtime, MockEngine, u32, u32) {
        let mouse = mux_core::emulated::device(mux_core::DeviceKind::Mouse, 1)
            .unwrap()
            .handle as u32;
        let keyboard = mux_core::emulated::device(mux_core::DeviceKind::Keyboard, 1)
            .unwrap()
            .handle as u32;
        let seat = ResolvedSeat {
            name: "test".into(),
            mouse,
            keyboard: Some(keyboard),
            target_pid: 0,
        };
        (
            Runtime::new(vec![seat]),
            MockEngine::default(),
            mouse,
            keyboard,
        )
    }

    #[test]
    fn guarded_file_load_rejects_completed_studio_mutation_with_ack_undrained() {
        let (mut runtime, mut engine, _, keyboard) = connected_studio();
        let observed_at_click = runtime.state(false).macro_revision;
        let studio_clip = key_clip(0, keyboard);
        let studio_bytes = studio_clip.to_json().unwrap();
        let _queued_ack = runtime
            .agent_studio_command(
                7,
                Command::Load {
                    user: 0,
                    clip: studio_clip,
                },
                &mut engine,
            )
            .unwrap();
        assert!(runtime.macro_revision() > observed_at_click);
        let file_clip = Clip::new(vec![mux_core::automation::TimedAction {
            at_micros: 0,
            action: Action::MoveTo {
                user: 0,
                x: 12,
                y: 18,
            },
        }])
        .unwrap();
        assert!(
            runtime
                .load_if_revision(observed_at_click, file_clip, &mut engine)
                .is_err()
        );
        assert_eq!(
            runtime.studio_state(Some(0)).users[0]
                .clip
                .as_ref()
                .unwrap()
                .to_json()
                .unwrap(),
            studio_bytes
        );
        assert_eq!(runtime.state(false).loaded_events, 0);
    }

    fn two_user_runtime_and_engine() -> (Runtime, MockEngine, [u32; 2], [u32; 2]) {
        let mut mice = [0; 2];
        let mut keyboards = [0; 2];
        let seats = (0..2)
            .map(|index| {
                mice[index] =
                    mux_core::emulated::device(mux_core::DeviceKind::Mouse, index as u32 + 1)
                        .unwrap()
                        .handle as u32;
                keyboards[index] =
                    mux_core::emulated::device(mux_core::DeviceKind::Keyboard, index as u32 + 1)
                        .unwrap()
                        .handle as u32;
                ResolvedSeat {
                    name: format!("test-{index}"),
                    mouse: mice[index],
                    keyboard: Some(keyboards[index]),
                    target_pid: 0,
                }
            })
            .collect();
        (Runtime::new(seats), MockEngine::default(), mice, keyboards)
    }

    fn keyboard_test_key(device: u32, key: u32, up: bool) -> Packet {
        let scan = match key {
            27 => 0x01,
            0x41 => 0x1e,
            0x42 => 0x30,
            0x43 => 0x2e,
            0x44 => 0x20,
            _ => panic!("keyboard gate test needs an explicit scan code"),
        };
        Packet::key_scanned(device, key, scan, 0, up)
    }

    fn keyboard_test_graph(runtime: &mut Runtime, engine: &mut MockEngine, interval: u32) {
        let expected_revision = runtime.state(false).graph_revision;
        runtime
            .command(
                automation_runtime::Command::Graph {
                    edges: vec![automation_runtime::Edge {
                        source: 0,
                        target: 1,
                        mouse: true,
                        keyboard: true,
                        text: true,
                        options: mux_core::replication::EdgeOptions {
                            step_interval_ms: interval,
                            ..Default::default()
                        },
                    }],
                    expected_revision,
                },
                engine,
            )
            .unwrap();
    }

    #[test]
    fn disabled_keyboard_sources_do_not_record_or_queue_but_other_users_continue() {
        let (mut runtime, mut engine, _, keyboards) = two_user_runtime_and_engine();
        keyboard_test_graph(&mut runtime, &mut engine, 1000);
        runtime
            .command(automation_runtime::Command::RecordStart, &mut engine)
            .unwrap();
        engine.disabled_keyboard_users.insert(0);
        runtime
            .packet(
                keyboard_test_key(keyboards[0], 0x41, false),
                &mut engine,
                true,
            )
            .unwrap();
        runtime
            .action(
                Action::Text {
                    user: 0,
                    text: "ignored".into(),
                },
                &mut engine,
                true,
            )
            .unwrap();
        assert!(engine.packets.is_empty());
        assert_eq!(runtime.state(false).recorded_events, 0);
        assert_eq!(runtime.state(false).replication.pending_events, 0);

        for up in [false, true] {
            runtime
                .packet(keyboard_test_key(keyboards[1], 0x42, up), &mut engine, true)
                .unwrap();
        }
        assert_eq!(engine.packets.len(), 2);
        assert_eq!(runtime.state(false).recorded_events, 2);
        runtime
            .command(automation_runtime::Command::RecordStop, &mut engine)
            .unwrap();
        runtime
            .studio_command(
                Command::Record {
                    user: 0,
                    active: true,
                },
                &mut engine,
            )
            .unwrap();
        runtime
            .packet(
                keyboard_test_key(keyboards[0], 27, false),
                &mut engine,
                true,
            )
            .unwrap();
        runtime
            .action(
                Action::Text {
                    user: 0,
                    text: "still ignored".into(),
                },
                &mut engine,
                true,
            )
            .unwrap();
        let lane = &runtime.studio_state(None).users[0];
        assert!(lane.recording);
        assert_eq!(lane.recorded_events, 0);
        assert_eq!(engine.packets.len(), 2);
        assert_eq!(runtime.state(false).replication.pending_events, 0);
    }

    #[test]
    fn disabled_manual_keyboard_does_not_interrupt_studio_playback() {
        let (mut runtime, mut engine, _, keyboards) = two_user_runtime_and_engine();
        for (user, keyboard) in keyboards.into_iter().enumerate() {
            runtime
                .studio_command(
                    Command::Load {
                        user,
                        clip: key_clip(user, keyboard),
                    },
                    &mut engine,
                )
                .unwrap();
            runtime
                .studio_command(
                    Command::Play {
                        user,
                        options: PlaybackOptions {
                            delay_ms: 60_000,
                            ..Default::default()
                        },
                    },
                    &mut engine,
                )
                .unwrap();
        }
        engine.disabled_keyboard_users.insert(0);
        for key in [27, 0x42] {
            runtime
                .packet(
                    keyboard_test_key(keyboards[0], key, false),
                    &mut engine,
                    true,
                )
                .unwrap();
        }
        runtime
            .action(
                Action::Text {
                    user: 0,
                    text: "ignored".into(),
                },
                &mut engine,
                true,
            )
            .unwrap();
        assert!(engine.packets.is_empty());
        for user in &runtime.studio_state(None).users {
            assert_eq!(user.playback.as_ref().unwrap().state, "playing");
        }
        engine.disabled_keyboard_users.remove(&0);
        runtime
            .packet(
                keyboard_test_key(keyboards[0], 27, false),
                &mut engine,
                true,
            )
            .unwrap();
        let state = runtime.studio_state(None);
        assert_eq!(state.users[0].playback.as_ref().unwrap().state, "cancelled");
        assert_eq!(state.users[1].playback.as_ref().unwrap().state, "playing");
    }

    #[test]
    fn disabled_replica_skips_new_keys_and_text_but_saved_up_can_retry() {
        let (mut runtime, mut engine, _, keyboards) = two_user_runtime_and_engine();
        keyboard_test_graph(&mut runtime, &mut engine, 0);
        let down = keyboard_test_key(keyboards[0], 0x41, false);
        runtime.packet(down, &mut engine, true).unwrap();
        assert!(engine.key_held(keyboards[0], 0x41));
        assert!(engine.key_held(keyboards[1], 0x41));
        engine.disabled_keyboard_users.extend([0, 1]);
        engine.fail_key_releases.insert((keyboards[1], 0x41));
        assert!(runtime.packet(released(down), &mut engine, true).is_err());
        assert!(!engine.key_held(keyboards[0], 0x41));
        assert!(engine.key_held(keyboards[1], 0x41));
        engine.fail_key_releases.clear();
        runtime.packet(released(down), &mut engine, true).unwrap();
        assert!(engine.keys.is_empty());
        assert!(runtime.claims.0.is_empty() && runtime.claims.1.is_empty());

        engine.disabled_keyboard_users.remove(&0);
        let before = engine.packets.len();
        let next = keyboard_test_key(keyboards[0], 0x42, false);
        runtime.packet(next, &mut engine, true).unwrap();
        runtime
            .action(
                Action::Text {
                    user: 0,
                    text: "source only".into(),
                },
                &mut engine,
                true,
            )
            .unwrap();
        runtime.packet(released(next), &mut engine, true).unwrap();
        assert!(
            engine.packets[before..]
                .iter()
                .all(|packet| packet.device == keyboards[0])
        );
        assert_eq!(runtime.state(false).replication.pending_events, 0);

        // A destination can be disabled after a paced copy was admitted.
        keyboard_test_graph(&mut runtime, &mut engine, 1000);
        engine.disabled_keyboard_users.remove(&1);
        let queued = keyboard_test_key(keyboards[0], 0x43, false);
        runtime.packet(queued, &mut engine, true).unwrap();
        assert_eq!(runtime.state(false).replication.pending_events, 1);
        engine.disabled_keyboard_users.insert(1);
        runtime.tick(&mut engine).unwrap();
        assert_eq!(runtime.state(false).replication.pending_events, 0);
        assert!(engine.key_held(keyboards[0], 0x43));
        assert!(!engine.key_held(keyboards[1], 0x43));
        runtime.cancel(&mut engine).unwrap();
        assert!(engine.keys.is_empty());

        // Toggle cleanup must cancel future text and release an already delivered
        // replica before a disable/re-enable can admit work again.
        engine.disabled_keyboard_users.remove(&1);
        let held = keyboard_test_key(keyboards[0], 0x44, false);
        runtime.packet(held, &mut engine, true).unwrap();
        runtime.tick(&mut engine).unwrap();
        assert!(engine.key_held(keyboards[1], 0x44));
        runtime
            .action(
                Action::Text {
                    user: 0,
                    text: "cancel on toggle".into(),
                },
                &mut engine,
                true,
            )
            .unwrap();
        assert_eq!(runtime.state(false).replication.pending_events, 1);
        engine.fail_key_releases.insert((keyboards[1], 0x44));
        assert!(runtime.keyboard_settings_change(0, &mut engine).is_err());
        assert!(
            engine.keyboard_enabled(0),
            "a failed release cannot commit disabled settings"
        );
        assert!(engine.key_held(keyboards[1], 0x44));
        assert_eq!(runtime.state(false).replication.pending_events, 0);
        engine.fail_key_releases.clear();
        runtime.keyboard_settings_change(0, &mut engine).unwrap();
        assert!(engine.keys.is_empty());
        engine.disabled_keyboard_users.insert(0);
        engine.disabled_keyboard_users.remove(&0);
        let delivered = engine.packets.len();
        runtime.tick(&mut engine).unwrap();
        assert_eq!(engine.packets.len(), delivered);
        assert_eq!(runtime.state(false).replication.pending_events, 0);
    }

    #[test]
    fn disabled_agent_keyboard_keeps_source_permissions_and_skips_irrelevant_pid_checks() {
        let (mut runtime, mut engine, mice, keyboards) = two_user_runtime_and_engine();
        keyboard_test_graph(&mut runtime, &mut engine, 0);
        let mut access = (*scoped_access(mice[0], keyboards[0])).clone();
        access.session_control = true;
        access.active_users.insert(1);
        runtime.agent_access = Arc::new(access);
        engine.window = Some(Window { hwnd: 10, pid: 101 });
        engine.disabled_keyboard_users.insert(1);
        let down = keyboard_test_key(keyboards[0], 0x41, false);
        runtime
            .agent_action(
                7,
                Action::Packet {
                    user: 0,
                    packet: down,
                },
                &mut engine,
            )
            .unwrap();
        runtime
            .agent_action(
                7,
                Action::Packet {
                    user: 0,
                    packet: released(down),
                },
                &mut engine,
            )
            .unwrap();
        assert_eq!(
            engine.packets.len(),
            2,
            "disabled destination needs no extra user/PID grant"
        );
        runtime
            .agent_studio_command(7, record_command(true), &mut engine)
            .unwrap();
        engine.disabled_keyboard_users.insert(0);
        engine.keyboard_override = Some(Window { hwnd: 20, pid: 202 });
        let before = engine.packets.len();
        runtime
            .agent_action(
                7,
                Action::Packet {
                    user: 0,
                    packet: down,
                },
                &mut engine,
            )
            .unwrap();
        runtime
            .agent_action(
                7,
                Action::Text {
                    user: 0,
                    text: "ignored".into(),
                },
                &mut engine,
            )
            .unwrap();
        assert_eq!(engine.packets.len(), before);
        assert_eq!(runtime.studio_state(None).users[0].recorded_events, 0);
        assert_eq!(runtime.state(false).replication.pending_events, 0);

        let foreign_device = runtime
            .agent_action(
                7,
                Action::Packet {
                    user: 0,
                    packet: keyboard_test_key(keyboards[1], 0x42, false),
                },
                &mut engine,
            )
            .unwrap_err();
        assert!(
            foreign_device
                .downcast_ref::<AgentScopeViolation>()
                .is_some()
        );
        let missing_user = runtime
            .agent_action(
                7,
                Action::Text {
                    user: 1,
                    text: "not granted".into(),
                },
                &mut engine,
            )
            .unwrap_err();
        assert!(missing_user.downcast_ref::<AgentScopeViolation>().is_some());
        engine.disabled_keyboard_users.remove(&0);
        let denied_pid = runtime
            .agent_action(
                7,
                Action::Packet {
                    user: 0,
                    packet: down,
                },
                &mut engine,
            )
            .unwrap_err();
        assert!(denied_pid.downcast_ref::<AgentScopeViolation>().is_some());
        assert_eq!(engine.packets.len(), before);
    }

    #[test]
    fn mouse_to_key_outputs_and_their_replicas_survive_disabled_keyboard_sources() {
        let (mut runtime, mut engine, mice, keyboards) = two_user_runtime_and_engine();
        keyboard_test_graph(&mut runtime, &mut engine, 0);
        for user in 0..2 {
            let mut settings = mux_core::output::Settings::default();
            settings.buttons[0] = mux_core::output::MouseOutput::Key { key: 0x46 };
            settings
                .keys
                .insert(0x44, mux_core::output::KeyOutput::Key { key: 0x47 });
            runtime.outputs.set(user, settings).unwrap();
        }
        engine.disabled_keyboard_users.extend([0, 1]);
        runtime
            .packet(
                keyboard_test_key(keyboards[0], 0x44, false),
                &mut engine,
                true,
            )
            .unwrap();
        assert!(
            engine.packets.is_empty(),
            "raw keyboard gate precedes key translation"
        );
        runtime
            .packet(Packet::button(mice[0], 0, false), &mut engine, true)
            .unwrap();
        for keyboard in keyboards {
            assert!(engine.key_held(keyboard, 0x46));
        }
        assert_eq!(engine.packets.len(), 2);
        runtime
            .packet(Packet::button(mice[0], 0, true), &mut engine, true)
            .unwrap();
        assert!(engine.keys.is_empty());
        assert!(runtime.claims.0.is_empty() && runtime.claims.1.is_empty());
        assert_eq!(engine.packets.len(), 4);
        assert!(
            engine
                .packets
                .iter()
                .all(|packet| packet.kind == 1 && packet.data == 0x46)
        );
    }

    #[test]
    fn global_cancel_attempts_later_lanes_and_global_cleanup_after_failed_lane() {
        let (mut runtime, mut engine, _, keyboards) = two_user_runtime_and_engine();
        for (origin, keyboard, key) in [
            (Origin::Studio(0), keyboards[0], 0x41),
            (Origin::Studio(1), keyboards[1], 0x42),
            (Origin::Manual(1), keyboards[1], 0x43),
        ] {
            runtime
                .claims
                .input(origin, Packet::key(keyboard, key, false), &mut engine)
                .unwrap();
        }
        engine.fail_key_releases.insert((keyboards[0], 0x41));
        engine.fail_release_all = true;
        assert!(runtime.cancel(&mut engine).is_err());
        assert!(engine.key_held(keyboards[0], 0x41));
        assert!(!engine.key_held(keyboards[1], 0x42));
        assert!(engine.key_held(keyboards[1], 0x43));
        assert_eq!(engine.release_all_attempts, 1);
        engine.fail_key_releases.clear();
        engine.fail_release_all = false;
        runtime.cancel(&mut engine).unwrap();
        assert!(engine.keys.is_empty());
        assert_eq!(engine.release_all_attempts, 2);
        assert_eq!(
            engine
                .key_release_attempts
                .iter()
                .filter(|key| **key == (keyboards[1], 0x42))
                .count(),
            1
        );
    }

    #[test]
    fn agent_cleanup_sweeps_all_owners_and_users_and_retries_only_failed_releases() {
        use crate::routing_engine::ProgramOrigin;

        let (mut runtime, mut engine, _, keyboards) = two_user_runtime_and_engine();
        let claims = [
            (Origin::Agent(7, 0), keyboards[0], 0x41),
            (
                Origin::Replica(ProgramOrigin::Agent(7, 0), 0),
                keyboards[0],
                0x42,
            ),
            (Origin::Agent(7, 0), keyboards[0], 0x43),
            (Origin::Agent(7, 1), keyboards[1], 0x51),
            (Origin::Agent(7, 1), keyboards[1], 0x52),
            (Origin::Agent(8, 1), keyboards[1], 0x52),
            (Origin::Manual(1), keyboards[1], 0x52),
            (Origin::Studio(1), keyboards[1], 0x52),
            (
                Origin::Replica(ProgramOrigin::Manual(1), 1),
                keyboards[1],
                0x52,
            ),
        ];
        for (origin, device, key) in claims {
            let down = Packet::key(device, key, false);
            runtime
                .claims
                .input_output(origin, down, Some(down), &[], &mut engine)
                .unwrap();
        }
        runtime
            .claims
            .input_output(
                Origin::Agent(7, 1),
                Packet::key(keyboards[1], 0x53, false),
                None,
                &[],
                &mut engine,
            )
            .unwrap();
        assert_eq!(runtime.agent_pending_owners(7), 6);
        engine
            .fail_key_releases
            .extend([(keyboards[0], 0x41), (keyboards[0], 0x42)]);

        let error = runtime.cancel_agent(7, &mut engine).unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("2 input owner release(s) failed")
        );
        assert_eq!(runtime.agent_pending_owners(7), 2);
        assert_eq!(runtime.agent_pending_owners(8), 1);
        assert!(!engine.key_held(keyboards[0], 0x43));
        assert!(!engine.key_held(keyboards[1], 0x51));
        assert!(engine.key_held(keyboards[1], 0x52));
        for key in [0x41, 0x42, 0x43] {
            assert_eq!(
                engine
                    .key_release_attempts
                    .iter()
                    .filter(|&&attempt| attempt == (keyboards[0], key))
                    .count(),
                1
            );
        }
        assert!(!engine.key_release_attempts.contains(&(keyboards[1], 0x52)));

        engine.fail_key_releases.clear();
        runtime.cancel_agent(7, &mut engine).unwrap();
        assert_eq!(runtime.agent_pending_owners(7), 0);
        for key in [0x41, 0x42] {
            assert!(!engine.key_held(keyboards[0], key));
            assert_eq!(
                engine
                    .key_release_attempts
                    .iter()
                    .filter(|&&attempt| attempt == (keyboards[0], key))
                    .count(),
                2
            );
            assert_eq!(
                engine
                    .packets
                    .iter()
                    .filter(|packet| packet.device == keyboards[0]
                        && packet.data == key
                        && packet.flags & 1 != 0)
                    .count(),
                1
            );
        }
        let attempts = engine.key_release_attempts.len();
        runtime.cancel_agent(7, &mut engine).unwrap();
        assert_eq!(engine.key_release_attempts.len(), attempts);
        // The other client's shared claim and all local origins remain unchanged.
        assert_eq!(runtime.claims.1.len(), 4);
        runtime.cancel_agent(8, &mut engine).unwrap();
        assert!(engine.key_held(keyboards[1], 0x52));
        assert_eq!(runtime.claims.1.len(), 3);
        assert_eq!(engine.key_release_attempts.len(), attempts);
    }

    #[test]
    fn acknowledged_but_held_release_retains_owner_while_other_users_cleanup_and_retry_once() {
        let (mut runtime, mut engine, _, keyboards) = two_user_runtime_and_engine();
        let studio_owner = AgentStudioOwner {
            client: 7,
            user: 0,
            hwnd: 10,
            pid: 101,
        };
        runtime.lanes[0].agent_owner = Some(studio_owner);
        let held = Packet::key(keyboards[0], 117, false);
        let healthy = Packet::key(keyboards[1], 119, false);
        let shared = Packet::key(keyboards[1], 118, false);
        for (origin, down) in [
            (Origin::AgentStudio(studio_owner), held),
            (Origin::Agent(7, 1), healthy),
            (Origin::Agent(7, 1), shared),
            (Origin::Agent(8, 1), shared),
        ] {
            runtime
                .claims
                .input_output(origin, down, Some(down), &[], &mut engine)
                .unwrap();
        }
        engine.retained_key_releases.insert((keyboards[0], 117));

        let error = runtime.cancel_agent(7, &mut engine).unwrap_err();
        assert!(format!("{error:#}").contains("Input release remains pending"));
        assert!(engine.key_held(keyboards[0], 117));
        assert!(!engine.key_held(keyboards[1], 119));
        assert!(engine.key_held(keyboards[1], 118));
        assert_eq!(runtime.agent_pending_owners(7), 1);
        assert_eq!(runtime.agent_pending_owners(8), 1);
        assert_eq!(runtime.lanes[0].agent_owner, Some(studio_owner));
        assert_eq!(engine.key_release_attempts.len(), 2);
        assert!(engine.key_release_attempts.contains(&(keyboards[0], 117)));
        assert!(engine.key_release_attempts.contains(&(keyboards[1], 119)));

        engine.retained_key_releases.clear();
        runtime.cancel_agent(7, &mut engine).unwrap();
        assert!(!engine.key_held(keyboards[0], 117));
        assert!(engine.key_held(keyboards[1], 118));
        assert_eq!(runtime.agent_pending_owners(7), 0);
        assert!(runtime.lanes[0].agent_owner.is_none());
        assert_eq!(engine.key_release_attempts.len(), 3);
        assert_eq!(
            engine.key_release_attempts.last(),
            Some(&(keyboards[0], 117))
        );
        assert_eq!(
            engine
                .packets
                .iter()
                .filter(|packet| packet.device == keyboards[0]
                    && packet.data == 117
                    && packet.flags & 1 != 0)
                .count(),
            1
        );
        let attempts = engine.key_release_attempts.len();
        runtime.cancel_agent(7, &mut engine).unwrap();
        assert_eq!(engine.key_release_attempts.len(), attempts);
        runtime.cancel_agent(8, &mut engine).unwrap();
        assert!(!engine.key_held(keyboards[1], 118));
        assert_eq!(
            engine.key_release_attempts.last(),
            Some(&(keyboards[1], 118))
        );
    }

    #[test]
    fn failed_agent_cleanup_stops_all_owned_work_and_retains_studio_identity_until_retry() {
        use crate::routing_engine::ProgramOrigin;

        let (mut runtime, mut engine, mice, keyboards) = two_user_runtime_and_engine();
        let mut access = (*scoped_access(mice[0], keyboards[0])).clone();
        access.session_control = true;
        access.active_users.insert(1);
        access.users.insert(
            1,
            AgentGrant {
                mouse: mice[1],
                keyboard: Some(keyboards[1]),
            },
        );
        runtime.agent_access = Arc::new(access);
        engine.window = Some(Window { hwnd: 10, pid: 101 });
        runtime
            .command(
                automation_runtime::Command::Graph {
                    edges: vec![automation_runtime::Edge {
                        source: 0,
                        target: 1,
                        mouse: true,
                        keyboard: false,
                        text: false,
                        options: mux_core::replication::EdgeOptions {
                            click_delay_ms: 60_000,
                            ..Default::default()
                        },
                    }],
                    expected_revision: 0,
                },
                &mut engine,
            )
            .unwrap();
        start_connected(&mut runtime, &mut engine, keyboards[0]);
        let studio_owner = runtime.lanes[0].agent_owner.unwrap();
        runtime
            .agent_studio_command(
                7,
                Command::Record {
                    user: 1,
                    active: true,
                },
                &mut engine,
            )
            .unwrap();
        for client in [7, 8] {
            runtime
                .agent_action(
                    client,
                    Action::Packet {
                        user: 0,
                        packet: Packet::button(mice[0], 0, false),
                    },
                    &mut engine,
                )
                .unwrap();
            assert!(runtime.inner.has_pending(ProgramOrigin::Agent(client, 0)));
        }
        runtime
            .agent_action(
                7,
                Action::Packet {
                    user: 1,
                    packet: Packet::key(keyboards[1], 0x44, false),
                },
                &mut engine,
            )
            .unwrap();
        assert!(runtime.lanes[1].agent_recording.is_some());
        engine.fail_key_releases.insert((keyboards[0], 0x41));
        // Revoking the grant cannot prevent cleanup of already saved outputs.
        runtime.agent_access = Arc::new(AgentAccess::default());

        runtime.cancel_agent(7, &mut engine).unwrap_err();
        assert_eq!(runtime.lanes[0].agent_owner, Some(studio_owner));
        assert_eq!(
            runtime.lanes[0].playback.as_ref().unwrap().snapshot().state,
            PlaybackState::Cancelled
        );
        assert!(runtime.lanes[0].power.is_none());
        assert!(runtime.lanes[1].agent_recording.is_none());
        assert!(!engine.key_held(keyboards[1], 0x44));
        assert!(!runtime.inner.has_pending(ProgramOrigin::Agent(7, 0)));
        assert!(runtime.inner.has_pending(ProgramOrigin::Agent(8, 0)));
        assert_eq!(runtime.agent_pending_owners(7), 1);
        assert_eq!(
            engine
                .key_release_attempts
                .iter()
                .filter(|&&key| key == (keyboards[0], 0x41))
                .count(),
            1
        );

        engine.fail_key_releases.clear();
        runtime.cancel_agent(7, &mut engine).unwrap();
        assert!(runtime.lanes[0].agent_owner.is_none());
        assert!(runtime.lanes[0].clip.is_some());
        assert_eq!(runtime.agent_pending_owners(7), 0);
        assert_eq!(
            engine
                .key_release_attempts
                .iter()
                .filter(|&&key| key == (keyboards[0], 0x41))
                .count(),
            2
        );
        assert!(runtime.inner.has_pending(ProgramOrigin::Agent(8, 0)));
    }

    #[test]
    fn idle_guard_ignores_an_unrelated_user_but_rejects_a_disabled_source_claim_read_only() {
        let (mut runtime, mut engine, mice, _) = two_user_runtime_and_engine();
        let unrelated = Packet::button(mice[1], 0, false);
        runtime
            .claims
            .input_output(
                Origin::Manual(1),
                unrelated,
                Some(unrelated),
                &[],
                &mut engine,
            )
            .unwrap();
        runtime.require_idle_user_input(0).unwrap();

        let disabled_source = Packet::button(mice[0], 3, false);
        runtime
            .claims
            .input_output(Origin::Manual(0), disabled_source, None, &[], &mut engine)
            .unwrap();
        let packet_count = engine.packets.len();
        let pair_count = runtime.claims.1.len();
        let error = runtime.require_idle_user_input(0).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Release held input before changing annotation settings"
        );
        assert_eq!(engine.packets.len(), packet_count);
        assert_eq!(runtime.claims.1.len(), pair_count);
        assert!(runtime.claims.1.keys().any(|owner| owner.source
            == Token {
                device: mice[0],
                kind: 0,
                code: 3,
            }));
    }

    #[test]
    fn idle_guard_detects_shared_keyboard_output_without_releasing_any_owner() {
        let (mut runtime, mut engine, _, keyboards) = two_user_runtime_and_engine();
        let source = Packet::key(keyboards[1], 0x41, false);
        let output = Packet::key(keyboards[0], 0x42, false);
        runtime
            .claims
            .input_output(Origin::Manual(1), source, Some(output), &[], &mut engine)
            .unwrap();
        runtime
            .claims
            .input_output(Origin::Studio(1), source, Some(output), &[], &mut engine)
            .unwrap();
        let packets = engine.packets.clone();
        let owners = runtime.claims.0.values().next().unwrap().owners.clone();

        assert!(runtime.require_idle_user_input(0).is_err());
        assert_eq!(
            serde_json::to_value(&engine.packets).unwrap(),
            serde_json::to_value(&packets).unwrap()
        );
        assert!(runtime.claims.0.values().next().unwrap().owners == owners);
        assert_eq!(runtime.claims.1.len(), 2);
    }

    #[test]
    fn idle_guard_detects_replica_path_through_selected_user_without_mutation() {
        use crate::routing_engine::ProgramOrigin;

        let (mut runtime, mut engine, mice, _) = two_user_runtime_and_engine();
        let packet = Packet::button(mice[1], 0, false);
        runtime
            .claims
            .input_output(
                Origin::Replica(ProgramOrigin::Manual(1), 1),
                packet,
                Some(packet),
                &[0],
                &mut engine,
            )
            .unwrap();
        let packets = engine.packets.clone();
        let paths = runtime
            .claims
            .1
            .values()
            .map(|pair| pair.path.clone())
            .collect::<Vec<_>>();

        assert!(runtime.require_idle_user_input(0).is_err());
        assert_eq!(
            serde_json::to_value(&engine.packets).unwrap(),
            serde_json::to_value(&packets).unwrap()
        );
        assert_eq!(
            runtime
                .claims
                .1
                .values()
                .map(|pair| pair.path.clone())
                .collect::<Vec<_>>(),
            paths
        );
    }

    fn key_clip(user: usize, keyboard: u32) -> Clip {
        Clip::new(vec![
            mux_core::automation::TimedAction {
                at_micros: 0,
                action: Action::Packet {
                    user,
                    packet: Packet::key(keyboard, 0x41, false),
                },
            },
            mux_core::automation::TimedAction {
                at_micros: 100_000_000,
                action: Action::Packet {
                    user,
                    packet: Packet::key(keyboard, 0x41, true),
                },
            },
        ])
        .unwrap()
    }

    #[test]
    fn retiring_path_user_releases_derived_holds_but_preserves_destination_manual_owner() {
        let (_, _, mouse, keyboard) = runtime_and_engine();
        let removed = ResolvedSeat {
            name: "clone".into(),
            mouse: mouse + 20,
            keyboard: Some(keyboard + 20),
            target_pid: 0,
        };
        let mut engine = MockEngine::default();
        let mut claims = Claims::default();
        let first = Packet::key(keyboard, 0x41, false);
        let second = Packet::key(keyboard, 0x42, false);
        claims.input(Origin::Manual(2), first, &mut engine).unwrap();
        for packet in [first, second] {
            claims
                .input_via(
                    Origin::Replica(crate::routing_engine::ProgramOrigin::Studio(0), 0),
                    packet,
                    &[0, 1, 2],
                    &mut engine,
                )
                .unwrap();
        }
        claims
            .release_retiring_user(1, &removed, &mut engine)
            .unwrap();
        assert!(engine.key_held(keyboard, 0x41));
        assert!(!engine.key_held(keyboard, 0x42));
        assert_eq!(
            engine
                .packets
                .iter()
                .filter(|p| p.kind == 1 && p.flags & 1 != 0)
                .count(),
            1
        );
        claims.release(Origin::Manual(2), &mut engine).unwrap();
        assert!(!engine.key_held(keyboard, 0x41));
    }

    #[test]
    fn releasing_studio_claim_preserves_composite_key_and_manual_button_owners() {
        let (_, _, mouse, keyboard) = runtime_and_engine();
        let (mut claims, mut engine) = (Claims::default(), MockEngine::default());

        let key_down = Packet::key(keyboard, 0x41, false);
        claims
            .input(Origin::Composite, key_down, &mut engine)
            .unwrap();
        claims
            .input(Origin::Studio(0), key_down, &mut engine)
            .unwrap();
        claims.release(Origin::Studio(0), &mut engine).unwrap();
        assert!(engine.key_held(keyboard, 0x41));
        assert_eq!(
            engine.packets.len(),
            1,
            "the remaining owner keeps the key down"
        );
        claims.release(Origin::Composite, &mut engine).unwrap();
        assert!(!engine.key_held(keyboard, 0x41));

        let button_down = Packet::button(mouse, 0, false);
        claims
            .input(Origin::Manual(0), button_down, &mut engine)
            .unwrap();
        claims
            .input(Origin::Studio(0), button_down, &mut engine)
            .unwrap();
        claims.release(Origin::Studio(0), &mut engine).unwrap();
        assert!(engine.button_held(mouse, 0));
        claims.release(Origin::Manual(0), &mut engine).unwrap();
        assert!(!engine.button_held(mouse, 0));

        let key_releases = engine
            .packets
            .iter()
            .filter(|packet| packet.kind == 1 && packet.flags & 1 != 0)
            .count();
        let button_releases = engine
            .packets
            .iter()
            .filter(|packet| packet.kind == 0 && packet.flags == 2)
            .count();
        assert_eq!(key_releases, 1);
        assert_eq!(button_releases, 1);
    }

    #[test]
    fn studio_cancel_preserves_manual_key_hold_until_manual_release() {
        let (mut runtime, mut engine, _, keyboard) = runtime_and_engine();
        runtime
            .studio_command(
                Command::Load {
                    user: 0,
                    clip: key_clip(0, keyboard),
                },
                &mut engine,
            )
            .unwrap();
        runtime
            .studio_command(
                Command::Play {
                    user: 0,
                    options: PlaybackOptions {
                        loops: None,
                        ..PlaybackOptions::default()
                    },
                },
                &mut engine,
            )
            .unwrap();
        runtime.tick(&mut engine).unwrap();
        assert!(engine.key_held(keyboard, 0x41));

        runtime
            .action(
                Action::Packet {
                    user: 0,
                    packet: Packet::key(keyboard, 0x41, false),
                },
                &mut engine,
                false,
            )
            .unwrap();
        runtime
            .studio_command(Command::Cancel { user: 0 }, &mut engine)
            .unwrap();
        assert!(engine.key_held(keyboard, 0x41));
        assert_eq!(
            engine
                .packets
                .iter()
                .filter(|packet| packet.kind == 1 && packet.flags & 1 != 0)
                .count(),
            0
        );

        runtime
            .action(
                Action::Packet {
                    user: 0,
                    packet: Packet::key(keyboard, 0x41, true),
                },
                &mut engine,
                false,
            )
            .unwrap();
        assert!(!engine.key_held(keyboard, 0x41));
        assert_eq!(
            engine
                .packets
                .iter()
                .filter(|packet| packet.kind == 1 && packet.flags & 1 != 0)
                .count(),
            1
        );
    }

    #[test]
    fn rejected_independent_and_composite_mutations_keep_active_claims() {
        let (mut runtime, mut engine, _, keyboard) = runtime_and_engine();
        runtime
            .studio_command(
                Command::Load {
                    user: 0,
                    clip: key_clip(0, keyboard),
                },
                &mut engine,
            )
            .unwrap();
        runtime
            .studio_command(
                Command::Play {
                    user: 0,
                    options: PlaybackOptions {
                        loops: None,
                        ..PlaybackOptions::default()
                    },
                },
                &mut engine,
            )
            .unwrap();
        runtime.tick(&mut engine).unwrap();
        assert!(engine.key_held(keyboard, 0x41));

        let invalid_play = runtime.studio_command(
            Command::Play {
                user: 0,
                options: PlaybackOptions {
                    speed: f64::NAN,
                    ..PlaybackOptions::default()
                },
            },
            &mut engine,
        );
        assert!(invalid_play.is_err());
        assert_eq!(
            runtime.studio_state(None).users[0]
                .playback
                .as_ref()
                .unwrap()
                .state,
            "playing"
        );

        let rejected_composite = runtime.command(
            automation_runtime::Command::Load {
                clip: key_clip(0, keyboard),
            },
            &mut engine,
        );
        assert!(rejected_composite.is_err());
        assert!(engine.key_held(keyboard, 0x41));
        assert_eq!(
            runtime.studio_state(None).users[0]
                .playback
                .as_ref()
                .unwrap()
                .state,
            "playing"
        );
    }

    #[test]
    fn mapper_capture_permissions_are_separate_from_routing_and_bind_read_user_client_and_generation()
     {
        let (mut runtime, mut engine, mice, keyboards) = two_user_runtime_and_engine();
        runtime.mapper_virtual_users = vec![true, true];
        runtime.mapper_capture_pids = BTreeSet::from([101]);
        runtime.agent_access = Arc::new(AgentAccess {
            enabled: true,
            allowed_pids: BTreeSet::from([101]),
            users: (0..2)
                .map(|user| {
                    (
                        user,
                        AgentGrant {
                            mouse: mice[user],
                            keyboard: Some(keyboards[user]),
                        },
                    )
                })
                .collect(),
            session_control: true,
            capture: false,
            generation: 7,
            clients: BTreeSet::from([9]),
            active_users: BTreeSet::from([0, 1]),
        });
        engine.window = Some(Window { hwnd: 20, pid: 101 });
        let origin = crate::routing_engine::ProgramOrigin::Agent(9, 0);
        assert!(
            runtime
                .with_mapper(&mut engine, false, |_, backend| backend.capture_authority(
                    origin,
                    0,
                    1,
                    &[0, 1]
                ))
                .is_err(),
            "routing grants never imply capture permission"
        );
        Arc::make_mut(&mut runtime.agent_access).capture = true;
        let receipt = runtime
            .with_mapper(&mut engine, false, |_, backend| {
                backend.capture_authority(origin, 0, 1, &[0, 1])
            })
            .unwrap();
        assert_eq!(receipt.generation, 7);
        assert_eq!(receipt.window, Window { hwnd: 20, pid: 101 });
        Arc::make_mut(&mut runtime.agent_access).users.remove(&1);
        assert!(
            runtime
                .with_mapper(&mut engine, false, |_, backend| backend.capture_authority(
                    origin,
                    0,
                    1,
                    &[0, 1]
                ))
                .is_err(),
            "cross-user read needs its own user grant"
        );
        Arc::make_mut(&mut runtime.agent_access).users.insert(
            1,
            AgentGrant {
                mouse: mice[1],
                keyboard: Some(keyboards[1]),
            },
        );
        runtime.mapper_agent_disconnected(9);
        assert!(
            runtime
                .with_mapper(&mut engine, false, |_, backend| backend.capture_authority(
                    origin,
                    0,
                    1,
                    &[0, 1]
                ))
                .is_err()
        );
        runtime.mapper_agent_connected(9);
        Arc::make_mut(&mut runtime.agent_access).capture = false;
        assert!(
            runtime
                .with_mapper(&mut engine, false, |_, backend| backend.capture_authority(
                    crate::routing_engine::ProgramOrigin::Manual(0),
                    0,
                    1,
                    &[0, 1]
                ))
                .is_ok(),
            "manual GUI scope does not borrow an agent capture grant"
        );
        runtime.mapper_capture_pids.clear();
        assert!(
            runtime
                .with_mapper(&mut engine, false, |_, backend| backend.capture_authority(
                    crate::routing_engine::ProgramOrigin::Manual(0),
                    0,
                    1,
                    &[0, 1]
                ))
                .is_err(),
            "manual capture still needs an explicit session PID scope"
        );
        assert!(
            engine.packets.is_empty(),
            "authorization must never emit input"
        );
    }

    fn scoped_access(mouse: u32, keyboard: u32) -> Arc<AgentAccess> {
        Arc::new(AgentAccess {
            enabled: true,
            allowed_pids: BTreeSet::from([101]),
            users: BTreeMap::from([(
                0,
                AgentGrant {
                    mouse,
                    keyboard: Some(keyboard),
                },
            )]),
            session_control: false,
            active_users: BTreeSet::from([0]),
            ..Default::default()
        })
    }

    #[test]
    fn forwarding_denies_ungranted_sink_before_claim_or_anchor_mutation() {
        let (mut runtime, mut engine, mice, keyboards) = two_user_runtime_and_engine();
        runtime
            .command(
                automation_runtime::Command::Forwarding {
                    edges: vec![mux_core::forwarding::ResolvedEdge {
                        source: 0,
                        target: 1,
                        keyboard: true,
                    }],
                    expected_revision: 0,
                },
                &mut engine,
            )
            .unwrap();
        runtime.agent_access = Arc::new(AgentAccess {
            enabled: true,
            allowed_pids: BTreeSet::from([101]),
            users: BTreeMap::from([(
                0,
                AgentGrant {
                    mouse: mice[0],
                    keyboard: Some(keyboards[0]),
                },
            )]),
            session_control: false,
            active_users: BTreeSet::from([0, 1]),
            ..Default::default()
        });
        engine.window = Some(Window { hwnd: 10, pid: 101 });
        for action in [
            Action::Packet {
                user: 0,
                packet: Packet::button(mice[0], 0, false),
            },
            Action::Packet {
                user: 0,
                packet: Packet::key(keyboards[0], 0x41, false),
            },
            Action::MoveTo {
                user: 0,
                x: 20,
                y: 30,
            },
            Action::Text {
                user: 0,
                text: "A".into(),
            },
        ] {
            assert!(runtime.agent_action(7, action, &mut engine).is_err());
        }
        assert!(engine.packets.is_empty());
        assert!(runtime.claims.0.is_empty() && runtime.claims.1.is_empty());
        assert_eq!(runtime.inner.forwarding_anchor(0).unwrap(), (0, 0));
    }

    #[test]
    fn disabled_forwarded_output_still_requires_sink_pid_before_source_claim() {
        let (mut runtime, mut engine, mice, keyboards) = two_user_runtime_and_engine();
        runtime
            .command(
                automation_runtime::Command::Forwarding {
                    edges: vec![mux_core::forwarding::ResolvedEdge {
                        source: 0,
                        target: 1,
                        keyboard: false,
                    }],
                    expected_revision: 0,
                },
                &mut engine,
            )
            .unwrap();
        let mut settings = mux_core::output::Settings::default();
        settings.buttons[0] = mux_core::output::MouseOutput::Disabled;
        runtime.outputs.set(1, settings).unwrap();
        runtime.agent_access = Arc::new(AgentAccess {
            enabled: true,
            allowed_pids: BTreeSet::from([101]),
            users: BTreeMap::from([
                (
                    0,
                    AgentGrant {
                        mouse: mice[0],
                        keyboard: Some(keyboards[0]),
                    },
                ),
                (
                    1,
                    AgentGrant {
                        mouse: mice[1],
                        keyboard: Some(keyboards[1]),
                    },
                ),
            ]),
            session_control: false,
            active_users: BTreeSet::from([0, 1]),
            ..Default::default()
        });
        engine.window = Some(Window { hwnd: 20, pid: 202 });
        assert!(
            runtime
                .agent_action(
                    7,
                    Action::Packet {
                        user: 0,
                        packet: Packet::button(mice[0], 0, false),
                    },
                    &mut engine,
                )
                .is_err()
        );
        assert!(engine.packets.is_empty());
        assert!(runtime.claims.0.is_empty() && runtime.claims.1.is_empty());
    }

    #[test]
    fn agent_actions_require_both_actual_recipient_and_pending_attachment_grants() {
        for kind in 0..5 {
            let (mut runtime, mut engine, mouse, keyboard) = runtime_and_engine();
            let actual = Window { hwnd: 10, pid: 101 };
            let pending = Window { hwnd: 20, pid: 202 };
            engine.window = Some(actual);
            engine.pending_window = Some(pending);
            runtime.agent_access = scoped_access(mouse, keyboard);
            let action = match kind {
                0 => Action::Packet {
                    user: 0,
                    packet: Packet::motion(mouse, 4, 5),
                },
                1 => Action::Packet {
                    user: 0,
                    packet: Packet::button(mouse, 0, false),
                },
                2 => Action::Packet {
                    user: 0,
                    packet: Packet::key(keyboard, 0x41, false),
                },
                3 => Action::Text {
                    user: 0,
                    text: "A".into(),
                },
                _ => Action::MoveTo {
                    user: 0,
                    x: 4,
                    y: 5,
                },
            };
            let before = serde_json::to_value(runtime.state(false)).unwrap();
            let error = runtime
                .agent_action(7, action.clone(), &mut engine)
                .unwrap_err();
            assert!(error.downcast_ref::<AgentScopeViolation>().is_some());
            assert!(engine.packets.is_empty());
            assert!(engine.moves.is_empty());
            assert!(engine.mouse_activity.is_empty());
            assert!(runtime.claims.0.is_empty() && runtime.claims.1.is_empty());
            assert_eq!(serde_json::to_value(runtime.state(false)).unwrap(), before);
            Arc::make_mut(&mut runtime.agent_access)
                .allowed_pids
                .insert(pending.pid);
            // Granting the attach target still cannot authorize a third recipient.
            engine.window = Some(Window { hwnd: 30, pid: 303 });
            assert!(
                runtime
                    .agent_action(7, action.clone(), &mut engine)
                    .is_err()
            );
            assert!(engine.packets.is_empty() && engine.moves.is_empty());
            engine.window = Some(actual);
            runtime.agent_action(7, action, &mut engine).unwrap();
            assert!(!engine.packets.is_empty() || !engine.moves.is_empty());
        }
    }

    #[test]
    fn replica_delivery_rechecks_pending_attachment_without_creating_claims() {
        use crate::routing_engine::ProgramOrigin;
        let (runtime, mut engine, mouse, keyboard) = runtime_and_engine();
        engine.window = Some(Window { hwnd: 10, pid: 101 });
        engine.pending_window = Some(Window { hwnd: 20, pid: 202 });
        let mut claims = Claims::default();
        let mut guarded = ClaimEngine {
            inner: &mut engine,
            claims: &mut claims,
            origin: Origin::Composite,
            agent_access: scoped_access(mouse, keyboard),
            outputs: &runtime.outputs,
        };
        assert_eq!(
            guarded.pending_attachment_window(0).unwrap().unwrap().pid,
            202
        );
        let origin = ProgramOrigin::Agent(7, 0);
        let down = Packet::key(keyboard, 0x41, false);
        assert!(guarded.input_replica(origin, 0, &[0], down).is_err());
        assert!(guarded.move_replica(origin, 0, &[0], 0, 4, 5).is_err());
        assert!(guarded.claims.0.is_empty() && guarded.claims.1.is_empty());
        Arc::make_mut(&mut guarded.agent_access)
            .allowed_pids
            .insert(202);
        guarded.input_replica(origin, 0, &[0], down).unwrap();
        guarded.move_replica(origin, 0, &[0], 0, 4, 5).unwrap();
        assert_eq!(engine.packets.len(), 1);
        assert_eq!(engine.moves, vec![(0, 4, 5)]);
    }

    #[test]
    fn agent_studio_pending_attachment_must_remain_in_its_pinned_application() {
        let (runtime, mut engine, mouse, keyboard) = runtime_and_engine();
        let selected = Window { hwnd: 10, pid: 101 };
        engine.window = Some(selected);
        engine.pending_window = Some(Window { hwnd: 20, pid: 202 });
        let owner = AgentStudioOwner {
            client: 7,
            user: 0,
            hwnd: selected.hwnd,
            pid: selected.pid,
        };
        let mut claims = Claims::default();
        let mut access = scoped_access(mouse, keyboard);
        Arc::make_mut(&mut access).allowed_pids.insert(202);
        Arc::make_mut(&mut access).session_control = true;
        {
            let mut guarded = ClaimEngine {
                inner: &mut engine,
                claims: &mut claims,
                origin: Origin::AgentStudio(owner),
                agent_access: access.clone(),
                outputs: &runtime.outputs,
            };
            assert!(guarded.input(Packet::key(keyboard, 0x41, false)).is_err());
            assert!(guarded.move_to(0, 4, 5).is_err());
        }
        assert!(engine.packets.is_empty() && engine.moves.is_empty());
        assert!(claims.0.is_empty() && claims.1.is_empty());
        engine.pending_window = Some(selected);
        let mut guarded = ClaimEngine {
            inner: &mut engine,
            claims: &mut claims,
            origin: Origin::AgentStudio(owner),
            agent_access: access,
            outputs: &runtime.outputs,
        };
        guarded.input(Packet::key(keyboard, 0x41, false)).unwrap();
        assert_eq!(engine.packets.len(), 1);
    }

    #[test]
    fn translated_clear_all_shortcut_requires_grant_for_every_active_user() {
        let (mut runtime, mut engine, mice, keyboards) = two_user_runtime_and_engine();
        engine.window = Some(Window { hwnd: 10, pid: 101 });
        let mut output_settings = mux_core::output::Settings::default();
        output_settings.buttons[0] = mux_core::output::MouseOutput::Key { key: 0x46 };
        runtime.outputs.set(0, output_settings).unwrap();
        engine.clear_all_keys.insert((0, keyboards[0], 0x46));

        runtime.agent_access = Arc::new(AgentAccess {
            enabled: true,
            allowed_pids: BTreeSet::from([101]),
            users: BTreeMap::from([(
                0,
                AgentGrant {
                    mouse: mice[0],
                    keyboard: Some(keyboards[0]),
                },
            )]),
            session_control: false,
            active_users: BTreeSet::from([0, 1]),
            ..Default::default()
        });

        let source_down = Packet::button(mice[0], 0, false);
        let check = |runtime: &Runtime, engine: &MockEngine, packet| {
            check_agent_packet(
                &runtime.agent_access,
                &runtime.outputs,
                engine,
                77,
                0,
                &[],
                0,
                packet,
            )
        };

        assert!(check(&runtime, &engine, source_down).is_err());
        Arc::make_mut(&mut runtime.agent_access).session_control = true;
        assert!(check(&runtime, &engine, source_down).is_err());

        // The release is always allowed, including while the all-users shortcut
        // is unauthorized, so the agent can relinquish a previously held key.
        assert!(check(&runtime, &engine, Packet::button(mice[0], 0, true)).is_ok());

        Arc::make_mut(&mut runtime.agent_access).users.insert(
            1,
            AgentGrant {
                mouse: mice[1],
                keyboard: Some(keyboards[1]),
            },
        );
        assert!(check(&runtime, &engine, source_down).is_ok());

        // A newly active user invalidates a previously complete grant until
        // that user's identity is explicitly added to the permission set.
        Arc::make_mut(&mut runtime.agent_access)
            .active_users
            .insert(2);
        assert!(check(&runtime, &engine, source_down).is_err());
        assert!(engine.packets.is_empty());
    }

    #[test]
    fn authorized_raw_mouse_activity_precedes_suppression_but_not_scope_validation() {
        let (mut runtime, mut engine, mouse, keyboard) = runtime_and_engine();
        let mut settings = mux_core::output::Settings::default();
        settings.buttons[3] = mux_core::output::MouseOutput::Disabled;
        runtime.outputs.set(0, settings).unwrap();
        let mut claims = Claims::default();
        {
            let mut guarded = ClaimEngine {
                inner: &mut engine,
                claims: &mut claims,
                origin: Origin::Manual(0),
                agent_access: Arc::new(AgentAccess::default()),
                outputs: &runtime.outputs,
            };
            guarded.input(Packet::button(mouse, 3, false)).unwrap();
            guarded.input(Packet::key(keyboard, 0x41, false)).unwrap();
            guarded
                .input_replica(
                    crate::routing_engine::ProgramOrigin::Manual(0),
                    0,
                    &[0],
                    Packet::motion(mouse, 1, 0),
                )
                .unwrap();
        }
        assert_eq!(engine.mouse_activity, [0, 0]);
        assert!(engine.packets.iter().all(|packet| packet.flags != 64));
        {
            let mut guarded = ClaimEngine {
                inner: &mut engine,
                claims: &mut claims,
                origin: Origin::Agent(17, 0),
                agent_access: Arc::new(AgentAccess::default()),
                outputs: &runtime.outputs,
            };
            assert!(guarded.input(Packet::motion(mouse, 1, 0)).is_err());
        }
        assert_eq!(engine.mouse_activity, [0, 0]);
    }

    #[test]
    fn cancelling_one_agent_preserves_other_agent_studio_and_manual_key_owners() {
        let (mut runtime, mut engine, _, keyboard) = runtime_and_engine();
        let down = Packet::key(keyboard, 0x41, false);
        for origin in [
            Origin::Manual(0),
            Origin::Studio(0),
            Origin::Agent(51, 0),
            Origin::Agent(52, 0),
        ] {
            runtime.claims.input(origin, down, &mut engine).unwrap();
        }
        runtime
            .claims
            .input(
                Origin::Replica(crate::routing_engine::ProgramOrigin::Agent(51, 0), 0),
                down,
                &mut engine,
            )
            .unwrap();
        assert_eq!(engine.packets.len(), 1);

        runtime.cancel_agent(51, &mut engine).unwrap();
        assert!(engine.key_held(keyboard, 0x41));
        runtime.cancel_agent(52, &mut engine).unwrap();
        assert!(engine.key_held(keyboard, 0x41));

        runtime
            .claims
            .release(Origin::Studio(0), &mut engine)
            .unwrap();
        assert!(engine.key_held(keyboard, 0x41));
        runtime
            .claims
            .release(Origin::Manual(0), &mut engine)
            .unwrap();
        assert!(!engine.key_held(keyboard, 0x41));
        assert_eq!(
            engine
                .packets
                .iter()
                .filter(|packet| packet.kind == 1 && packet.flags & 1 != 0)
                .count(),
            1
        );
    }

    #[test]
    fn delayed_agent_replica_rechecks_target_pid_and_still_allows_its_release() {
        use crate::routing_engine::ProgramOrigin;

        let (runtime, mut engine, _, keyboard) = runtime_and_engine();
        engine.window = Some(Window { hwnd: 10, pid: 101 });
        let access = scoped_access(0, keyboard);
        let mut claims = Claims::default();
        let down = Packet::key(keyboard, 0x41, false);
        {
            let mut guarded = ClaimEngine {
                inner: &mut engine,
                claims: &mut claims,
                origin: Origin::Composite,
                agent_access: access.clone(),
                outputs: &runtime.outputs,
            };
            guarded
                .input_replica(ProgramOrigin::Agent(9, 0), 0, &[0], down)
                .unwrap();
        }
        assert!(engine.key_held(keyboard, 0x41));

        engine.window = Some(Window { hwnd: 20, pid: 202 });
        let other = Packet::key(keyboard, 0x42, false);
        let error = {
            let mut guarded = ClaimEngine {
                inner: &mut engine,
                claims: &mut claims,
                origin: Origin::Composite,
                agent_access: access,
                outputs: &runtime.outputs,
            };
            let error = guarded
                .input_replica(ProgramOrigin::Agent(9, 0), 0, &[0], other)
                .unwrap_err();
            // A scope change must not prevent the agent from releasing its own held key.
            guarded
                .input_replica(ProgramOrigin::Agent(9, 0), 0, &[0], released(down))
                .unwrap();
            error
        };
        assert!(error.downcast_ref::<AgentScopeViolation>().is_some());
        assert!(!engine.key_held(keyboard, 0x41));
        assert!(!engine.key_held(keyboard, 0x42));
        assert_eq!(engine.packets.len(), 2);
    }

    #[test]
    fn agent_can_position_an_unfocused_cursor_then_click_an_allowed_target() {
        let (runtime, mut engine, mouse, keyboard) = runtime_and_engine();
        let access = scoped_access(mouse, keyboard);
        let mut claims = Claims::default();
        {
            let mut guarded = ClaimEngine {
                inner: &mut engine,
                claims: &mut claims,
                origin: Origin::Agent(14, 0),
                agent_access: access.clone(),
                outputs: &runtime.outputs,
            };
            guarded.move_to(0, 40, 50).unwrap();
        }
        assert!(engine.packets.is_empty());

        engine.window = Some(Window { hwnd: 35, pid: 101 });
        let down = Packet::button(mouse, 0, false);
        {
            let mut guarded = ClaimEngine {
                inner: &mut engine,
                claims: &mut claims,
                origin: Origin::Agent(14, 0),
                agent_access: access,
                outputs: &runtime.outputs,
            };
            guarded.input(down).unwrap();
        }
        assert!(engine.button_held(mouse, 0));
        assert_eq!(engine.packets.len(), 1);
        let packet = engine.packets[0];
        assert_eq!(
            (packet.device, packet.kind, packet.flags, packet.data),
            (down.device, down.kind, down.flags, down.data)
        );
    }
    #[test]
    fn output_aliases_share_native_key_with_distinct_raw_source_owners() {
        use mux_core::output::{KeyOutput, MouseOutput, Settings};
        let (mut runtime, mut engine, mouse, keyboard) = runtime_and_engine();
        let mut settings = Settings::default();
        settings.buttons[0] = MouseOutput::Key { key: 116 };
        settings.buttons[3] = MouseOutput::Key { key: 116 };
        settings.keys.insert(65, KeyOutput::Key { key: 116 });
        runtime
            .output_command(
                crate::output_runtime::Command::Configure { user: 0, settings },
                &mut engine,
            )
            .unwrap();
        let sources = [
            mux_windows::keyboard::packet_code(keyboard, 116, false).unwrap(),
            Packet::button(mouse, 0, false),
            Packet::button(mouse, 3, false),
            Packet::key(keyboard, 65, false),
        ];
        for packet in sources {
            runtime.packet(packet, &mut engine, false).unwrap();
        }
        assert_eq!(engine.packets.len(), 1);
        for packet in sources.into_iter().skip(1) {
            runtime
                .packet(released(packet), &mut engine, false)
                .unwrap();
        }
        assert!(engine.key_held(keyboard, 116));
        assert_eq!(engine.packets.len(), 1);
        runtime
            .packet(released(sources[0]), &mut engine, false)
            .unwrap();
        assert!(!engine.key_held(keyboard, 116));
        assert_eq!(engine.packets.len(), 2);
        assert_eq!(engine.packets[1].data, 116);
        assert_eq!(engine.packets[1].flags & 1, 1);
    }
    #[test]
    fn output_rule_change_releases_changed_pairs_and_rejects_invalid_without_side_effects() {
        use mux_core::output::{MouseOutput, Settings};
        let (mut runtime, mut engine, mouse, keyboard) = runtime_and_engine();
        let mut settings = Settings::default();
        settings.buttons[3] = MouseOutput::Key { key: 116 };
        runtime
            .output_command(
                crate::output_runtime::Command::Configure {
                    user: 0,
                    settings: settings.clone(),
                },
                &mut engine,
            )
            .unwrap();
        runtime
            .packet(Packet::button(mouse, 3, false), &mut engine, false)
            .unwrap();
        runtime
            .packet(
                mux_windows::keyboard::packet_code(keyboard, 117, false).unwrap(),
                &mut engine,
                false,
            )
            .unwrap();
        let before = engine.packets.len();
        let mut invalid = settings.clone();
        invalid.buttons[3] = MouseOutput::Key { key: 0 };
        assert!(
            runtime
                .output_command(
                    crate::output_runtime::Command::Configure {
                        user: 0,
                        settings: invalid
                    },
                    &mut engine
                )
                .is_err()
        );
        assert_eq!(engine.packets.len(), before);
        assert!(engine.key_held(keyboard, 116) && engine.key_held(keyboard, 117));
        settings.buttons[3] = MouseOutput::Key { key: 118 };
        runtime
            .output_command(
                crate::output_runtime::Command::Configure { user: 0, settings },
                &mut engine,
            )
            .unwrap();
        assert!(!engine.key_held(keyboard, 116));
        assert!(engine.key_held(keyboard, 117));
        runtime
            .packet(Packet::button(mouse, 3, true), &mut engine, false)
            .unwrap();
        assert!(!engine.key_held(keyboard, 118));
        assert_eq!(engine.packets.len(), before + 1);
    }

    fn connected_studio() -> (Runtime, MockEngine, u32, u32) {
        let (mut runtime, mut engine, mouse, keyboard) = runtime_and_engine();
        let mut access = (*scoped_access(mouse, keyboard)).clone();
        access.session_control = true;
        access.allowed_pids.insert(202);
        runtime.agent_access = Arc::new(access);
        engine.window = Some(Window { hwnd: 10, pid: 101 });
        (runtime, engine, mouse, keyboard)
    }

    fn start_connected(runtime: &mut Runtime, engine: &mut MockEngine, keyboard: u32) {
        runtime
            .agent_studio_command(
                7,
                Command::Load {
                    user: 0,
                    clip: key_clip(0, keyboard),
                },
                engine,
            )
            .unwrap();
        runtime
            .agent_studio_command(
                7,
                Command::Play {
                    user: 0,
                    options: PlaybackOptions::default(),
                },
                engine,
            )
            .unwrap();
        runtime.tick(engine).unwrap();
    }

    #[test]
    fn connected_authoring_binds_devices_and_rejects_invalid_edits_atomically() {
        let (mut runtime, mut engine, _, keyboard) = connected_studio();
        let state = runtime
            .agent_studio_command(
                7,
                Command::Load {
                    user: 0,
                    clip: key_clip(0, keyboard + 2),
                },
                &mut engine,
            )
            .unwrap();
        let clip = state.users[0].clip.as_ref().unwrap();
        assert!(clip.events.iter().all(|event| matches!(&event.action,
            Action::Packet { packet, .. } if packet.device == keyboard)));
        let before = serde_json::to_value(runtime.studio_state(Some(0))).unwrap();
        let event = mux_core::automation::TimedAction {
            at_micros: 0,
            action: Action::Text {
                user: 1,
                text: "wrong lane".into(),
            },
        };
        assert!(
            runtime
                .agent_studio_command(
                    7,
                    Command::Edit {
                        user: 0,
                        edit: Edit::Insert { event },
                    },
                    &mut engine
                )
                .is_err()
        );
        assert!(
            runtime
                .agent_studio_command(
                    7,
                    Command::Record {
                        user: 0,
                        active: false,
                    },
                    &mut engine
                )
                .is_err()
        );
        assert_eq!(
            before,
            serde_json::to_value(runtime.studio_state(Some(0))).unwrap()
        );
        assert!(engine.packets.is_empty());
    }

    #[test]
    fn connected_play_cannot_steal_holds_and_reset_preserves_manual_input() {
        let (mut runtime, mut engine, _, keyboard) = connected_studio();
        runtime
            .agent_studio_command(
                7,
                Command::Load {
                    user: 0,
                    clip: key_clip(0, keyboard),
                },
                &mut engine,
            )
            .unwrap();
        let down = Packet::key(keyboard, 0x44, false);
        runtime
            .action(
                Action::Packet {
                    user: 0,
                    packet: down,
                },
                &mut engine,
                false,
            )
            .unwrap();
        assert!(
            runtime
                .agent_studio_command(
                    7,
                    Command::Play {
                        user: 0,
                        options: PlaybackOptions::default(),
                    },
                    &mut engine
                )
                .is_err()
        );
        runtime
            .agent_studio_command(7, Command::Reset { user: 0 }, &mut engine)
            .unwrap();
        assert!(engine.key_held(keyboard, 0x44));
        assert_eq!(
            serde_json::to_value(&engine.packets).unwrap(),
            serde_json::to_value(vec![down]).unwrap()
        );
        assert!(runtime.lanes[0].clip.is_none());
    }

    #[test]
    fn connected_lane_ownership_blocks_replacement_and_cancel_preserves_direct_claims() {
        let (mut runtime, mut engine, _, keyboard) = connected_studio();
        start_connected(&mut runtime, &mut engine, keyboard);
        let before = serde_json::to_value(runtime.studio_state(Some(0))).unwrap();
        for command in [
            Command::Cancel { user: 0 },
            Command::Reset { user: 0 },
            Command::Load {
                user: 0,
                clip: key_clip(0, keyboard),
            },
        ] {
            assert!(
                runtime
                    .agent_studio_command(8, command, &mut engine)
                    .is_err()
            );
        }
        assert!(
            runtime
                .agent_studio_command(7, Command::Reset { user: 0 }, &mut engine)
                .is_err()
        );
        assert_eq!(
            before,
            serde_json::to_value(runtime.studio_state(Some(0))).unwrap()
        );
        let action = Action::Packet {
            user: 0,
            packet: Packet::key(keyboard, 0x41, false),
        };
        runtime
            .agent_action(7, action.clone(), &mut engine)
            .unwrap();
        runtime.agent_action(8, action, &mut engine).unwrap();
        runtime
            .agent_studio_command(7, Command::Cancel { user: 0 }, &mut engine)
            .unwrap();
        assert!(engine.key_held(keyboard, 0x41));
        runtime.cancel_agent(7, &mut engine).unwrap();
        assert!(engine.key_held(keyboard, 0x41));
        runtime.cancel_agent(8, &mut engine).unwrap();
        assert!(!engine.key_held(keyboard, 0x41));
        assert_eq!(engine.packets.len(), 2);
    }

    #[test]
    fn connected_playback_pins_window_even_when_new_process_is_permitted() {
        let (mut runtime, mut engine, _, keyboard) = connected_studio();
        start_connected(&mut runtime, &mut engine, keyboard);
        assert!(engine.key_held(keyboard, 0x41));
        engine.window = Some(Window { hwnd: 20, pid: 202 });
        runtime.lanes[0].last_tick -= Duration::from_secs(101);
        let error = runtime.tick(&mut engine).unwrap_err();
        assert!(error.downcast_ref::<AgentScopeViolation>().is_some());
        assert!(!engine.key_held(keyboard, 0x41));
        assert_eq!(engine.packets.len(), 2);
        assert!(runtime.lanes[0].agent_owner.is_none());
        assert!(runtime.lanes[0].clip.is_some());
    }

    #[test]
    fn delayed_studio_replica_rechecks_scope_and_releases_only_its_origin() {
        let (mut runtime, mut engine, _, keyboard) = connected_studio();
        start_connected(&mut runtime, &mut engine, keyboard);
        let origin = runtime.studio_origin(0);
        let packet = Packet::key(keyboard, 0x44, false);
        let invoke = |runtime: &mut Runtime, engine: &mut MockEngine| {
            let mut guarded = ClaimEngine {
                inner: engine,
                claims: &mut runtime.claims,
                origin: Origin::Composite,
                agent_access: runtime.agent_access.clone(),
                outputs: &runtime.outputs,
            };
            guarded.input_replica(origin.program(), 0, &[0], packet)
        };
        invoke(&mut runtime, &mut engine).unwrap();
        runtime
            .agent_action(8, Action::Packet { user: 0, packet }, &mut engine)
            .unwrap();
        Arc::make_mut(&mut runtime.agent_access).session_control = false;
        assert!(invoke(&mut runtime, &mut engine).is_err());
        runtime.cancel_agent(7, &mut engine).unwrap();
        assert!(!engine.key_held(keyboard, 0x41));
        assert!(engine.key_held(keyboard, 0x44));
        runtime.cancel_agent(8, &mut engine).unwrap();
        assert!(!engine.key_held(keyboard, 0x44));
    }

    #[test]
    fn connected_mutations_preserve_a_gui_owned_playback() {
        let (mut runtime, mut engine, _, keyboard) = connected_studio();
        runtime
            .studio_command(
                Command::Load {
                    user: 0,
                    clip: key_clip(0, keyboard),
                },
                &mut engine,
            )
            .unwrap();
        runtime
            .studio_command(
                Command::Play {
                    user: 0,
                    options: PlaybackOptions::default(),
                },
                &mut engine,
            )
            .unwrap();
        runtime.tick(&mut engine).unwrap();
        for command in [
            Command::Cancel { user: 0 },
            Command::Reset { user: 0 },
            Command::Play {
                user: 0,
                options: PlaybackOptions::default(),
            },
        ] {
            assert!(
                runtime
                    .agent_studio_command(7, command, &mut engine)
                    .is_err()
            );
        }
        runtime.cancel_agent(7, &mut engine).unwrap();
        assert!(engine.key_held(keyboard, 0x41));
        assert_eq!(
            runtime.studio_state(None).users[0]
                .playback
                .as_ref()
                .unwrap()
                .state,
            "playing"
        );
        runtime
            .studio_command(Command::Cancel { user: 0 }, &mut engine)
            .unwrap();
        assert!(!engine.key_held(keyboard, 0x41));
    }

    #[test]
    fn connected_play_preflights_composite_and_completion_releases_its_owner() {
        let (mut runtime, mut engine, _, keyboard) = connected_studio();
        runtime
            .agent_studio_command(
                7,
                Command::Load {
                    user: 0,
                    clip: key_clip(0, keyboard),
                },
                &mut engine,
            )
            .unwrap();
        runtime
            .command(automation_runtime::Command::RecordStart, &mut engine)
            .unwrap();
        let before = serde_json::to_value(runtime.studio_state(Some(0))).unwrap();
        assert!(
            runtime
                .agent_studio_command(
                    7,
                    Command::Play {
                        user: 0,
                        options: PlaybackOptions::default(),
                    },
                    &mut engine
                )
                .is_err()
        );
        assert!(runtime.state(false).recording);
        assert_eq!(
            before,
            serde_json::to_value(runtime.studio_state(Some(0))).unwrap()
        );
        runtime.stop_recording();
        runtime
            .agent_studio_command(
                7,
                Command::Play {
                    user: 0,
                    options: PlaybackOptions::default(),
                },
                &mut engine,
            )
            .unwrap();
        runtime.tick(&mut engine).unwrap();
        assert!(engine.key_held(keyboard, 0x41));
        runtime.lanes[0].last_tick -= Duration::from_secs(101);
        runtime.tick(&mut engine).unwrap();
        let state = runtime.studio_state(None);
        assert!(!engine.key_held(keyboard, 0x41));
        assert!(state.users[0].agent_client.is_none());
        assert_eq!(state.users[0].playback.as_ref().unwrap().state, "completed");
        assert!(runtime.lanes[0].power.is_none());
    }

    #[test]
    fn connected_macro_cannot_send_to_another_permitted_keyboard_process() {
        let (mut runtime, mut engine, _, keyboard) = connected_studio();
        runtime
            .agent_studio_command(
                7,
                Command::Load {
                    user: 0,
                    clip: key_clip(0, keyboard),
                },
                &mut engine,
            )
            .unwrap();
        runtime
            .agent_studio_command(
                7,
                Command::Play {
                    user: 0,
                    options: PlaybackOptions::default(),
                },
                &mut engine,
            )
            .unwrap();
        engine.keyboard_override = Some(Window { hwnd: 20, pid: 202 });
        let error = runtime.tick(&mut engine).unwrap_err();
        assert!(error.downcast_ref::<AgentScopeViolation>().is_some());
        assert!(engine.packets.is_empty());
        assert!(runtime.lanes[0].agent_owner.is_none());
    }

    fn record_command(active: bool) -> Command {
        Command::Record { user: 0, active }
    }
    fn committed_clip(runtime: &Runtime) -> serde_json::Value {
        serde_json::json!({ "revision": runtime.lanes[0].revision, "clip": runtime.lanes[0].clip })
    }
    fn seed_recording(runtime: &mut Runtime, engine: &mut MockEngine, keyboard: u32) {
        runtime
            .agent_studio_command(
                7,
                Command::Load {
                    user: 0,
                    clip: key_clip(0, keyboard),
                },
                engine,
            )
            .unwrap();
        runtime
            .agent_studio_command(7, record_command(true), engine)
            .unwrap();
    }

    #[test]
    fn connected_recording_captures_only_its_owners_accepted_logical_actions() {
        let (mut runtime, mut engine, mouse, keyboard) = connected_studio();
        seed_recording(&mut runtime, &mut engine, keyboard);
        let prior = committed_clip(&runtime);
        runtime
            .agent_action(
                7,
                Action::Text {
                    user: 0,
                    text: "owner".into(),
                },
                &mut engine,
            )
            .unwrap();
        runtime
            .agent_action(
                8,
                Action::Text {
                    user: 0,
                    text: "other".into(),
                },
                &mut engine,
            )
            .unwrap();
        for up in [false, true] {
            runtime
                .action(
                    Action::Packet {
                        user: 0,
                        packet: Packet::key(keyboard, 0x44, up),
                    },
                    &mut engine,
                    true,
                )
                .unwrap();
        }
        let motion = Action::Packet {
            user: 0,
            packet: Packet::motion(mouse, 1, 2),
        };
        runtime.agent_action(7, motion, &mut engine).unwrap();
        assert_eq!(runtime.studio_state(None).users[0].recorded_events, 2);
        assert_eq!(prior, committed_clip(&runtime));
        assert!(
            runtime
                .agent_studio_command(8, record_command(false), &mut engine)
                .is_err()
        );
        assert!(
            runtime
                .agent_studio_command(7, Command::Cancel { user: 0 }, &mut engine)
                .is_err()
        );
        assert!(
            runtime
                .studio_command(Command::Reset { user: 0 }, &mut engine)
                .is_err()
        );
        let state = runtime
            .agent_studio_command(7, record_command(false), &mut engine)
            .unwrap();
        assert!(!state.users[0].recording && state.users[0].recording_client.is_none());
        let events = &state.users[0].clip.as_ref().unwrap().events;
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0].action, Action::Text { text, .. } if text == "owner"));
        assert!(
            matches!(&events[1].action, Action::Packet { packet, .. } if packet.kind == 0 && packet.flags == 0)
        );
    }

    #[test]
    fn empty_or_invalid_connected_recording_preserves_the_committed_clip() {
        let (mut runtime, mut engine, _, keyboard) = connected_studio();
        seed_recording(&mut runtime, &mut engine, keyboard);
        let prior = committed_clip(&runtime);
        assert!(
            runtime
                .agent_studio_command(7, record_command(false), &mut engine)
                .is_err()
        );
        assert_eq!(prior, committed_clip(&runtime));
        assert!(runtime.lanes[0].agent_recording.is_none());
        assert!(
            runtime.studio_state(None).users[0]
                .recording_error
                .is_some()
        );
        runtime
            .agent_studio_command(7, record_command(true), &mut engine)
            .unwrap();
        // A deterministic invalid working candidate; real input is user-validated before capture.
        runtime.lanes[0]
            .agent_recording
            .as_mut()
            .unwrap()
            .recorder
            .capture(
                Action::Text {
                    user: 1,
                    text: "invalid lane".into(),
                },
                Duration::ZERO,
            )
            .unwrap();
        assert!(
            runtime
                .agent_studio_command(7, record_command(false), &mut engine)
                .is_err()
        );
        assert_eq!(prior, committed_clip(&runtime));
        assert!(runtime.lanes[0].agent_recording.is_none());
    }

    #[test]
    fn connected_record_start_checks_target_grant_holds_and_composite_without_preemption() {
        let (mut runtime, mut engine, _, keyboard) = connected_studio();
        engine.window = None;
        assert!(
            runtime
                .agent_studio_command(7, record_command(true), &mut engine)
                .is_err()
        );
        engine.window = Some(Window { hwnd: 30, pid: 303 });
        assert!(
            runtime
                .agent_studio_command(7, record_command(true), &mut engine)
                .is_err()
        );
        engine.window = Some(Window { hwnd: 10, pid: 101 });
        Arc::make_mut(&mut runtime.agent_access).session_control = false;
        assert!(
            runtime
                .agent_studio_command(7, record_command(true), &mut engine)
                .is_err()
        );
        Arc::make_mut(&mut runtime.agent_access).session_control = true;
        runtime
            .action(
                Action::Packet {
                    user: 0,
                    packet: Packet::key(keyboard, 0x44, false),
                },
                &mut engine,
                true,
            )
            .unwrap();
        assert!(
            runtime
                .agent_studio_command(7, record_command(true), &mut engine)
                .is_err()
        );
        assert!(engine.key_held(keyboard, 0x44));
        runtime
            .action(
                Action::Packet {
                    user: 0,
                    packet: Packet::key(keyboard, 0x44, true),
                },
                &mut engine,
                true,
            )
            .unwrap();
        runtime
            .command(automation_runtime::Command::RecordStart, &mut engine)
            .unwrap();
        assert!(
            runtime
                .agent_studio_command(7, record_command(true), &mut engine)
                .is_err()
        );
        assert!(runtime.state(false).recording);
        runtime.stop_recording();
        runtime
            .agent_studio_command(7, record_command(true), &mut engine)
            .unwrap();
        assert!(
            runtime
                .command(automation_runtime::Command::RecordStart, &mut engine)
                .is_err()
        );
        runtime.stop_recording();
        assert!(runtime.lanes[0].agent_recording.is_none());
    }

    #[test]
    fn recording_duration_failure_discards_before_delivery_and_allows_held_release() {
        let (mut runtime, mut engine, _, keyboard) = connected_studio();
        seed_recording(&mut runtime, &mut engine, keyboard);
        let prior = committed_clip(&runtime);
        let down = Packet::key(keyboard, 0x41, false);
        runtime
            .agent_action(
                7,
                Action::Packet {
                    user: 0,
                    packet: down,
                },
                &mut engine,
            )
            .unwrap();
        let expired = runtime.lanes[0].agent_recording.as_ref().unwrap().started
            + Duration::from_secs(90_000);
        let up = released(down);
        let error = runtime
            .agent_action_with_clock(
                7,
                Action::Packet {
                    user: 0,
                    packet: up,
                },
                &mut engine,
                || expired,
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Recording capacity validation failed")
        );
        assert!(runtime.lanes[0].agent_recording.is_none());
        assert!(engine.key_held(keyboard, 0x41));
        assert_eq!(prior, committed_clip(&runtime));
        assert_eq!(engine.packets.len(), 1);
        runtime
            .agent_action(
                7,
                Action::Packet {
                    user: 0,
                    packet: up,
                },
                &mut engine,
            )
            .unwrap();
        assert!(!engine.key_held(keyboard, 0x41));
    }

    #[test]
    fn partial_text_failure_records_only_the_previously_successful_prefix() {
        let (mut runtime, mut engine, mouse, keyboard) = connected_studio();
        seed_recording(&mut runtime, &mut engine, keyboard);
        runtime
            .agent_action(
                7,
                Action::Packet {
                    user: 0,
                    packet: Packet::motion(mouse, 1, 0),
                },
                &mut engine,
            )
            .unwrap();
        let before = engine.packets.len();
        engine.fail_after = Some(before + 1);
        assert!(
            runtime
                .agent_action(
                    7,
                    Action::Text {
                        user: 0,
                        text: "AB".into()
                    },
                    &mut engine
                )
                .is_err()
        );
        assert_eq!(engine.packets.len(), before + 1); // The first character cannot be rolled back.
        assert_eq!(runtime.studio_state(None).users[0].recorded_events, 1);
        let state = runtime
            .agent_studio_command(7, record_command(false), &mut engine)
            .unwrap();
        assert_eq!(state.users[0].clip.as_ref().unwrap().events.len(), 1);
        assert!(matches!(
            state.users[0].clip.as_ref().unwrap().events[0].action,
            Action::Packet { .. }
        ));
    }

    #[test]
    fn failed_tap_release_is_not_recorded_and_keeps_its_input_claim_retryable() {
        let (mut runtime, mut engine, _, keyboard) = connected_studio();
        seed_recording(&mut runtime, &mut engine, keyboard);
        let down = Packet::key(keyboard, 0x41, false);
        runtime
            .agent_action(
                7,
                Action::Packet {
                    user: 0,
                    packet: down,
                },
                &mut engine,
            )
            .unwrap();
        engine.fail_after = Some(engine.packets.len());
        assert!(
            runtime
                .agent_action(
                    7,
                    Action::Packet {
                        user: 0,
                        packet: released(down)
                    },
                    &mut engine
                )
                .is_err()
        );
        assert!(engine.key_held(keyboard, 0x41));
        assert_eq!(runtime.studio_state(None).users[0].recorded_events, 1);
        runtime
            .agent_studio_command(7, record_command(false), &mut engine)
            .unwrap();
        engine.fail_after = None;
        runtime.cancel_agent(7, &mut engine).unwrap();
        assert!(!engine.key_held(keyboard, 0x41));
        assert_eq!(engine.packets.len(), 2);
        assert_eq!(runtime.lanes[0].clip.as_ref().unwrap().events.len(), 1);
    }

    #[test]
    fn graph_transaction_preserves_recording_on_rejection_and_releases_holds_only_on_apply() {
        use crate::automation_runtime::Command as GraphCommand;

        let (mut runtime, mut engine, _, keyboard) = connected_studio();
        seed_recording(&mut runtime, &mut engine, keyboard);
        runtime
            .agent_action(
                7,
                Action::Text {
                    user: 0,
                    text: "take".into(),
                },
                &mut engine,
            )
            .unwrap();
        let down = mux_windows::keyboard::packet_code(keyboard, 118, false).unwrap();
        runtime.packet(down, &mut engine, true).unwrap();
        let count = engine.packets.len();
        let prior = committed_clip(&runtime);
        let graph = || GraphCommand::Graph {
            edges: Vec::new(),
            expected_revision: 0,
        };

        assert!(runtime.command(graph(), &mut engine).is_err());
        assert_eq!(engine.packets.len(), count);
        assert!(engine.key_held(keyboard, 118));
        assert_eq!(runtime.studio_state(None).users[0].recorded_events, 1);
        assert_eq!(committed_clip(&runtime), prior);
        assert_eq!(runtime.state(false).graph_revision, 0);

        runtime
            .agent_studio_command(7, record_command(false), &mut engine)
            .unwrap();
        let committed = committed_clip(&runtime);
        let applied = runtime.command(graph(), &mut engine).unwrap();
        assert!(applied.acknowledged);
        assert_eq!(applied.graph_revision, 1);
        assert!(!engine.key_held(keyboard, 118));
        assert!(runtime.claims.0.is_empty() && runtime.claims.1.is_empty());
        assert_eq!(committed_clip(&runtime), committed);

        runtime.packet(down, &mut engine, false).unwrap();
        let count = engine.packets.len();
        assert!(runtime.command(graph(), &mut engine).is_err());
        runtime
            .command(
                GraphCommand::CancelReplication {
                    expected_revision: 1,
                },
                &mut engine,
            )
            .unwrap();
        assert!(engine.key_held(keyboard, 118));
        assert_eq!(engine.packets.len(), count);
        assert_eq!(runtime.state(false).graph_revision, 1);
        runtime.packet(released(down), &mut engine, false).unwrap();
    }

    #[test]
    fn recording_disconnect_and_local_discard_preserve_clip_and_unrelated_input() {
        let (mut runtime, mut engine, _, keyboard) = connected_studio();
        seed_recording(&mut runtime, &mut engine, keyboard);
        let prior = committed_clip(&runtime);
        let down = Packet::key(keyboard, 0x44, false);
        runtime
            .action(
                Action::Packet {
                    user: 0,
                    packet: down,
                },
                &mut engine,
                true,
            )
            .unwrap();
        runtime.cancel_agent(7, &mut engine).unwrap();
        assert!(engine.key_held(keyboard, 0x44));
        assert_eq!(prior, committed_clip(&runtime));
        runtime
            .action(
                Action::Packet {
                    user: 0,
                    packet: released(down),
                },
                &mut engine,
                true,
            )
            .unwrap();
        runtime
            .agent_studio_command(7, record_command(true), &mut engine)
            .unwrap();
        runtime
            .action(
                Action::Packet {
                    user: 0,
                    packet: down,
                },
                &mut engine,
                true,
            )
            .unwrap();
        runtime
            .studio_command(Command::Cancel { user: 0 }, &mut engine)
            .unwrap();
        assert!(runtime.lanes[0].agent_recording.is_none());
        assert!(engine.key_held(keyboard, 0x44));
        assert_eq!(prior, committed_clip(&runtime));
    }

    #[test]
    fn settings_interruption_rejects_recording_before_touching_capture_or_input() {
        let (mut runtime, mut engine, _, keyboard) = connected_studio();
        seed_recording(&mut runtime, &mut engine, keyboard);
        runtime
            .agent_action(
                7,
                Action::Text {
                    user: 0,
                    text: "keep".into(),
                },
                &mut engine,
            )
            .unwrap();
        let down = Packet::key(keyboard, 0x44, false);
        runtime
            .action(
                Action::Packet {
                    user: 0,
                    packet: down,
                },
                &mut engine,
                true,
            )
            .unwrap();
        let state = serde_json::to_value(runtime.studio_state(Some(0))).unwrap();
        let packets = engine.packets.len();
        assert!(runtime.interrupt_user(0, &mut engine).is_err());
        assert_eq!(
            state,
            serde_json::to_value(runtime.studio_state(Some(0))).unwrap()
        );
        assert_eq!(packets, engine.packets.len());
        assert!(engine.key_held(keyboard, 0x44));
        runtime
            .studio_command(Command::Cancel { user: 0 }, &mut engine)
            .unwrap();
        runtime.interrupt_user(0, &mut engine).unwrap();
        assert!(engine.key_held(keyboard, 0x44));
    }

    #[test]
    fn distinct_extended_source_keys_share_output_until_both_release() {
        use mux_core::output::{KeyOutput, Settings};
        let (mut runtime, mut engine, _, keyboard) = runtime_and_engine();
        let mut settings = Settings::default();
        settings.keys.insert(13, KeyOutput::Key { key: 116 });
        runtime
            .output_command(
                crate::output_runtime::Command::Configure { user: 0, settings },
                &mut engine,
            )
            .unwrap();
        let mut enter = Packet::key(keyboard, 13, false);
        enter.x = 28;
        let mut num_enter = enter;
        num_enter.flags = 2;
        runtime.packet(enter, &mut engine, false).unwrap();
        runtime.packet(num_enter, &mut engine, false).unwrap();
        assert_eq!(engine.packets.len(), 1);
        runtime.packet(released(enter), &mut engine, false).unwrap();
        assert!(engine.key_held(keyboard, 116));
        runtime
            .packet(released(num_enter), &mut engine, false)
            .unwrap();
        assert!(!engine.key_held(keyboard, 116));
        assert_eq!(engine.packets.len(), 2);
    }
}
