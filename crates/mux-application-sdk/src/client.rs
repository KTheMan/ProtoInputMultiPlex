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
    desktop_version: Option<u32>,
    state: StreamState,
    epoch: Option<u64>,
    last_sequence: u64,
    // Engine retains the original last-delivered sequence while advancing loss.
    // Keep the origin separate from last_sequence, which is the recovery floor.
    gap_origin: Option<u64>,
    users: BTreeMap<UserId, UserView>,
    windows: BTreeMap<WindowId, WindowInfo>,
    screens: Option<ScreenState>,
    capabilities: Vec<CapabilityStatus>,
    mode: Option<ApplicationMode>,
    hosted: Option<HostedState>,
    toolkit: crate::user_toolkit::UserToolkit,
}

/// A validated query or action receipt can arrive before its ordered stream
/// event. Keep that observation separate so the later equal-revision event is
/// still folded and checked against the same state.
#[derive(Debug, Clone, Default)]
pub struct HostedReceipt {
    epoch: Option<u64>,
    state: Option<HostedState>,
}

impl HostedReceipt {
    fn consistent(first: &HostedState, second: &HostedState) -> bool {
        if first.revision == second.revision {
            first == second
        } else if first.revision > second.revision {
            first.routing_generation >= second.routing_generation
        } else {
            second.routing_generation >= first.routing_generation
        }
    }

    pub fn clear(&mut self) {
        self.epoch = None;
        self.state = None;
    }

    pub fn observe(
        &mut self,
        epoch: u64,
        observed: &HostedState,
        model: &ClientState,
    ) -> Result<(), FoldError> {
        if model.state() != &StreamState::Live || model.epoch() != Some(epoch) {
            return Err(FoldError::NotLive);
        }
        observed
            .validate()
            .map_err(|_| FoldError::Invalid("hosted receipt"))?;
        if let Some(stream) = model.hosted() {
            if !Self::consistent(stream, observed) {
                return Err(FoldError::Invalid("hosted receipt conflicts with stream"));
            }
            if stream.revision > observed.revision {
                return Ok(());
            }
        }
        if self.epoch == Some(epoch)
            && let Some(previous) = &self.state
        {
            if !Self::consistent(previous, observed) {
                return Err(FoldError::Invalid("conflicting hosted receipts"));
            }
            if previous.revision > observed.revision {
                return Ok(());
            }
        }
        self.epoch = Some(epoch);
        self.state = Some(observed.clone());
        Ok(())
    }

    pub fn reconcile(&mut self, model: &ClientState) -> Result<(), FoldError> {
        if model.state() != &StreamState::Live || self.epoch != model.epoch() {
            self.clear();
            return Ok(());
        }
        if let (Some(stream), Some(receipt)) = (model.hosted(), self.state.as_ref()) {
            if !Self::consistent(stream, receipt) {
                return Err(FoldError::Invalid("hosted event conflicts with receipt"));
            }
            if stream.revision >= receipt.revision {
                self.clear();
            }
        }
        Ok(())
    }

    pub fn current(&self, model: &ClientState, epoch: u64) -> Option<HostRevision> {
        if model.state() != &StreamState::Live || model.epoch() != Some(epoch) {
            return None;
        }
        let stream = model.hosted();
        let receipt = if self.epoch == Some(epoch) {
            self.state.as_ref()
        } else {
            None
        };
        if let (Some(stream), Some(receipt)) = (stream, receipt)
            && !Self::consistent(stream, receipt)
        {
            return None;
        }
        let revision = stream
            .map(|state| state.revision)
            .into_iter()
            .chain(receipt.map(|state| state.revision))
            .max()?;
        Some(HostRevision { epoch, revision })
    }

    pub fn permits(&self, model: &ClientState, reference: HostRevision) -> bool {
        self.current(model, reference.epoch) == Some(reference)
    }
}

impl Default for ClientState {
    fn default() -> Self {
        Self {
            desktop_version: None,
            state: StreamState::AwaitingSnapshot,
            epoch: None,
            last_sequence: 0,
            gap_origin: None,
            users: BTreeMap::new(),
            windows: BTreeMap::new(),
            screens: None,
            capabilities: Vec::new(),
            mode: None,
            hosted: None,
            toolkit: crate::user_toolkit::UserToolkit::default(),
        }
    }
}

