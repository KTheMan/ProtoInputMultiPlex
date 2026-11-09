//! Bounded consumer-only registration and virtual press pairing primitives.
use mux_application_sdk::ServerMessage;
use mux_core::application_event::{Event, Poll, Transition, UserId};
use mux_core::desktop::MouseButton;
use serde::Deserialize;
use std::collections::BTreeMap;

/// The callback reads the authenticated event's position, never final-batch state.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) struct ButtonObservation<'a> {
    pub user: &'a UserId,
    pub button: &'a MouseButton,
    pub transition: &'a Transition,
    pub position: &'a mux_core::application_event::Point,
    pub transform_revision: &'a u64,
}

#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn button_observation(event: &Event) -> Option<ButtonObservation<'_>> {
    use mux_core::application_event::{DesktopObservation, DesktopRecordV1, Input};
    match event {
        Event::DesktopObserved {
            user,
            record: DesktopRecordV1 {
                change: DesktopObservation::Button { button, transition },
                screen_revision: transform_revision,
                ..
            },
            position: Some(position),
            ..
        }
        | Event::Input {
            user,
            input: Input::Button { button, transition },
            position: Some(position),
            transform_revision,
            ..
        } => Some(ButtonObservation { user, button, transition, position, transform_revision }),
        _ => None,
    }
}

/// Snapshot queries are structurally validated but never establish/roll back the
/// live stream. A delayed query may legitimately precede its latest watermark.
#[cfg(test)]
pub(crate) fn validate_snapshot_query(
    snapshot: mux_core::application_event::Snapshot,
    epoch: u64,
) -> Result<(), mux_application_sdk::FoldError> {
    validate_snapshot_query_with_desktop(snapshot, epoch, None)
}
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn validate_snapshot_query_with_desktop(
    snapshot: mux_core::application_event::Snapshot,
    epoch: u64,
    version: Option<u32>,
) -> Result<(), mux_application_sdk::FoldError> {
    let mut query = mux_application_sdk::ClientState::default();
    if let Some(version) = version {
        query.enable_desktop_observation(version)?;
    }
    query.install_snapshot(snapshot, epoch)
}

/// Hosted request IDs are monotonic for the WebClient lifetime. Refresh and Gap
/// only invalidate callback dispatch; neither retires a pending request deadline.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn hosted_deadline_pending<T>(pending: &BTreeMap<u64, T>, request_id: u64) -> bool {
    pending.contains_key(&request_id)
}

pub const MAX_REGISTRATIONS: usize = 64;

/// Parse this from JSON text, preserving u64 identities without a JavaScript Number.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventFilter {
    #[serde(default)]
    pub kind: EventKind,
    pub user: Option<UserId>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    #[default]
    Any,
    Server,
    Local,
    Input,
    Users,
    Screens,
    Windows,
    Hosted,
}

impl EventFilter {
    pub fn matches(&self, message: Option<&ServerMessage>) -> bool {
        let Some(message) = message else {
            return self.user.is_none() && matches!(self.kind, EventKind::Any | EventKind::Local);
        };
        if self.user.is_none() && matches!(self.kind, EventKind::Any | EventKind::Server) {
            return true;
        }
        let ServerMessage::EventStream {
            stream: Poll::Events { events },
            ..
        } = message
        else {
            return false;
        };
        events.iter().any(|envelope| {
            let (kind, user) = match &envelope.event {
                Event::Input { user, .. } | Event::DesktopObserved { user, .. } => {
                    (EventKind::Input, Some(*user))
                }
                Event::UserCreated { user } | Event::UserUpdated { user } => {
                    (EventKind::Users, Some(user.id))
                }
                Event::UserDisposed { user } => (EventKind::Users, Some(*user)),
                Event::ScreensChanged { .. } => (EventKind::Screens, None),
                Event::HostedStateChanged { .. } => (EventKind::Hosted, None),
                Event::Window { .. } => (EventKind::Windows, None),
            };
            let kind_matches = matches!(self.kind, EventKind::Any | EventKind::Server)
                || matches!(
                    (self.kind, kind),
                    (EventKind::Input, EventKind::Input)
                        | (EventKind::Users, EventKind::Users)
                        | (EventKind::Screens, EventKind::Screens)
                        | (EventKind::Hosted, EventKind::Hosted)
                        | (EventKind::Windows, EventKind::Windows)
                );
            kind_matches && self.user.is_none_or(|wanted| user == Some(wanted))
        })
    }
}

