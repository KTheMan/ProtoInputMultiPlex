use super::*;
use crate::desktop::{CursorShape, MouseButton};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

struct User {
    owner: RecipientId,
    view: UserView,
}

struct Recipient {
    /// Local host supplied identity, retained privately and never emitted to clients.
    identity: String,
    grant: Grant,
    tracked: BTreeSet<WindowId>,
    window_access: BTreeMap<WindowId, WindowAccess>,
}

struct Subscriber {
    recipient: RecipientId,
    capacity: usize,
    queue: VecDeque<Envelope>,
    last_delivered: u64,
    delivered: u64,
    gap: Option<(u64, u64)>,
    ended: Option<EndReason>,
}

struct OwnedWindow {
    owner: RecipientId,
    info: WindowInfo,
}

/// An embedded host owns this engine and alone calls authority/registration methods.
/// Do not expose this object directly as an untrusted command-RPC method table.
/// All calls are serialized by the embedding event loop; no background workers exist.
pub struct Engine {
    epoch: u64,
    sequence: u64,
    next_id: u64,
    ended: bool,
    screens: ScreenState,
    users: BTreeMap<UserId, User>,
    recipients: BTreeMap<RecipientId, Recipient>,
    subscribers: BTreeMap<SubscriptionId, Subscriber>,
    windows: BTreeMap<WindowId, OwnedWindow>,
}

impl Engine {
    /// Epoch must be unique for each host session; the host persists or randomizes it.
    pub fn new(epoch: u64, screens: ScreenState) -> Result<Self, Error> {
        if epoch == 0 {
            return Err(Error::Invalid("nonzero host session epoch required"));
        }
        screens.validate()?;
        Ok(Self {
            epoch,
            sequence: 0,
            next_id: 0,
            ended: false,
            screens,
            users: BTreeMap::new(),
            recipients: BTreeMap::new(),
            subscribers: BTreeMap::new(),
            windows: BTreeMap::new(),
        })
    }

    fn active(&self) -> Result<(), Error> {
        if self.ended {
            Err(Error::Ended)
        } else {
            Ok(())
        }
    }

    fn id(&mut self) -> Result<u64, Error> {
        self.next_id = self.next_id.checked_add(1).ok_or(Error::Limit)?;
        Ok(self.next_id)
    }

    fn reserve_events(&self, count: u64) -> Result<(), Error> {
        self.active()?;
        self.sequence.checked_add(count).ok_or(Error::Limit)?;
        Ok(())
    }

    /// Host-only: identity must already be verified locally. This creates no users
    /// and enables no event export. Transport credentials are deliberately separate.
    pub fn register_recipient(&mut self, local_identity: &str) -> Result<RecipientId, Error> {
        self.active()?;
        validate_name(local_identity)?;
        if self.recipients.len() >= MAX_RECIPIENTS {
            return Err(Error::Limit);
        }
        let id = RecipientId(self.id()?);
        self.recipients.insert(
            id,
            Recipient {
                identity: local_identity.into(),
                grant: Grant::default(),
                tracked: BTreeSet::new(),
                window_access: BTreeMap::new(),
            },
        );
        Ok(id)
    }

    pub fn set_mode(&mut self, mode: ApplicationMode) -> Result<ApplicationMode, Error> {
        self.active()?;
        match mode {
            ApplicationMode::Cooperative => Ok(mode),
            _ => Err(Error::Unsupported(
                "only cooperative application mode is available",
            )),
        }
    }

    pub fn host_control(&mut self, _control: HostControl) -> Result<(), Error> {
        self.active()?;
        Err(Error::Unsupported("host UI adapter unavailable"))
    }

    pub fn recipient_identity(&self, recipient: RecipientId) -> Result<&str, Error> {
        self.recipients
            .get(&recipient)
            .map(|value| value.identity.as_str())
            .ok_or(Error::MissingRecipient)
    }