impl ClientState {
    /// Explicit decoder opt-in before installing any provider snapshot or event.
    pub fn enable_desktop_observation(&mut self, version: u32) -> Result<(), FoldError> {
        if version != 1 || self.state == StreamState::Live {
            return Err(FoldError::Version);
        }
        self.desktop_version = Some(version);
        Ok(())
    }
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
    /// Executes a versioned consumer-local operation. Never queues a Host request.
    pub fn local_toolkit(
        &mut self,
        request: crate::user_toolkit::ToolkitRequest,
    ) -> Result<crate::user_toolkit::ToolkitResponse, crate::user_toolkit::ToolkitError> {
        use crate::user_toolkit::{TOOLKIT_VERSION, ToolkitError};
        if request.version != TOOLKIT_VERSION {
            return Err(ToolkitError::Invalid);
        }
        if self.state != StreamState::Live {
            return Err(ToolkitError::NotLive);
        }
        self.toolkit.execute(&self.users, request.operation)
    }

    /// Exact u64 identities remain JSON integers; callers pass text, never JS Number.
    pub fn local_toolkit_json(
        &mut self,
        request: &str,
    ) -> Result<String, crate::user_toolkit::ToolkitError> {
        use crate::user_toolkit::{MAX_TOOLKIT_REQUEST_BYTES, ToolkitError, ToolkitRequest};
        if request.is_empty() || request.len() > MAX_TOOLKIT_REQUEST_BYTES {
            return Err(ToolkitError::Limit);
        }
        let request: ToolkitRequest =
            serde_json::from_str(request).map_err(|_| ToolkitError::Invalid)?;
        let response = self.local_toolkit(request)?;
        serde_json::to_string(&response).map_err(|_| ToolkitError::Invalid)
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
    pub fn hosted(&self) -> Option<&HostedState> {
        self.hosted.as_ref()
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
            Ok(mut next) => {
                next.toolkit.reconcile(expected_epoch, &next.users);
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
        if let Some(state) = &snapshot.hosted {
            state
                .validate()
                .map_err(|_| FoldError::Invalid("hosted state"))?;
            if self.epoch == Some(expected_epoch)
                && self.hosted.as_ref().is_some_and(|previous| {
                    state.revision < previous.revision
                        || state.routing_generation < previous.routing_generation
                        || (state.revision == previous.revision && state != previous)
                })
            {
                return Err(FoldError::Invalid("hosted state rollback"));
            }
        }
        if self.desktop_version != Some(1)
            && (snapshot.users.iter().any(|user| user.desktop.is_some())
                || snapshot
                    .capabilities
                    .iter()
                    .any(|status| status.capability == Capability::DesktopObservationV1))
        {
            return Err(FoldError::Version);
        }
        if snapshot.users.iter().any(|user| {
            user.desktop
                .as_ref()
                .is_some_and(|desktop| desktop.screen_revision != snapshot.screens.revision)
        }) {
            return Err(FoldError::Invalid("desktop screen revision"));
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
            desktop_version: self.desktop_version,
            state: StreamState::Live,
            epoch: Some(snapshot.epoch),
            last_sequence: snapshot.through_sequence,
            gap_origin: None,
            users,
            windows,
            screens: Some(snapshot.screens),
            capabilities: snapshot.capabilities,
            mode: Some(snapshot.mode),
            hosted: snapshot.hosted,
            toolkit: self.toolkit.clone(),
        })
    }

    /// A sequence skip is legitimate for recipient filtering. Only an explicit gap
    /// requests resubscribe. Malformed/duplicate/reordered batches fail closed.
    /// The whole batch is atomic: no valid prefix is committed if a suffix is bad.
    pub fn fold_poll(&mut self, poll: Poll) -> Result<FoldOutcome, FoldError> {
        if self.state == StreamState::NeedsSnapshot {
            return match poll {
                Poll::Gap {
                    after_sequence,
                    lost_through,
                } if self.gap_origin == Some(after_sequence)
                    && lost_through > after_sequence
                    && lost_through >= self.last_sequence =>
                {
                    self.last_sequence = lost_through;
                    Ok(FoldOutcome::NeedsSnapshot)
                }
                Poll::Gap { .. } => Err(FoldError::Sequence),
                Poll::Ended { reason } => {
                    self.clear(StreamState::Ended(reason));
                    Ok(FoldOutcome::Ended(reason))
                }
                Poll::Events { .. } => Err(FoldError::NotLive),
            };
        }
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
                self.gap_origin = Some(after_sequence);
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
        self.gap_origin = None;
    }

    fn clear(&mut self, state: StreamState) {
        // Removing the complete user state releases all held keys/buttons and
        // prevents stale cursor rendering. Keep the last watermark against rollback.
        self.toolkit = crate::user_toolkit::UserToolkit::default();
        self.users.clear();
        self.windows.clear();
        self.screens = None;
        self.capabilities.clear();
        self.mode = None;
        self.hosted = None;
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
            Event::DesktopObserved {
                user,
                source,
                record,
                position,
            } => {
                if self.desktop_version != Some(1) {
                    return Err(FoldError::Version);
                }
                let view = self
                    .users
                    .get_mut(&user)
                    .ok_or(FoldError::Invalid("unknown desktop user"))?;
                if view.source != source {
                    return Err(FoldError::Invalid("desktop source"));
                }
                fold_desktop_observation(view, record, screens)
                    .map_err(|_| FoldError::Invalid("desktop observation"))?;
                if view.pointer.as_ref().map(|pointer| pointer.position) != position {
                    return Err(FoldError::Invalid("desktop event position"));
                }
            }
            Event::HostedStateChanged { state } => {
                state
                    .validate()
                    .map_err(|_| FoldError::Invalid("hosted state"))?;
                if self.hosted.as_ref().is_some_and(|previous| {
                    state.revision <= previous.revision
                        || state.routing_generation < previous.routing_generation
                }) {
                    return Err(FoldError::Invalid("hosted state rollback"));
                }
                self.hosted = Some(state);
            }
            Event::UserCreated { user } => {
                if user.desktop.is_some() && self.desktop_version != Some(1) {
                    return Err(FoldError::Version);
                }
                validate_user(&user, epoch, screens.desktop)?;
                if user
                    .desktop
                    .as_ref()
                    .is_some_and(|desktop| desktop.screen_revision != screens.revision)
                {
                    return Err(FoldError::Invalid("desktop screen revision"));
                }
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
                if user.desktop.is_some() && self.desktop_version != Some(1) {
                    return Err(FoldError::Version);
                }
                validate_user(&user, epoch, screens.desktop)?;
                if user
                    .desktop
                    .as_ref()
                    .is_some_and(|desktop| desktop.screen_revision != screens.revision)
                {
                    return Err(FoldError::Invalid("desktop screen revision"));
                }
                let previous = self
                    .users
                    .get(&user.id)
                    .ok_or(FoldError::Invalid("unknown updated user"))?;
                if previous.source != user.source
                    || previous.desktop.is_some() != user.desktop.is_some()
                {
                    return Err(FoldError::Invalid("source changed"));
                }
                if previous
                    .desktop
                    .as_ref()
                    .zip(user.desktop.as_ref())
                    .is_some_and(|(old, next)| next.last_sequence < old.last_sequence)
                {
                    return Err(FoldError::Sequence);
                }
                if previous.owned_by_recipient != user.owned_by_recipient {
                    return Err(FoldError::Invalid("user ownership changed"));
                }
                // Preserve all observed cursor/color/render fields by replacing the
                // complete authorized state, including consent-redacted Options.
                self.users.insert(user.id, user);
            }
            Event::UserDisposed { user } => {
                self.toolkit.remove(user);
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
                if view.source != source || view.desktop.is_some() {
                    return Err(FoldError::Invalid("input source"));
                }
                fold_input(view, input, position, screens.desktop)?;
            }
            Event::ScreensChanged { screens: next } => {
                if self.users.values().any(|user| user.desktop.is_some()) {
                    return Err(FoldError::Invalid("desktop binding screen change"));
                }
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
    if let Some(observed) = &user.desktop {
        if observed.version != 1
            || user.owned_by_recipient
            || user.cursor.is_some()
            || user.held_keys.is_some()
            || (observed.configured_cursor.is_some() != user.pointer.is_some())
            || (observed.text_units && observed.native_held_keys.is_none())
        {
            return Err(FoldError::Invalid("desktop user authority"));
        }
        if let Some(cursor) = observed.configured_cursor {
            cursor
                .validate()
                .map_err(|_| FoldError::Invalid("configured desktop cursor"))?;
        }
        if let Some(keys) = &observed.native_held_keys {
            validate_native_keys(keys).map_err(|_| FoldError::Invalid("native held keys"))?;
        }
    }
    if user.pointer.is_none()
        && user.held_keys.is_none()
        && user
            .desktop
            .as_ref()
            .is_none_or(|desktop| desktop.native_held_keys.is_none())
    {
        return Err(FoldError::Invalid("empty user authority"));
    }
    Ok(())
}

fn validate_window(window: &WindowInfo) -> Result<(), FoldError> {
    if !window.bounds.valid_region() {
        return Err(FoldError::Invalid("window bounds"));
    }
    window.metadata.validate().map_err(FoldError::Invalid)
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

#[cfg(test)]
mod hosted_receipt_tests {
    use super::*;

    fn state(revision: u64, mode: HostedMode) -> HostedState {
        HostedState {
            revision,
            routing_generation: 4,
            phase: HostedPhase::Ready,
            mode: Some(mode),
            multi_keyboard: Some(false),
            minimized: Some(false),
        }
    }

    #[test]
    fn reply_ahead_of_equal_stream_event_remains_exact_and_foldable() {
        let bounds = Bounds {
            left: 0,
            top: 0,
            right: 640,
            bottom: 480,
        };
        let mut model = ClientState::default();
        model
            .install_snapshot(
                Snapshot {
                    hosted: Some(state(1, HostedMode::Switched)),
                    version: 1,
                    epoch: 71,
                    through_sequence: 1,
                    users: Vec::new(),
                    screens: ScreenState {
                        revision: 1,
                        desktop: bounds,
                        screens: vec![bounds],
                    },
                    windows: Vec::new(),
                    capabilities: Vec::new(),
                    mode: ApplicationMode::Cooperative,
                },
                71,
            )
            .unwrap();
        let mut generation_receipt = HostedReceipt::default();
        generation_receipt
            .observe(71, &state(3, HostedMode::Concurrent), &model)
            .unwrap();
        let mut older_with_future_generation = state(2, HostedMode::Concurrent);
        older_with_future_generation.routing_generation = 5;
        assert!(
            generation_receipt
                .observe(71, &older_with_future_generation, &model)
                .is_err()
        );
        let mut newer_with_rolled_back_generation = state(4, HostedMode::Concurrent);
        newer_with_rolled_back_generation.routing_generation = 3;
        assert!(
            generation_receipt
                .observe(71, &newer_with_rolled_back_generation, &model)
                .is_err()
        );
        let mut contradictory_generation_stream = model.clone();
        contradictory_generation_stream
            .fold_poll(Poll::Events {
                events: vec![Envelope {
                    version: 1,
                    epoch: 71,
                    sequence: 2,
                    event: Event::HostedStateChanged {
                        state: older_with_future_generation,
                    },
                }],
            })
            .unwrap();
        assert!(
            generation_receipt
                .reconcile(&contradictory_generation_stream)
                .is_err()
        );
        assert!(
            generation_receipt
                .current(&contradictory_generation_stream, 71)
                .is_none()
        );
        let mut receipt = HostedReceipt::default();
        let updated = state(2, HostedMode::Concurrent);
        receipt.observe(71, &updated, &model).unwrap();
        assert!(receipt.permits(
            &model,
            HostRevision {
                epoch: 71,
                revision: 2
            }
        ));
        for revision in [1, 3] {
            assert!(!receipt.permits(
                &model,
                HostRevision {
                    epoch: 71,
                    revision
                }
            ));
        }
        assert!(!receipt.permits(
            &model,
            HostRevision {
                epoch: 72,
                revision: 2
            }
        ));
        assert_eq!(model.hosted().unwrap().revision, 1);
        model
            .fold_poll(Poll::Events {
                events: vec![Envelope {
                    version: 1,
                    epoch: 71,
                    sequence: 2,
                    event: Event::HostedStateChanged {
                        state: updated.clone(),
                    },
                }],
            })
            .unwrap();
        receipt.reconcile(&model).unwrap();
        assert!(receipt.permits(
            &model,
            HostRevision {
                epoch: 71,
                revision: 2
            }
        ));
        let mut conflicting = updated;
        conflicting.mode = Some(HostedMode::Switched);
        assert!(receipt.observe(71, &conflicting, &model).is_err());
        let mut contradictory_stream = model.clone();
        let third = state(3, HostedMode::Concurrent);
        receipt.observe(71, &third, &model).unwrap();
        contradictory_stream
            .fold_poll(Poll::Events {
                events: vec![Envelope {
                    version: 1,
                    epoch: 71,
                    sequence: 3,
                    event: Event::HostedStateChanged {
                        state: state(3, HostedMode::Switched),
                    },
                }],
            })
            .unwrap();
        assert!(receipt.reconcile(&contradictory_stream).is_err());
        assert!(receipt.permits(
            &model,
            HostRevision {
                epoch: 71,
                revision: 3
            }
        ));
        model
            .fold_poll(Poll::Gap {
                after_sequence: 2,
                lost_through: 3,
            })
            .unwrap();
        receipt.reconcile(&model).unwrap();
        assert!(!receipt.permits(
            &model,
            HostRevision {
                epoch: 71,
                revision: 3
            }
        ));
        model.disconnect();
        receipt.reconcile(&model).unwrap();
        assert!(!receipt.permits(
            &model,
            HostRevision {
                epoch: 71,
                revision: 2
            }
        ));
    }
}
