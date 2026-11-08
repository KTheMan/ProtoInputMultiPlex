//! Transport-neutral Rust state folding, usable by a later Rust/WASM binding.
//! A transport authenticates snapshots and supplies its current expected epoch.
use crate::protocol::MAX_POLL_EVENTS;
use mux_core::application_event::*;
use mux_core::desktop::{Bounds, CursorShape, MouseButton};
use std::collections::{BTreeMap, BTreeSet, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamState {
    AwaitingSnapshot,
    Live,
    NeedsSnapshot,
    Ended(EndReason),
    Disconnected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoldError {
    NotLive,
    Version,
    Epoch,
    Sequence,
    Invalid(&'static str),
}

impl std::fmt::Display for FoldError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}
impl std::error::Error for FoldError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoldOutcome {
    Applied(usize),
    NeedsSnapshot,
    Ended(EndReason),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClientState {
    state: StreamState,
    epoch: Option<u64>,
    last_sequence: u64,
    users: BTreeMap<UserId, UserView>,
    windows: BTreeMap<WindowId, WindowInfo>,
    screens: Option<ScreenState>,
    capabilities: Vec<CapabilityStatus>,
    mode: Option<ApplicationMode>,
}

impl Default for ClientState {
    fn default() -> Self {
        Self {
            state: StreamState::AwaitingSnapshot,
            epoch: None,
            last_sequence: 0,
            users: BTreeMap::new(),
            windows: BTreeMap::new(),
            screens: None,
            capabilities: Vec::new(),
            mode: None,
        }
    }
}

impl ClientState {
    pub fn state(&self) -> &StreamState {
        &self.state
    }
    pub fn epoch(&self) -> Option<u64> {
        self.epoch
    }
    pub fn last_sequence(&self) -> u64 {
        self.last_sequence
    }
    pub fn users(&self) -> &BTreeMap<UserId, UserView> {
        &self.users
    }
    pub fn windows(&self) -> &BTreeMap<WindowId, WindowInfo> {
        &self.windows
    }
    pub fn screens(&self) -> Option<&ScreenState> {
        self.screens.as_ref()
    }
    pub fn capabilities(&self) -> &[CapabilityStatus] {
        &self.capabilities
    }
    pub fn mode(&self) -> Option<ApplicationMode> {
        self.mode
    }

    /// Installs a fresh snapshot for the authenticated connection's expected epoch.
    /// The caller obtains that epoch from its trusted handshake, not an old event.
    pub fn install_snapshot(
        &mut self,
        snapshot: Snapshot,
        expected_epoch: u64,
    ) -> Result<(), FoldError> {
        match self.validated_snapshot(snapshot, expected_epoch) {
            Ok(next) => {
                *self = next;
                Ok(())
            }
            Err(error) => {
                self.clear(StreamState::NeedsSnapshot);
                Err(error)
            }
        }
    }

    fn validated_snapshot(
        &self,
        snapshot: Snapshot,
        expected_epoch: u64,
    ) -> Result<Self, FoldError> {
        if snapshot.version != PROTOCOL_VERSION {
            return Err(FoldError::Version);
        }
        if expected_epoch == 0 || snapshot.epoch != expected_epoch {
            return Err(FoldError::Epoch);
        }
        if self.epoch == Some(expected_epoch) && snapshot.through_sequence < self.last_sequence {
            return Err(FoldError::Sequence);
        }
        snapshot
            .screens
            .validate()
            .map_err(|_| FoldError::Invalid("screens"))?;
        if snapshot.users.len() > MAX_USERS
            || snapshot.windows.len() > 128
            || snapshot.capabilities.len() > 32
        {
            return Err(FoldError::Invalid("snapshot limits"));
        }
        if snapshot.mode != ApplicationMode::Cooperative {
            return Err(FoldError::Invalid("unsupported application mode"));
        }
        let mut users = BTreeMap::new();
        for user in snapshot.users {
            validate_user(&user, expected_epoch, snapshot.screens.desktop)?;
            if users
                .values()
                .any(|existing: &UserView| existing.source == user.source)
                || users.insert(user.id, user).is_some()
            {
                return Err(FoldError::Invalid("duplicate user or source"));
            }
        }
        let mut windows = BTreeMap::new();
        for window in snapshot.windows {
            validate_window(&window)?;
            if windows.insert(window.id, window).is_some() {
                return Err(FoldError::Invalid("duplicate window"));
            }
        }
        for (index, status) in snapshot.capabilities.iter().enumerate() {
            if status.detail.len() > 512
                || snapshot.capabilities[..index]
                    .iter()
                    .any(|earlier| earlier.capability == status.capability)
            {
                return Err(FoldError::Invalid("capability list"));
            }
        }
        Ok(Self {
            state: StreamState::Live,
            epoch: Some(snapshot.epoch),
            last_sequence: snapshot.through_sequence,
            users,
            windows,
            screens: Some(snapshot.screens),
            capabilities: snapshot.capabilities,
            mode: Some(snapshot.mode),
        })
    }

    /// A sequence skip is legitimate for recipient filtering. Only an explicit gap
    /// requests resubscribe. Malformed/duplicate/reordered batches fail closed.
    /// The whole batch is atomic: no valid prefix is committed if a suffix is bad.
    pub fn fold_poll(&mut self, poll: Poll) -> Result<FoldOutcome, FoldError> {
        if self.state != StreamState::Live {
            return Err(FoldError::NotLive);
        }
        match poll {
            Poll::Gap {
                after_sequence,
                lost_through,
            } => {
                let valid = after_sequence >= self.last_sequence && lost_through > after_sequence;
                self.clear(StreamState::NeedsSnapshot);
                if !valid {
                    return Err(FoldError::Sequence);
                }
                // Resubscription must cover every event reported as lost. A
                // pre-gap snapshot could otherwise restore stale held input.
                self.last_sequence = lost_through;
                Ok(FoldOutcome::NeedsSnapshot)
            }
            Poll::Ended { reason } => {
                self.clear(StreamState::Ended(reason));
                Ok(FoldOutcome::Ended(reason))
            }
            Poll::Events { events } => {
                if events.len() > MAX_POLL_EVENTS {
                    self.clear(StreamState::NeedsSnapshot);
                    return Err(FoldError::Invalid("batch limit"));
                }
                let count = events.len();
                let mut next = self.clone();
                for event in events {
                    if let Err(error) = next.fold_event(event) {
                        self.clear(StreamState::NeedsSnapshot);
                        return Err(error);
                    }
                }
                *self = next;
                Ok(FoldOutcome::Applied(count))
            }
        }
    }

    pub fn disconnect(&mut self) {
        self.clear(StreamState::Disconnected);
    }

    fn clear(&mut self, state: StreamState) {
        // Removing the complete user state releases all held keys/buttons and
        // prevents stale cursor rendering. Keep the last watermark against rollback.
        self.users.clear();
        self.windows.clear();
        self.screens = None;
        self.capabilities.clear();
        self.mode = None;
        self.state = state;
    }

    fn fold_event(&mut self, envelope: Envelope) -> Result<(), FoldError> {
        if envelope.version != PROTOCOL_VERSION {
            return Err(FoldError::Version);
        }
        if Some(envelope.epoch) != self.epoch {
            return Err(FoldError::Epoch);
        }
        if envelope.sequence <= self.last_sequence {
            return Err(FoldError::Sequence);
        }
        let screens = self.screens.as_ref().ok_or(FoldError::NotLive)?;
        let epoch = self.epoch.ok_or(FoldError::Epoch)?;
        match envelope.event {
            Event::UserCreated { user } => {
                validate_user(&user, epoch, screens.desktop)?;
                if self.users.len() >= MAX_USERS
                    || self.users.contains_key(&user.id)
                    || self
                        .users
                        .values()
                        .any(|existing| existing.source == user.source)
                {
                    return Err(FoldError::Invalid("duplicate user or source"));
                }
                self.users.insert(user.id, user);
            }
            Event::UserUpdated { user } => {
                validate_user(&user, epoch, screens.desktop)?;
                let previous = self
                    .users
                    .get(&user.id)
                    .ok_or(FoldError::Invalid("unknown updated user"))?;
                if previous.source != user.source {
                    return Err(FoldError::Invalid("source changed"));
                }
                if previous.owned_by_recipient != user.owned_by_recipient {
                    return Err(FoldError::Invalid("user ownership changed"));
                }
                // Preserve all observed cursor/color/render fields by replacing the
                // complete authorized state, including consent-redacted Options.
                self.users.insert(user.id, user);
            }
            Event::UserDisposed { user } => {
                if self.users.remove(&user).is_none() {
                    return Err(FoldError::Invalid("unknown disposed user"));
                }
            }
            Event::Input {
                user,
                source,
                input,
                position,
                transform_revision,
            } => {
                if transform_revision != screens.revision {
                    return Err(FoldError::Invalid("stale input transform"));
                }
                let view = self
                    .users
                    .get_mut(&user)
                    .ok_or(FoldError::Invalid("unknown input user"))?;
                if view.source != source {
                    return Err(FoldError::Invalid("input source"));
                }
                fold_input(view, input, position, screens.desktop)?;
            }
            Event::ScreensChanged { screens: next } => {
                next.validate().map_err(|_| FoldError::Invalid("screens"))?;
                if next.revision <= screens.revision
                    || self.users.values().any(|user| {
                        user.pointer.as_ref().is_some_and(|pointer| {
                            !next
                                .desktop
                                .contains(pointer.position.x, pointer.position.y)
                        })
                    })
                {
                    return Err(FoldError::Invalid("screen revision or bounds"));
                }
                self.screens = Some(next);
            }
            Event::Window { update } => match update {
                WindowUpdate::Tracked { window } => {
                    validate_window(&window)?;
                    if self.windows.len() >= 128 && !self.windows.contains_key(&window.id) {
                        return Err(FoldError::Invalid("window limit"));
                    }
                    self.windows.insert(window.id, window);
                }
                WindowUpdate::Changed { window } => {
                    validate_window(&window)?;
                    if !self.windows.contains_key(&window.id) {
                        return Err(FoldError::Invalid("untracked window"));
                    }
                    self.windows.insert(window.id, window);
                }
                WindowUpdate::Untracked { window } | WindowUpdate::Destroyed { window } => {
                    if self.windows.remove(&window).is_none() {
                        return Err(FoldError::Invalid("untracked window"));
                    }
                }
            },
        }
        self.last_sequence = envelope.sequence;
        Ok(())
    }
}

fn validate_user(user: &UserView, epoch: u64, desktop: Bounds) -> Result<(), FoldError> {
    if user.id.epoch != epoch
        || user.id.local == 0
        || user.source.0 == 0
        || user.name.is_empty()
        || user.name.len() > 128
        || user.name.trim() != user.name
        || user.name.chars().any(char::is_control)
    {
        return Err(FoldError::Invalid("user identity"));
    }
    if let Some(pointer) = &user.pointer {
        let distinct: HashSet<MouseButton> = pointer.held_buttons.iter().copied().collect();
        if !desktop.contains(pointer.position.x, pointer.position.y)
            || pointer.held_buttons.len() > 5
            || distinct.len() != pointer.held_buttons.len()
        {
            return Err(FoldError::Invalid("pointer state"));
        }
    }
    if let Some(cursor) = &user.cursor
        && (user.pointer.is_none()
            || !(8..=256).contains(&cursor.size)
            || cursor.shape == CursorShape::System)
    {
        return Err(FoldError::Invalid("cursor state"));
    }
    if let Some(keys) = &user.held_keys {
        let distinct: BTreeSet<KeyCode> = keys.iter().copied().collect();
        if keys.len() > 256 || distinct.len() != keys.len() || keys.iter().any(|key| !key.valid()) {
            return Err(FoldError::Invalid("key state"));
        }
    }
    if user.pointer.is_none() && user.held_keys.is_none() {
        return Err(FoldError::Invalid("empty user authority"));
    }
    Ok(())
}

fn validate_window(window: &WindowInfo) -> Result<(), FoldError> {
    if window.bounds.valid_region() {
        Ok(())
    } else {
        Err(FoldError::Invalid("window bounds"))
    }
}

fn fold_input(
    user: &mut UserView,
    input: Input,
    position: Option<Point>,
    desktop: Bounds,
) -> Result<(), FoldError> {
    if position.is_some() != user.pointer.is_some() {
        return Err(FoldError::Invalid("pointer consent mismatch"));
    }
    if let Some(point) = position
        && !desktop.contains(point.x, point.y)
    {
        return Err(FoldError::Invalid("input coordinate"));
    }
    match input {
        Input::Motion { position: next } => {
            if position != Some(next) {
                return Err(FoldError::Invalid("motion coordinate mismatch"));
            }
            user.pointer
                .as_mut()
                .ok_or(FoldError::Invalid("pointer grant missing"))?
                .position = next;
        }
        Input::Button { button, transition } => {
            let pointer = user
                .pointer
                .as_mut()
                .ok_or(FoldError::Invalid("pointer grant missing"))?;
            if position != Some(pointer.position) {
                return Err(FoldError::Invalid("button coordinate mismatch"));
            }
            fold_transition(&mut pointer.held_buttons, button, transition)?;
        }
        Input::Wheel { units, .. } => {
            let pointer = user
                .pointer
                .as_ref()
                .ok_or(FoldError::Invalid("pointer grant missing"))?;
            if units == 0 || position != Some(pointer.position) {
                return Err(FoldError::Invalid("wheel state"));
            }
        }
        Input::Key { key, transition } => {
            if !key.valid() {
                return Err(FoldError::Invalid("key code"));
            }
            if user
                .pointer
                .as_ref()
                .is_some_and(|pointer| position != Some(pointer.position))
            {
                return Err(FoldError::Invalid("key coordinate mismatch"));
            }
            fold_transition(
                user.held_keys
                    .as_mut()
                    .ok_or(FoldError::Invalid("key grant missing"))?,
                key,
                transition,
            )?;
        }
    }
    Ok(())
}

fn fold_transition<T: PartialEq>(
    held: &mut Vec<T>,
    value: T,
    transition: Transition,
) -> Result<(), FoldError> {
    match (transition, held.iter().position(|item| *item == value)) {
        (Transition::Down, None) => held.push(value),
        (Transition::Up, Some(index)) => {
            held.remove(index);
        }
        _ => return Err(FoldError::Invalid("unbalanced input transition")),
    }
    Ok(())
}

/// Explicit application-event name for the same transport-neutral state machine.
pub type EventClientState = ClientState;