    /// Host-only explicit consent. Every change retires queued data and subscriptions;
    /// a client must subscribe again for an appropriately redacted fresh snapshot.
    pub fn approve_grant(&mut self, recipient: RecipientId, grant: Grant) -> Result<(), Error> {
        self.active()?;
        if let UserScope::Only(users) = &grant.users
            && users.iter().any(|id| !self.users.contains_key(id))
        {
            return Err(Error::MissingUser);
        }
        let previous = self
            .recipients
            .get(&recipient)
            .ok_or(Error::MissingRecipient)?
            .grant
            .clone();
        if previous != grant {
            // Revoking producer consent also retires its held state. Observers cannot
            // clear somebody else's keys simply by changing their own export grant.
            let affected: Vec<_> = self
                .users
                .iter()
                .filter_map(|(id, user)| {
                    (user.owner == recipient
                        && ((previous.pointer_events
                            && scope_allows(&previous.users, *id, Some(recipient), recipient)
                            && (!grant.pointer_events
                                || !scope_allows(&grant.users, *id, Some(recipient), recipient)))
                            || (previous.key_events
                                && scope_allows(&previous.users, *id, Some(recipient), recipient)
                                && (!grant.key_events
                                    || !scope_allows(
                                        &grant.users,
                                        *id,
                                        Some(recipient),
                                        recipient,
                                    )))))
                    .then_some(*id)
                })
                .collect();
            self.reserve_events(affected.len() as u64)?;
            let reason = if grant.enabled() {
                EndReason::GrantChanged
            } else {
                EndReason::Revoked
            };
            let current = self
                .recipients
                .get_mut(&recipient)
                .ok_or(Error::MissingRecipient)?;
            current.grant = grant.clone();
            if !current.grant.owned_windows {
                current.tracked.clear();
                current.window_access.clear();
            }
            self.end_recipient_subscriptions(recipient, reason);
            for id in affected {
                let user = self.users.get_mut(&id).ok_or(Error::MissingUser)?;
                if (!grant.pointer_events
                    || !scope_allows(&grant.users, id, Some(recipient), recipient))
                    && let Some(pointer) = user.view.pointer.as_mut()
                {
                    pointer.held_buttons.clear();
                }
                if (!grant.key_events
                    || !scope_allows(&grant.users, id, Some(recipient), recipient))
                    && let Some(keys) = user.view.held_keys.as_mut()
                {
                    keys.clear();
                }
                let view = user.view.clone();
                self.publish(Event::UserUpdated { user: view });
            }
        }
        Ok(())
    }

    fn end_recipient_subscriptions(&mut self, recipient: RecipientId, reason: EndReason) {
        for subscriber in self
            .subscribers
            .values_mut()
            .filter(|value| value.recipient == recipient)
        {
            subscriber.queue.clear();
            subscriber.gap = None;
            subscriber.ended = Some(reason);
        }
    }

    pub fn create_user(&mut self, owner: RecipientId, name: &str) -> Result<UserId, Error> {
        self.reserve_events(1)?;
        validate_name(name)?;
        if !self.recipients.contains_key(&owner) {
            return Err(Error::MissingRecipient);
        }
        if self.users.len() >= MAX_USERS {
            return Err(Error::Limit);
        }
        let local = self.id()?;
        let id = UserId {
            epoch: self.epoch,
            local,
        };
        let view = UserView {
            id,
            owned_by_recipient: false,
            name: name.into(),
            source: SourceId(local),
            pointer: Some(PointerState {
                position: Point {
                    x: self.screens.desktop.left,
                    y: self.screens.desktop.top,
                },
                held_buttons: Vec::new(),
            }),
            cursor: Some(CursorState::default()),
            held_keys: Some(Vec::new()),
        };
        self.users.insert(
            id,
            User {
                owner,
                view: view.clone(),
            },
        );
        self.publish(Event::UserCreated { user: view });
        Ok(id)
    }

    fn own_user(&self, owner: RecipientId, user: UserId) -> Result<&User, Error> {
        self.active()?;
        let value = self.users.get(&user).ok_or(Error::MissingUser)?;
        if value.owner != owner {
            return Err(Error::Denied);
        }
        Ok(value)
    }

    pub fn rename_user(
        &mut self,
        owner: RecipientId,
        user: UserId,
        name: &str,
    ) -> Result<(), Error> {
        self.reserve_events(1)?;
        self.own_user(owner, user)?;
        validate_name(name)?;
        let value = self.users.get_mut(&user).ok_or(Error::MissingUser)?;
        value.view.name = name.into();
        let view = value.view.clone();
        self.publish(Event::UserUpdated { user: view });
        Ok(())
    }