/// Tokens never repeat, including after clear. Dispatch snapshots IDs and looks
/// up each ID immediately before invocation: removals apply now, additions later.
pub struct Registry<T> {
    next: u64,
    entries: BTreeMap<String, T>,
}
impl<T> Default for Registry<T> {
    fn default() -> Self {
        Self {
            next: 0,
            entries: BTreeMap::new(),
        }
    }
}
impl<T> Registry<T> {
    pub fn insert(&mut self, entry: T) -> Result<String, &'static str> {
        if self.entries.len() >= MAX_REGISTRATIONS {
            return Err("Consumer registration limit reached");
        }
        let next = self
            .next
            .checked_add(1)
            .ok_or("Registration identities exhausted")?;
        let token = format!("web-hook:{next:016x}");
        self.next = next;
        self.entries.insert(token.clone(), entry);
        Ok(token)
    }
    pub fn remove(&mut self, token: &str) -> bool {
        self.entries.remove(token).is_some()
    }
    pub fn get(&self, token: &str) -> Option<&T> {
        self.entries.get(token)
    }
    pub fn entries_mut(&mut self, token: &str) -> Option<&mut T> {
        self.entries.get_mut(token)
    }
    pub fn ids(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.entries.values_mut()
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// One pending press per visible user for a binding's one configured button.
/// `stamp` includes DOM element identity, both rectangles, DPR and screen revision.
pub struct ClickPairs<T> {
    presses: BTreeMap<UserId, T>,
}
impl<T> Default for ClickPairs<T> {
    fn default() -> Self {
        Self {
            presses: BTreeMap::new(),
        }
    }
}
impl<T: PartialEq> ClickPairs<T> {
    pub fn transition(&mut self, user: UserId, transition: Transition, hit: Option<T>) -> bool {
        match transition {
            Transition::Down => {
                self.presses.remove(&user);
                if let Some(stamp) = hit
                    && self.presses.len() < mux_core::application_event::MAX_USERS
                {
                    self.presses.insert(user, stamp);
                }
                false
            }
            Transition::Up => {
                let pressed = self.presses.remove(&user);
                pressed.is_some() && pressed == hit
            }
        }
    }
    pub fn retire(&mut self, user: UserId) {
        self.presses.remove(&user);
    }
    pub fn clear(&mut self) {
        self.presses.clear();
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClickOptions {
    pub selector: String,
    pub user: Option<UserId>,
    #[serde(default = "primary")]
    pub button: MouseButton,
}
fn primary() -> MouseButton {
    MouseButton::Primary
}

#[cfg(test)]
mod tests {
    use super::*;
    fn user(local: u64) -> UserId {
        UserId {
            epoch: u64::MAX,
            local,
        }
    }

    #[test]
    fn provider_selector_pairs_original_up_position_before_later_batch_motion() {
        use crate::geometry::CanvasGeometry;
        use mux_application_sdk::{ClientState, StreamState};
        use mux_core::application_event::*;
        use mux_core::desktop::{Bounds, CursorShape};
        use std::collections::BTreeSet;

        let bounds = Bounds { left: 0, top: 0, right: 400, bottom: 400 };
        let screens = ScreenState { revision: 7, desktop: bounds, screens: vec![bounds] };
        let click_position = Point { x: 100, y: 110 };
        let later_position = Point { x: 300, y: 310 };
        let mut engine = Engine::new(u64::MAX, screens.clone()).unwrap();
        let owner = engine.register_recipient("private provider").unwrap();
        let pointer = engine.register_recipient("pointer selector").unwrap();
        let keyboard = engine.register_recipient("key-only receiver").unwrap();
        let baseline = DesktopBaselineV1 {
            version: 1,
            next_sequence: 1,
            screens: screens.clone(),
            position: click_position,
            cursor: ConfiguredDesktopCursor { shape: CursorShape::System, size: 32.5,
                color: [1, 2, 3], visible: true, follow_application: false },
            held_buttons: Vec::new(),
            native_held_keys: Vec::new(),
        };
        let a = engine.create_desktop_observed(owner, "A", baseline.clone()).unwrap();
        // B's baseline hold must not manufacture a selector DOWN or consume A's press.
        let b = engine.create_desktop_observed(owner, "B", DesktopBaselineV1 {
            held_buttons: vec![MouseButton::X1], ..baseline
        }).unwrap();
        for (recipient, pointer_events, key_events) in [(pointer, true, false), (keyboard, false, true)] {
            engine.approve_grant(recipient, Grant {
                users: UserScope::Only(BTreeSet::from([a, b])),
                pointer_events, key_events, owned_windows: false, host_state: false,
            }).unwrap();
            engine.approve_desktop_observation(recipient, Some(DesktopConsentV1 {
                version: 1, text_units: false,
            })).unwrap();
            engine.negotiate_desktop_observation(recipient, Some(1)).unwrap();
        }
        let (subscription, snapshot) = engine.subscribe(pointer, 16).unwrap();
        let (key_subscription, key_snapshot) = engine.subscribe(keyboard, 16).unwrap();
        assert!(snapshot.users.iter().all(|view| view.held_keys.is_none()
            && view.desktop.as_ref().unwrap().native_held_keys.is_none()));
        assert!(key_snapshot.users.iter().all(|view| view.pointer.is_none()
            && view.desktop.as_ref().unwrap().configured_cursor.is_none()));
        let mut model = ClientState::default();
        model.enable_desktop_observation(1).unwrap();
        model.install_snapshot(snapshot, u64::MAX).unwrap();
        let original_model = model.clone();
        let source_a = model.users()[&a].source;
        let source_b = model.users()[&b].source;
        assert_ne!(source_a, source_b);

        for (user, sequence, change) in [
            (a, 1, DesktopObservation::Button { button: MouseButton::X1, transition: Transition::Down }),
            (b, 1, DesktopObservation::Button { button: MouseButton::X1, transition: Transition::Up }),
            (a, 2, DesktopObservation::Button { button: MouseButton::X1, transition: Transition::Up }),
            (a, 3, DesktopObservation::Motion { position: later_position }),
        ] {
            engine.publish_desktop_observation(owner, user, DesktopRecordV1 {
                version: 1, sequence, screen_revision: screens.revision,
                admission: DesktopAdmission::LogicalOnly, change,
            }).unwrap();
        }
        // One controlled batch, not an assumption about runtime IPC framing.
        let batch = engine.poll(pointer, subscription, 16).unwrap();
        model.fold_poll(batch.clone()).unwrap();
        assert_eq!(model.state(), &StreamState::Live);
        assert_eq!(model.users()[&a].pointer.as_ref().unwrap().position, later_position);
        assert!(model.users()[&a].pointer.as_ref().unwrap().held_buttons.is_empty());
        let message = ServerMessage::EventStream { subscription, stream: batch.clone() };
        assert!(EventFilter { kind: EventKind::Input, user: Some(a) }.matches(Some(&message)));
        assert!(!EventFilter { kind: EventKind::Input, user: Some(user(9_007_199_254_740_993)) }.matches(Some(&message)));

        let geometry = CanvasGeometry { left: 20.0, top: 30.0, width: 400.0,
            height: 400.0, pixel_ratio: 2.0 };
        let transform = geometry.transform(model.screens().unwrap()).unwrap();
        let options = ClickOptions { selector: "#target".into(), user: None, button: MouseButton::X1 };
        // A deterministic target identity/rectangle stands in for DOM hit_stamp.
        // ClickPairs is the same target/geometry pairing API used by that callback.
        let hit = |point: [f64; 2]| {
            ((100.0..160.0).contains(&point[0]) && (120.0..180.0).contains(&point[1]))
                .then_some((options.selector.as_str(), geometry, screens.revision))
        };
        let final_local = transform.to_canvas(later_position, screens.revision).unwrap();
        let final_point = [final_local[0] + geometry.left, final_local[1] + geometry.top];
        assert!(hit(final_point).is_none());
        let Poll::Events { events } = &batch else { panic!("expected provider records") };
        assert_eq!(events.len(), 4);
        let mut pairs = ClickPairs::default();
        let mut deliveries = Vec::new();
        for envelope in events {
            let Some(edge) = button_observation(&envelope.event) else { continue };
            let Event::DesktopObserved { user: original_user, source, .. } = &envelope.event
                else { panic!("expected original provider event") };
            assert_eq!(edge.user, original_user);
            assert_eq!(*source, model.users()[edge.user].source);
            assert_eq!(*edge.button, options.button);
            assert_eq!(*edge.transform_revision, screens.revision);
            assert!(transform.to_canvas(*edge.position, screens.revision + 1).is_err());
            let local = transform.to_canvas(*edge.position, *edge.transform_revision).unwrap();
            let point = [local[0] + geometry.left, local[1] + geometry.top];
            let stamp = hit(point);
            if pairs.transition(*edge.user, *edge.transition, stamp) {
                deliveries.push((*edge.user, *source, stamp.unwrap().0, point, envelope.clone()));
            }
        }
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].0, a);
        assert_eq!(deliveries[0].1, source_a);
        assert_eq!(deliveries[0].2, "#target");
        assert_eq!(deliveries[0].3, [120.0, 140.0]);
        assert_ne!(deliveries[0].3, final_point);
        assert_eq!(deliveries[0].4, events[2]); // Original A UP, never later Motion/B's UP.

        let key_batch = engine.poll(keyboard, key_subscription, 16).unwrap();
        assert!(matches!(&key_batch, Poll::Events { events } if events.is_empty()));
        assert!(!EventFilter { kind: EventKind::Input, user: None }.matches(Some(&ServerMessage::EventStream {
            subscription: key_subscription, stream: key_batch,
        })));
        let mut redacted = events[2].event.clone();
        if let Event::DesktopObserved { position, .. } = &mut redacted { *position = None; }
        assert!(button_observation(&redacted).is_none());

        let mut wrong_source = batch;
        if let Poll::Events { events } = &mut wrong_source
            && let Event::DesktopObserved { source, .. } = &mut events[2].event {
            *source = source_b;
        }
        let mut rejected = original_model;
        assert!(rejected.fold_poll(wrong_source).is_err());
        assert!(rejected.users().is_empty());
    }

    #[test]
    fn delayed_snapshot_query_validates_without_rolling_back_the_live_stream() {
        use mux_core::application_event::{ApplicationMode, ScreenState, Snapshot};
        use mux_core::desktop::Bounds;
        let bounds = Bounds {
            left: -100,
            top: -100,
            right: 100,
            bottom: 100,
        };
        let mut snapshot = Snapshot {
            hosted: None,
            version: 1,
            epoch: u64::MAX,
            through_sequence: 20,
            users: Vec::new(),
            screens: ScreenState {
                revision: 1,
                desktop: bounds,
                screens: vec![bounds],
            },
            windows: Vec::new(),
            capabilities: Vec::new(),
            mode: ApplicationMode::Cooperative,
        };
        let mut actual = mux_application_sdk::ClientState::default();
        actual.install_snapshot(snapshot.clone(), u64::MAX).unwrap();
        let before = actual.clone();
        snapshot.through_sequence = 10;
        assert!(validate_snapshot_query(snapshot.clone(), u64::MAX).is_ok());
        assert_eq!(actual, before);
        assert_eq!(actual.last_sequence(), 20);
        snapshot.epoch = 1;
        assert!(validate_snapshot_query(snapshot, u64::MAX).is_err());
        assert_eq!(actual, before);
    }

    #[test]
    fn exact_identity_and_local_filter() {
        let filter: EventFilter = serde_json::from_str(
            r#"{"kind":"input","user":{"epoch":18446744073709551615,"local":9007199254740993}}"#,
        )
        .unwrap();
        assert_eq!(filter.user, Some(user(9_007_199_254_740_993)));
        assert!(!filter.matches(None));
        assert!(EventFilter::default().matches(None));
        assert!(serde_json::from_str::<EventFilter>(r#"{"kind":"input","extra":true}"#).is_err());
    }

    #[test]
    fn event_kind_and_exact_user_must_match_the_same_event() {
        let message: ServerMessage = serde_json::from_value(serde_json::json!({
            "kind": "event_stream", "subscription": 1,
            "stream": {"kind": "events", "events": [
                {"version": 1, "epoch": u64::MAX, "sequence": 7,
                 "event": {"kind": "user_disposed", "user": user(1)}},
                {"version": 1, "epoch": u64::MAX, "sequence": 8,
                 "event": {"kind": "input", "user": user(2), "source": 4,
                    "input": {"kind": "button", "button": "primary", "transition": "down"},
                    "position": {"x": 0, "y": 0}, "transform_revision": 1}}
            ]}
        }))
        .unwrap();
        let mut filter = EventFilter {
            kind: EventKind::Input,
            user: Some(user(1)),
        };
        assert!(!filter.matches(Some(&message)));
        filter.user = Some(user(2));
        assert!(filter.matches(Some(&message)));
        filter.user = Some(UserId { epoch: 1, local: 2 });
        assert!(!filter.matches(Some(&message)));
        filter.kind = EventKind::Local;
        filter.user = None;
        assert!(!filter.matches(Some(&message)));
    }

    #[test]
    fn removals_apply_during_dispatch_additions_wait_and_handles_do_not_repeat() {
        let mut registry = Registry::default();
        let first = registry.insert(1).unwrap();
        let second = registry.insert(2).unwrap();
        let snapshot = registry.ids();
        registry.remove(&second);
        let third = registry.insert(3).unwrap();
        assert_eq!(
            snapshot
                .iter()
                .filter_map(|id| registry.get(id))
                .copied()
                .collect::<Vec<_>>(),
            vec![1]
        );
        assert!(!snapshot.contains(&third));
        registry.clear();
        assert_ne!(first, registry.insert(4).unwrap());
        for _ in 1..MAX_REGISTRATIONS {
            registry.insert(0).unwrap();
        }
        assert!(registry.insert(0).is_err());
    }

    #[test]
    fn virtual_pairing_is_per_user_and_geometry_and_never_snapshot_held() {
        let mut pairs = ClickPairs::default();
        assert!(!pairs.transition(user(1), Transition::Up, Some(9)));
        assert!(!pairs.transition(user(1), Transition::Down, Some(9)));
        assert!(!pairs.transition(user(2), Transition::Up, Some(9)));
        assert!(!pairs.transition(user(1), Transition::Up, Some(10)));
        pairs.transition(user(1), Transition::Down, Some(9));
        assert!(pairs.transition(user(1), Transition::Up, Some(9)));
        pairs.transition(user(1), Transition::Down, Some(9));
        pairs.retire(user(1));
        assert!(!pairs.transition(user(1), Transition::Up, Some(9)));
        pairs.transition(user(1), Transition::Down, Some(9));
        pairs.clear(); // gap, refresh, epoch, disconnect and dispose share this reset.
        assert!(!pairs.transition(user(1), Transition::Up, Some(9)));
        assert!(!pairs.transition(UserId { epoch: 1, local: 1 }, Transition::Up, Some(9)));
    }
}

#[cfg(test)]
mod hosted_filter_tests {
    use super::*;

    #[test]
    fn hosted_filter_matches_only_hosted_events_and_preserves_exact_u64() {
        let filter: EventFilter = serde_json::from_str(r#"{"kind":"hosted"}"#).unwrap();
        let message: ServerMessage = serde_json::from_str(r#"{"kind":"event_stream","subscription":1,"stream":{"kind":"events","events":[{"version":1,"epoch":18446744073709551615,"sequence":1,"event":{"kind":"hosted_state_changed","state":{"revision":18446744073709551615,"routing_generation":9007199254740993,"phase":"ready","mode":"concurrent","multi_keyboard":true,"minimized":false}}}]}}"#).unwrap();
        assert!(filter.matches(Some(&message)));
        assert!(!filter.matches(None));
        let reference: mux_application_sdk::HostRevision =
            serde_json::from_str(r#"{"epoch":18446744073709551615,"revision":9007199254740993}"#)
                .unwrap();
        assert_eq!(reference.epoch, u64::MAX);
        assert_eq!(reference.revision, 9_007_199_254_740_993);
    }
}

#[cfg(test)]
mod hosted_deadline_tests {
    use super::*;

    fn assert_dispatch_fence_preserves_request_deadline() {
        let mut pending = BTreeMap::from([(9_u64, ())]);
        let mut hooks = Registry::default();
        hooks.insert(()).unwrap();
        hooks.clear(); // Same registry fence used by Refresh and EventStream Gap.
        assert!(hosted_deadline_pending(&pending, 9));
        // A matched receipt or terminal stop retires this exact deadline.
        pending.remove(&9);
        assert!(!hosted_deadline_pending(&pending, 9));
        // A later request never revives a stale timer from a retired connection.
        pending.insert(10, ());
        assert!(!hosted_deadline_pending(&pending, 9));
        assert!(hosted_deadline_pending(&pending, 10));
    }

    #[test]
    fn refresh_callback_fence_does_not_cancel_hosted_deadline() {
        assert_dispatch_fence_preserves_request_deadline();
    }

    #[test]
    fn event_stream_gap_callback_fence_does_not_cancel_hosted_deadline() {
        assert_dispatch_fence_preserves_request_deadline();
    }
}