    pub fn dispose_user(&mut self, owner: RecipientId, user: UserId) -> Result<(), Error> {
        self.reserve_events(1)?;
        self.own_user(owner, user)?;
        // Keep ownership available to publication filtering until disposal is emitted.
        // Disposal is a release-all boundary; no synthetic target input.
        self.publish(Event::UserDisposed { user });
        self.users.remove(&user);
        Ok(())
    }

    pub fn cursor_control(
        &mut self,
        owner: RecipientId,
        user: UserId,
        control: CursorControl,
    ) -> Result<CursorState, Error> {
        self.reserve_events(1)?;
        let mut cursor = self
            .own_user(owner, user)?
            .view
            .cursor
            .clone()
            .ok_or(Error::MissingUser)?;
        match control {
            CursorControl::Visible(value) => cursor.visible = value,
            CursorControl::Shape(value) => {
                if value == CursorShape::System {
                    return Err(Error::Unsupported("system cursor renderer"));
                }
                if cursor.type_locked && value != cursor.shape {
                    return Err(Error::Locked);
                }
                cursor.shape = value;
            }
            CursorControl::Size(value) if (8..=256).contains(&value) => cursor.size = value,
            CursorControl::Size(_) => return Err(Error::Invalid("cursor size must be 8-256")),
            CursorControl::Color(value) => cursor.color = value,
            CursorControl::MovementLocked(value) => cursor.movement_locked = value,
            CursorControl::TypeLocked(value) => cursor.type_locked = value,
            CursorControl::Theme(_) => return Err(Error::Unsupported("cursor themes")),
            CursorControl::Flags(0) => {}
            CursorControl::Flags(_) => return Err(Error::Unsupported("cursor flags")),
        }
        let value = self.users.get_mut(&user).ok_or(Error::MissingUser)?;
        value.view.cursor = Some(cursor.clone());
        let view = value.view.clone();
        self.publish(Event::UserUpdated { user: view });
        Ok(cursor)
    }

    /// Accepted directly into the cooperative event loop. Consent is checked before
    /// state mutation; acceptance carries no target-input delivery promise.
    pub fn submit(&mut self, owner: RecipientId, user: UserId, input: Input) -> Result<u64, Error> {
        self.reserve_events(1)?;
        self.own_user(owner, user)?;
        let grant = &self
            .recipients
            .get(&owner)
            .ok_or(Error::MissingRecipient)?
            .grant;
        if !scope_allows(&grant.users, user, Some(owner), owner)
            || if input.is_key() {
                !grant.key_events
            } else {
                !grant.pointer_events
            }
        {
            return Err(Error::Denied);
        }
        let value = self.users.get_mut(&user).ok_or(Error::MissingUser)?;
        let pointer = value.view.pointer.as_mut().ok_or(Error::MissingUser)?;
        match input {
            Input::Motion { position } => {
                if value
                    .view
                    .cursor
                    .as_ref()
                    .is_some_and(|cursor| cursor.movement_locked)
                {
                    return Err(Error::Locked);
                }
                if !self.screens.desktop.contains(position.x, position.y) {
                    return Err(Error::Invalid("position outside desktop"));
                }
                pointer.position = position;
            }
            Input::Button { button, transition } => {
                transition_vec(&mut pointer.held_buttons, button, transition)?;
            }
            Input::Wheel { units: 0, .. } => return Err(Error::Invalid("zero wheel units")),
            Input::Wheel { .. } => {}
            Input::Key { key, transition } => {
                if !key.valid() {
                    return Err(Error::Invalid("unsupported USB HID keyboard usage"));
                }
                transition_vec(
                    value.view.held_keys.as_mut().ok_or(Error::MissingUser)?,
                    key,
                    transition,
                )?;
            }
        }
        let source = value.view.source;
        let position = pointer.position;
        self.publish(Event::Input {
            user,
            source,
            input,
            position: Some(position),
            transform_revision: self.screens.revision,
        });
        Ok(self.sequence)
    }

    /// Validates before the down event, then publishes exactly one down and one up.
    pub fn click(
        &mut self,
        owner: RecipientId,
        user: UserId,
        button: MouseButton,
    ) -> Result<[u64; 2], Error> {
        self.reserve_events(2)?;
        if self
            .own_user(owner, user)?
            .view
            .pointer
            .as_ref()
            .is_some_and(|pointer| pointer.held_buttons.contains(&button))
        {
            return Err(Error::InvalidTransition);
        }
        let down = self.submit(
            owner,
            user,
            Input::Button {
                button,
                transition: Transition::Down,
            },
        )?;
        let up = self.submit(
            owner,
            user,
            Input::Button {
                button,
                transition: Transition::Up,
            },
        )?;
        Ok([down, up])
    }

    pub fn subscribe(
        &mut self,
        recipient: RecipientId,
        capacity: usize,
    ) -> Result<(SubscriptionId, Snapshot), Error> {
        self.active()?;
        if capacity == 0 || capacity > MAX_QUEUE || self.subscribers.len() >= MAX_SUBSCRIBERS {
            return Err(Error::Limit);
        }
        let snapshot = self.snapshot(recipient)?;
        let id = SubscriptionId(self.id()?);
        self.subscribers.insert(
            id,
            Subscriber {
                recipient,
                capacity,
                queue: VecDeque::new(),
                last_delivered: self.sequence,
                delivered: 0,
                gap: None,
                ended: None,
            },
        );
        Ok((id, snapshot))
    }

    pub fn snapshot(&self, recipient: RecipientId) -> Result<Snapshot, Error> {
        self.active()?;
        let recipient_id = recipient;
        let recipient = self
            .recipients
            .get(&recipient)
            .ok_or(Error::MissingRecipient)?;
        if !recipient.grant.enabled() {
            return Err(Error::Denied);
        }
        let users = self
            .users
            .values()
            .filter_map(|user| view_for(&user.view, &recipient.grant, user.owner, recipient_id))
            .collect();
        let windows = if recipient.grant.owned_windows {
            recipient
                .tracked
                .iter()
                .filter_map(|id| self.windows.get(id).map(|value| value.info.clone()))
                .collect()
        } else {
            Vec::new()
        };
        Ok(Snapshot {
            version: PROTOCOL_VERSION,
            epoch: self.epoch,
            through_sequence: self.sequence,
            users,
            screens: self.screens.clone(),
            windows,
            capabilities: capabilities(),
            mode: ApplicationMode::Cooperative,
        })
    }

    /// Subscription tokens must be bound to an authenticated recipient by transports.
    pub fn poll(
        &mut self,
        recipient: RecipientId,
        subscription: SubscriptionId,
        max_events: usize,
    ) -> Result<Poll, Error> {
        if max_events == 0 || max_events > MAX_QUEUE {
            return Err(Error::Limit);
        }
        let value = self
            .subscribers
            .get_mut(&subscription)
            .ok_or(Error::MissingSubscription)?;
        if value.recipient != recipient {
            return Err(Error::Denied);
        }
        if let Some(reason) = value.ended {
            self.subscribers.remove(&subscription);
            return Ok(Poll::Ended { reason });
        }
        if let Some((after_sequence, lost_through)) = value.gap {
            return Ok(Poll::Gap {
                after_sequence,
                lost_through,
            });
        }
        let events: Vec<_> = value
            .queue
            .drain(..max_events.min(value.queue.len()))
            .collect();
        if let Some(last) = events.last() {
            value.last_delivered = last.sequence;
        }
        value.delivered = value.delivered.saturating_add(events.len() as u64);
        Ok(Poll::Events { events })
    }

    pub fn subscriber_stats(
        &self,
        recipient: RecipientId,
        subscription: SubscriptionId,
    ) -> Result<SubscriberStats, Error> {
        let value = self
            .subscribers
            .get(&subscription)
            .ok_or(Error::MissingSubscription)?;
        if value.recipient != recipient {
            return Err(Error::Denied);
        }
        Ok(SubscriberStats {
            capacity: value.capacity,
            queued: value.queue.len(),
            delivered: value.delivered,
            overflowed: value.gap.is_some(),
        })
    }

    pub fn unsubscribe(
        &mut self,
        recipient: RecipientId,
        subscription: SubscriptionId,
    ) -> Result<(), Error> {
        let value = self
            .subscribers
            .get(&subscription)
            .ok_or(Error::MissingSubscription)?;
        if value.recipient != recipient {
            return Err(Error::Denied);
        }
        self.subscribers.remove(&subscription);
        Ok(())
    }

    pub fn resubscribe(
        &mut self,
        recipient: RecipientId,
        subscription: SubscriptionId,
        capacity: usize,
    ) -> Result<(SubscriptionId, Snapshot), Error> {
        // Validate first: a rejected refresh does not destroy a healthy subscription.
        self.snapshot(recipient)?;
        if capacity == 0 || capacity > MAX_QUEUE {
            return Err(Error::Limit);
        }
        let value = self
            .subscribers
            .get(&subscription)
            .ok_or(Error::MissingSubscription)?;
        if value.recipient != recipient {
            return Err(Error::Denied);
        }
        if value.ended.is_some() {
            return Err(Error::Ended);
        }
        self.unsubscribe(recipient, subscription)?;
        self.subscribe(recipient, capacity)
    }

    fn publish(&mut self, event: Event) {
        // Every mutating caller reserves its sequence space before changing state.
        self.sequence += 1;
        let user_id = match &event {
            Event::UserCreated { user } | Event::UserUpdated { user } => Some(user.id),
            Event::UserDisposed { user } | Event::Input { user, .. } => Some(*user),
            _ => None,
        };
        let user_owner = user_id.and_then(|id| self.users.get(&id).map(|user| user.owner));
        for subscriber in self.subscribers.values_mut() {
            if subscriber.ended.is_some() {
                continue;
            }
            let Some(recipient) = self.recipients.get(&subscriber.recipient) else {
                continue;
            };
            let Some(event) = event_for(&event, recipient, subscriber.recipient, user_owner) else {
                continue;
            };
            if let Some((_, lost_through)) = subscriber.gap.as_mut() {
                *lost_through = self.sequence;
                continue;
            }
            if subscriber.queue.len() >= subscriber.capacity {
                subscriber.queue.clear();
                subscriber.gap = Some((subscriber.last_delivered, self.sequence));
                continue;
            }
            subscriber.queue.push_back(Envelope {
                version: PROTOCOL_VERSION,
                epoch: self.epoch,
                sequence: self.sequence,
                event,
            });
        }
    }

    pub fn update_screens(&mut self, screens: ScreenState) -> Result<(), Error> {
        self.reserve_events(1)?;
        screens.validate()?;
        if screens.revision <= self.screens.revision {
            return Err(Error::StaleRevision);
        }
        // Avoid silently moving held pointers when a desktop changes. The host must
        // retire/reposition affected users explicitly before changing the topology.
        if self.users.values().any(|user| {
            user.view.pointer.as_ref().is_some_and(|pointer| {
                !screens
                    .desktop
                    .contains(pointer.position.x, pointer.position.y)
            })
        }) {
            return Err(Error::Invalid("screen update excludes existing cursor"));
        }
        self.screens = screens.clone();
        self.publish(Event::ScreensChanged { screens });
        Ok(())
    }

    pub fn disconnect_recipient(&mut self, recipient: RecipientId) -> Result<(), Error> {
        self.active()?;
        if !self.recipients.contains_key(&recipient) {
            return Err(Error::MissingRecipient);
        }
        let users: Vec<_> = self
            .users
            .iter()
            .filter_map(|(id, user)| (user.owner == recipient).then_some(*id))
            .collect();
        self.reserve_events(users.len() as u64 + self.windows.len() as u64)?;
        self.end_recipient_subscriptions(recipient, EndReason::RecipientDisconnected);
        for user in users {
            self.publish(Event::UserDisposed { user });
            self.users.remove(&user);
        }
        let windows: Vec<_> = self
            .windows
            .iter()
            .filter_map(|(id, window)| (window.owner == recipient).then_some(*id))
            .collect();
        for window in windows {
            self.destroy_window(window)?;
        }
        self.recipients.remove(&recipient);
        Ok(())
    }

    /// Stops all streams and retires queued input, users, grants and owned windows.
    pub fn end(&mut self) {
        self.retire(EndReason::SessionEnded);
        self.ended = true;
    }

    /// Host supplies a new unique epoch. Old tokens can only drain their terminal
    /// notification or be explicitly unsubscribed; they cannot access new state.
    pub fn reset(&mut self, epoch: u64, screens: ScreenState) -> Result<(), Error> {
        self.active()?;
        if epoch == 0 || epoch == self.epoch {
            return Err(Error::Invalid("fresh epoch required"));
        }
        screens.validate()?;
        self.retire(EndReason::Reset);
        self.epoch = epoch;
        self.sequence = 0;
        self.screens = screens;
        Ok(())
    }

    fn retire(&mut self, reason: EndReason) {
        for subscriber in self.subscribers.values_mut() {
            subscriber.queue.clear();
            subscriber.gap = None;
            subscriber.ended = Some(reason);
        }
        self.users.clear();
        self.windows.clear();
        self.recipients.clear();
    }

    /// Host-only: register an already verified same-process window. Native handle
    /// ownership checks belong to the adapter, never to an untrusted caller.
    pub fn register_owned_window(
        &mut self,
        owner: RecipientId,
        observed: WindowObservation,
    ) -> Result<WindowId, Error> {
        self.register_owned_window_with_state(owner, observed, WindowObservationState::default())
    }

    pub fn register_owned_window_with_state(
        &mut self,
        owner: RecipientId,
        observed: WindowObservation,
        observation: WindowObservationState,
    ) -> Result<WindowId, Error> {
        self.active()?;
        if !self.recipients.contains_key(&owner) {
            return Err(Error::MissingRecipient);
        }
        if !observed.bounds.valid_region() {
            return Err(Error::Invalid("window bounds"));
        }
        observed.metadata.validate().map_err(Error::Invalid)?;
        if self.windows.len() >= 128 {
            return Err(Error::Limit);
        }
        let id = WindowId(self.id()?);
        self.windows.insert(
            id,
            OwnedWindow {
                owner,
                info: WindowInfo {
                    id,
                    bounds: observed.bounds,
                    metadata: observed.metadata,
                    observation,
                    visible: observed.visible,
                    minimized: observed.minimized,
                    maximized: observed.maximized,
                    click_through: observed.click_through,
                    enabled: observed.enabled,
                    topmost: observed.topmost,
                    opacity: observed.opacity,
                },
            },
        );
        Ok(id)
    }

    pub fn window_info(&self, owner: RecipientId, window: WindowId) -> Result<WindowInfo, Error> {
        self.active()?;
        let recipient = self.recipients.get(&owner).ok_or(Error::MissingRecipient)?;
        let value = self.windows.get(&window).ok_or(Error::MissingWindow)?;
        if !recipient.grant.owned_windows
            || (value.owner != owner
                && !recipient
                    .window_access
                    .get(&window)
                    .is_some_and(|access| access.observe))
        {
            return Err(Error::Denied);
        }
        Ok(value.info.clone())
    }

    /// Host UI only: grant or revoke one recipient's view of one existing window.
    /// The owner's publication grant is a separate prerequisite.
    pub fn delegate_window(
        &mut self,
        window: WindowId,
        recipient: RecipientId,
        access: WindowAccess,
    ) -> Result<(), Error> {
        self.reserve_events(1)?;
        if access.control && !access.observe {
            return Err(Error::Invalid("window control needs observation"));
        }
        let owner = self.windows.get(&window).ok_or(Error::MissingWindow)?.owner;
        if owner == recipient {
            return Err(Error::Invalid("owner does not need delegation"));
        }
        if !self
            .recipients
            .get(&owner)
            .is_some_and(|value| value.grant.owned_windows)
        {
            return Err(Error::Denied);
        }
        let grantee = self
            .recipients
            .get_mut(&recipient)
            .ok_or(Error::MissingRecipient)?;
        if !grantee.grant.owned_windows && (access.observe || access.control) {
            return Err(Error::Denied);
        }
        if !access.observe {
            grantee.window_access.remove(&window);
            grantee.tracked.remove(&window);
        } else {
            grantee.window_access.insert(window, access);
        }
        self.end_recipient_subscriptions(recipient, EndReason::GrantChanged);
        Ok(())
    }

    pub fn window_owner(&self, window: WindowId) -> Result<RecipientId, Error> {
        self.active()?;
        self.windows
            .get(&window)
            .map(|value| value.owner)
            .ok_or(Error::MissingWindow)
    }

    pub fn window_inventory(&self) -> Vec<(WindowInfo, RecipientId)> {
        self.windows
            .values()
            .map(|value| (value.info.clone(), value.owner))
            .collect()
    }

    pub fn authorize_window_control(
        &self,
        recipient: RecipientId,
        window: WindowId,
        control: &WindowControl,
    ) -> Result<RecipientId, Error> {
        self.window_info(recipient, window)?;
        if let WindowControl::MoveResize(bounds) = control
            && !bounds.valid_region()
        {
            return Err(Error::Invalid("window bounds"));
        }
        let owner = self.window_owner(window)?;
        if owner != recipient
            && !self
                .recipients
                .get(&recipient)
                .and_then(|value| value.window_access.get(&window))
                .is_some_and(|access| access.control)
        {
            return Err(Error::Denied);
        }
        Ok(owner)
    }

    pub fn mark_window_stale(&mut self, window: WindowId) -> Result<(), Error> {
        if self
            .windows
            .get(&window)
            .ok_or(Error::MissingWindow)?
            .info
            .observation
            .stale
        {
            return Ok(());
        }
        self.reserve_events(1)?;
        let value = self.windows.get_mut(&window).ok_or(Error::MissingWindow)?;
        value.info.observation.stale = true;
        let info = value.info.clone();
        self.publish(Event::Window {
            update: WindowUpdate::Changed { window: info },
        });
        Ok(())
    }

    pub fn track_window(
        &mut self,
        owner: RecipientId,
        window: WindowId,
    ) -> Result<WindowInfo, Error> {
        self.reserve_events(1)?;
        let info = self.window_info(owner, window)?;
        self.recipients
            .get_mut(&owner)
            .ok_or(Error::MissingRecipient)?
            .tracked
            .insert(window);
        self.publish(Event::Window {
            update: WindowUpdate::Tracked {
                window: info.clone(),
            },
        });
        Ok(info)
    }

    pub fn untrack_window(&mut self, owner: RecipientId, window: WindowId) -> Result<(), Error> {
        self.reserve_events(1)?;
        self.window_info(owner, window)?;
        // Publish while the recipient still tracks it, then remove the membership.
        self.publish(Event::Window {
            update: WindowUpdate::Untracked { window },
        });
        self.recipients
            .get_mut(&owner)
            .ok_or(Error::MissingRecipient)?
            .tracked
            .remove(&window);
        Ok(())
    }

    pub fn control_window(
        &mut self,
        owner: RecipientId,
        window: WindowId,
        control: WindowControl,
        adapter: &mut impl OwnWindowAdapter,
    ) -> Result<WindowAcknowledgement, Error> {
        self.reserve_events(1)?;
        let current = self.window_info(owner, window)?;
        if self.window_owner(window)? != owner {
            return Err(Error::Denied);
        }
        if let WindowControl::MoveResize(bounds) = &control
            && !bounds.valid_region()
        {
            return Err(Error::Invalid("window bounds"));
        }
        if !adapter.supported(window, control.action()) {
            return Err(Error::Unsupported("window action unavailable in backend"));
        }
        let observed = adapter.apply(owner, &current, &control)?;
        if observed.id != window {
            return Err(Error::Adapter(
                "backend returned a different window token".into(),
            ));
        }
        self.update_owned_window(owner, observed.clone())?;
        Ok(WindowAcknowledgement {
            window,
            action: control.action(),
            observed,
        })
    }

    /// Host-only native observation. Must refer to an existing owned token.
    pub fn update_owned_window(
        &mut self,
        owner: RecipientId,
        observed: WindowInfo,
    ) -> Result<(), Error> {
        self.reserve_events(1)?;
        let value = self
            .windows
            .get_mut(&observed.id)
            .ok_or(Error::MissingWindow)?;
        if value.owner != owner {
            return Err(Error::Denied);
        }
        if !observed.bounds.valid_region() {
            return Err(Error::Invalid("window bounds"));
        }
        observed.metadata.validate().map_err(Error::Invalid)?;
        value.info = observed.clone();
        self.publish(Event::Window {
            update: WindowUpdate::Changed { window: observed },
        });
        Ok(())
    }

    /// Native-client refresh updates receipt freshness without flooding tracked
    /// streams when the only difference is the host receipt timestamp.
    pub fn refresh_reported_window(
        &mut self,
        owner: RecipientId,
        observed: WindowInfo,
    ) -> Result<(), Error> {
        if !observed.bounds.valid_region() {
            return Err(Error::Invalid("window bounds"));
        }
        observed.metadata.validate().map_err(Error::Invalid)?;
        let current = self.windows.get(&observed.id).ok_or(Error::MissingWindow)?;
        if current.owner != owner {
            return Err(Error::Denied);
        }
        let mut comparison = observed.clone();
        comparison.observation.sampled_at_ms = current.info.observation.sampled_at_ms;
        if comparison == current.info {
            self.windows
                .get_mut(&observed.id)
                .ok_or(Error::MissingWindow)?
                .info = observed;
            Ok(())
        } else {
            self.update_owned_window(owner, observed)
        }
    }

    /// Host-only destruction notification. Dropping a track is not destruction.
    pub fn destroy_window(&mut self, window: WindowId) -> Result<(), Error> {
        self.reserve_events(1)?;
        if !self.windows.contains_key(&window) {
            return Err(Error::MissingWindow);
        }
        self.publish(Event::Window {
            update: WindowUpdate::Destroyed { window },
        });
        self.windows.remove(&window);
        for recipient in self.recipients.values_mut() {
            recipient.tracked.remove(&window);
            recipient.window_access.remove(&window);
        }
        Ok(())
    }
}

fn validate_name(name: &str) -> Result<(), Error> {
    if name.is_empty()
        || name.len() > 128
        || name.trim() != name
        || name.chars().any(char::is_control)
    {
        return Err(Error::Invalid(
            "name must be 1-128 bytes without padding or controls",
        ));
    }
    Ok(())
}

fn transition_vec<T: PartialEq>(
    held: &mut Vec<T>,
    item: T,
    transition: Transition,
) -> Result<(), Error> {
    let existing = held.iter().position(|value| *value == item);
    match (transition, existing) {
        (Transition::Down, None) => held.push(item),
        (Transition::Up, Some(index)) => {
            held.remove(index);
        }
        _ => return Err(Error::InvalidTransition),
    }
    Ok(())
}

fn view_for(
    user: &UserView,
    grant: &Grant,
    owner: RecipientId,
    recipient: RecipientId,
) -> Option<UserView> {
    if !scope_allows(&grant.users, user.id, Some(owner), recipient)
        || !(grant.pointer_events || grant.key_events)
    {
        return None;
    }
    let mut view = user.clone();
    view.owned_by_recipient = owner == recipient;
    if !grant.pointer_events {
        view.pointer = None;
        view.cursor = None;
    }
    if !grant.key_events {
        view.held_keys = None;
    }
    Some(view)
}

fn event_for(
    event: &Event,
    recipient: &Recipient,
    recipient_id: RecipientId,
    user_owner: Option<RecipientId>,
) -> Option<Event> {
    let grant = &recipient.grant;
    match event {
        Event::UserCreated { user } => Some(Event::UserCreated {
            user: view_for(user, grant, user_owner?, recipient_id)?,
        }),
        Event::UserUpdated { user } => Some(Event::UserUpdated {
            user: view_for(user, grant, user_owner?, recipient_id)?,
        }),
        Event::UserDisposed { user }
            if scope_allows(&grant.users, *user, user_owner, recipient_id)
                && (grant.pointer_events || grant.key_events) =>
        {
            Some(event.clone())
        }
        Event::Input { user, input, .. }
            if scope_allows(&grant.users, *user, user_owner, recipient_id)
                && if input.is_key() {
                    grant.key_events
                } else {
                    grant.pointer_events
                } =>
        {
            let mut event = event.clone();
            if !grant.pointer_events
                && let Event::Input { position, .. } = &mut event
            {
                *position = None;
            }
            Some(event)
        }
        Event::ScreensChanged { .. } if grant.enabled() => Some(event.clone()),
        Event::Window { update } if grant.owned_windows => {
            let id = match update {
                WindowUpdate::Tracked { window } | WindowUpdate::Changed { window } => window.id,
                WindowUpdate::Untracked { window } | WindowUpdate::Destroyed { window } => *window,
            };
            recipient.tracked.contains(&id).then(|| event.clone())
        }
        _ => None,
    }
}

fn scope_allows(
    scope: &UserScope,
    user: UserId,
    owner: Option<RecipientId>,
    recipient: RecipientId,
) -> bool {
    match scope {
        UserScope::AllOwned => owner == Some(recipient),
        UserScope::Only(users) => owner.is_some() && users.contains(&user),
    }
}
