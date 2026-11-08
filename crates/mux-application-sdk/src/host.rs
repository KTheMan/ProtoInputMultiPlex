use crate::protocol::*;
use mux_core::application_event::*;
use mux_core::desktop::MouseButton;
use std::{collections::BTreeMap, marker::PhantomData, rc::Rc};

#[derive(Clone, Copy, PartialEq, Eq)]
enum HeldControl {
    Button(MouseButton),
    Key(KeyCode),
}
#[derive(Clone, Copy)]
struct Held {
    user: UserId,
    producer: RecipientId,
    control: HeldControl,
}
#[derive(Clone, Copy)]
enum Family {
    Lifecycle,
    Cursor,
    Pointer,
    Key,
}

/// Native implementations must keep native handles private and check same-process
/// UI-thread ownership, including lifetime/reuse validation, on every operation.
pub trait WindowBackend: OwnWindowAdapter + Default {
    fn register(
        &mut self,
        engine: &mut Engine,
        recipient: RecipientId,
        native_window: usize,
    ) -> Result<WindowId, Error>;
    fn refresh(&self, engine: &mut Engine, window: WindowId) -> Result<WindowInfo, Error>;
    fn retire(&mut self, engine: &mut Engine, window: WindowId) -> Result<(), Error>;
    fn notify(&self, window: WindowId) -> Result<(), Error>;
    fn acknowledge(&self, window: WindowId) -> Result<(), Error>;
    fn wake_message(&self) -> Result<u32, Error>;
}

/// Trusted local embedding object. !Send/!Sync, even with an unavailable backend:
/// callers create, use and drop it on one owning UI thread. No workers are spawned.
pub struct Host<A: WindowBackend = crate::PlatformWindows> {
    engine: Engine,
    adapter: A,
    epoch: u64,
    ended: bool,
    controls: BTreeMap<RecipientId, ControlGrant>,
    event_grants: BTreeMap<RecipientId, Grant>,
    user_owners: BTreeMap<UserId, RecipientId>,
    held: Vec<Held>,
    windows: BTreeMap<WindowId, RecipientId>,
    remote_windows: BTreeMap<WindowId, RecipientId>,
    _thread_only: PhantomData<Rc<()>>,
}

impl<A: WindowBackend> Host<A> {
    pub fn new(epoch: u64, screens: ScreenState) -> Result<Self, Error> {
        Ok(Self {
            engine: Engine::new(epoch, screens)?,
            adapter: A::default(),
            epoch,
            ended: false,
            controls: BTreeMap::new(),
            event_grants: BTreeMap::new(),
            user_owners: BTreeMap::new(),
            held: Vec::new(),
            windows: BTreeMap::new(),
            remote_windows: BTreeMap::new(),
            _thread_only: PhantomData,
        })
    }

    pub fn register_recipient(&mut self, identity: &str) -> Result<RecipientId, Error> {
        let recipient = self.engine.register_recipient(identity)?;
        self.controls.insert(recipient, ControlGrant::default());
        self.event_grants.insert(recipient, Grant::default());
        Ok(recipient)
    }

    pub fn approve_events(&mut self, recipient: RecipientId, grant: Grant) -> Result<(), Error> {
        self.engine.recipient_identity(recipient)?;
        if let UserScope::Only(users) = &grant.users
            && (users.len() > MAX_USERS
                || users
                    .iter()
                    .any(|user| !self.user_owners.contains_key(user)))
        {
            return Err(Error::Invalid("event user scope"));
        }
        // Release only inputs produced by this caller whose observation/input scope
        // is being retired; owner revocation may additionally neutralize its users.
        let retiring: Vec<_> = self
            .held
            .iter()
            .filter(|held| {
                held.producer == recipient
                    && !self.event_allows(recipient, held.user, &grant, held.control)
            })
            .copied()
            .collect();
        self.release(retiring)?;
        self.engine.approve_grant(recipient, grant.clone())?;
        self.event_grants.insert(recipient, grant.clone());
        let owners = &self.user_owners;
        self.held.retain(|held| {
            owners.get(&held.user) != Some(&recipient)
                || scope_event(&grant.users, held.user, recipient, recipient)
                    && match held.control {
                        HeldControl::Button(_) => grant.pointer_events,
                        HeldControl::Key(_) => grant.key_events,
                    }
        });
        Ok(())
    }

    /// Host-only control authority, independent from event-export consent.
    /// Restriction releases only holds produced by this bound recipient.
    pub fn approve_controls(
        &mut self,
        recipient: RecipientId,
        grant: ControlGrant,
    ) -> Result<(), Error> {
        self.engine.recipient_identity(recipient)?;
        if let UserScope::Only(users) = &grant.users
            && (users.len() > MAX_USERS
                || users
                    .iter()
                    .any(|user| !self.user_owners.contains_key(user)))
        {
            return Err(Error::Invalid("control user scope"));
        }
        let retiring: Vec<_> = self
            .held
            .iter()
            .filter(|held| {
                let owner = self.user_owners.get(&held.user).copied();
                held.producer == recipient
                    && (owner.is_none_or(|owner| !grant.selects(held.user, recipient, owner))
                        || match held.control {
                            HeldControl::Button(_) => !grant.pointer_input,
                            HeldControl::Key(_) => !grant.key_input,
                        })
            })
            .copied()
            .collect();
        self.release(retiring)?;
        self.controls.insert(recipient, grant);
        Ok(())
    }

    /// Describes actual host approvals after transport authentication. It contains
    /// no credential and does not itself authorize a recipient.
    pub fn ready_for(&self, recipient: RecipientId) -> Result<EventReady, Error> {
        let snapshot = self.engine.snapshot(recipient)?;
        Ok(EventReady {
            version: PROTOCOL_VERSION,
            epoch: self.epoch,
            recipient,
            events: self
                .event_grants
                .get(&recipient)
                .cloned()
                .ok_or(Error::MissingRecipient)?,
            controls: self
                .controls
                .get(&recipient)
                .cloned()
                .ok_or(Error::MissingRecipient)?,
            capabilities: snapshot.capabilities,
            limits: EventLimits::default(),
        })
    }

    /// GUI-hosted remote publication. The bridge validates the current lease and
    /// owner-local token before calling this method. No native handle is accepted.
    pub fn register_remote_window(
        &mut self,
        owner: RecipientId,
        observed: WindowObservation,
    ) -> Result<WindowId, Error> {
        self.version(PROTOCOL_VERSION)?;
        let grant = self
            .event_grants
            .get(&owner)
            .ok_or(Error::MissingRecipient)?;
        let controls = self.controls.get(&owner).ok_or(Error::MissingRecipient)?;
        allowed(grant.owned_windows && controls.window_publication)?;
        let id = self.engine.register_owned_window_with_state(
            owner,
            observed,
            remote_observation_state(),
        )?;
        self.remote_windows.insert(id, owner);
        Ok(id)
    }

    pub fn remote_window_owner(&self, window: WindowId) -> Result<RecipientId, Error> {
        self.remote_windows
            .get(&window)
            .copied()
            .ok_or(Error::MissingWindow)
    }

    pub fn window_inventory(&self) -> Vec<(WindowInfo, RecipientId)> {
        self.engine
            .window_inventory()
            .into_iter()
            .filter(|(window, _)| self.remote_windows.contains_key(&window.id))
            .collect()
    }

    pub fn delegate_window(
        &mut self,
        window: WindowId,
        recipient: RecipientId,
        access: WindowAccess,
    ) -> Result<(), Error> {
        self.remote_window_active_owner(window)?;
        self.engine.delegate_window(window, recipient, access)
    }

    pub fn refresh_remote_window(
        &mut self,
        owner: RecipientId,
        window: WindowId,
        observed: WindowObservation,
    ) -> Result<WindowInfo, Error> {
        allowed(self.remote_window_owner(window)? == owner)?;
        let info = remote_info(window, observed);
        self.engine.refresh_reported_window(owner, info.clone())?;
        Ok(info)
    }

    pub fn retire_remote_window(
        &mut self,
        owner: RecipientId,
        window: WindowId,
    ) -> Result<(), Error> {
        allowed(self.remote_window_owner(window)? == owner)?;
        self.engine.destroy_window(window)?;
        self.remote_windows.remove(&window);
        Ok(())
    }

    pub fn mark_remote_window_stale(&mut self, window: WindowId) -> Result<(), Error> {
        self.remote_window_owner(window)?;
        self.engine.mark_window_stale(window)
    }

    pub fn authorize_remote_window_control(
        &self,
        recipient: RecipientId,
        window: WindowId,
        control: &WindowControl,
    ) -> Result<RecipientId, Error> {
        let owner = self.remote_window_active_owner(window)?;
        let controls = self
            .controls
            .get(&recipient)
            .ok_or(Error::MissingRecipient)?;
        allowed(controls.window_tracking)?;
        allowed(controls.window_control)?;
        allowed(
            self.engine
                .authorize_window_control(recipient, window, control)?
                == owner,
        )?;
        allowed(
            !self
                .engine
                .window_info(recipient, window)?
                .observation
                .stale,
        )?;
        Ok(owner)
    }

    /// Trusted topology observation; no client command can set screen state.
    pub fn update_screens(&mut self, screens: ScreenState) -> Result<(), Error> {
        self.engine.update_screens(screens)
    }

    pub fn create_owned_user(&mut self, owner: RecipientId, name: &str) -> Result<UserId, Error> {
        let user = self.engine.create_user(owner, name)?;
        self.user_owners.insert(user, owner);
        Ok(user)
    }

    fn event_allows(
        &self,
        recipient: RecipientId,
        user: UserId,
        grant: &Grant,
        control: HeldControl,
    ) -> bool {
        self.user_owners
            .get(&user)
            .is_some_and(|owner| scope_event(&grant.users, user, recipient, *owner))
            && match control {
                HeldControl::Button(_) => grant.pointer_events,
                HeldControl::Key(_) => grant.key_events,
            }
    }

    fn authorize_user(
        &self,
        recipient: RecipientId,
        user: UserId,
        family: Family,
    ) -> Result<RecipientId, Error> {
        let owner = *self.user_owners.get(&user).ok_or(Error::MissingUser)?;
        let control = self
            .controls
            .get(&recipient)
            .ok_or(Error::MissingRecipient)?;
        let events = self
            .event_grants
            .get(&recipient)
            .ok_or(Error::MissingRecipient)?;
        let operation = match family {
            Family::Lifecycle => control.user_lifecycle,
            Family::Cursor => control.cursor,
            Family::Pointer => control.pointer_input,
            Family::Key => control.key_input,
        };
        let observation = match family {
            Family::Lifecycle => events.pointer_events || events.key_events,
            Family::Cursor | Family::Pointer => events.pointer_events,
            Family::Key => events.key_events,
        };
        allowed(
            operation
                && observation
                && control.selects(user, recipient, owner)
                && scope_event(&events.users, user, recipient, owner),
        )?;
        Ok(owner)
    }

    fn release(&mut self, retiring: Vec<Held>) -> Result<(), Error> {
        for held in retiring {
            if let Some(owner) = self.user_owners.get(&held.user).copied() {
                let input = match held.control {
                    HeldControl::Button(button) => Input::Button {
                        button,
                        transition: Transition::Up,
                    },
                    HeldControl::Key(key) => Input::Key {
                        key,
                        transition: Transition::Up,
                    },
                };
                match self.engine.submit(owner, held.user, input) {
                    Ok(_) | Err(Error::Denied | Error::MissingUser | Error::InvalidTransition) => {}
                    Err(error) => {
                        self.end();
                        return Err(error);
                    }
                }
            }
            self.held.retain(|current| {
                !(current.user == held.user
                    && current.producer == held.producer
                    && current.control == held.control)
            });
        }
        Ok(())
    }

    fn submit(&mut self, recipient: RecipientId, user: UserId, input: Input) -> Result<u64, Error> {
        let owner = self.authorize_user(
            recipient,
            user,
            if input.is_key() {
                Family::Key
            } else {
                Family::Pointer
            },
        )?;
        let transition = match input {
            Input::Button { button, transition } => Some((HeldControl::Button(button), transition)),
            Input::Key { key, transition } => Some((HeldControl::Key(key), transition)),
            _ => None,
        };
        if let Some((control, Transition::Up)) = transition {
            let held = self
                .held
                .iter()
                .find(|held| held.user == user && held.control == control)
                .ok_or(Error::InvalidTransition)?;
            allowed(held.producer == recipient)?;
        }
        let sequence = self.engine.submit(owner, user, input)?;
        if let Some((control, transition)) = transition {
            match transition {
                Transition::Down => self.held.push(Held {
                    user,
                    producer: recipient,
                    control,
                }),
                Transition::Up => self
                    .held
                    .retain(|held| !(held.user == user && held.control == control)),
            }
        }
        Ok(sequence)
    }
    /// This entry point receives an already authenticated, server-bound recipient.
    /// A remote payload must never be allowed to choose this parameter.
    pub fn dispatch(
        &mut self,
        recipient: RecipientId,
        request: ClientRequest,
    ) -> Result<ClientResponse, Failure> {
        self.dispatch_inner(recipient, request).map_err(Into::into)
    }

    fn dispatch_inner(
        &mut self,
        recipient: RecipientId,
        request: ClientRequest,
    ) -> Result<ClientResponse, Error> {
        if request.version != PROTOCOL_VERSION {
            return Err(Error::Unsupported("application protocol version"));
        }
        let subscription_cleanup = matches!(
            &request.command,
            ClientCommand::Poll { .. }
                | ClientCommand::Stats { .. }
                | ClientCommand::Unsubscribe { .. }
        );
        if !subscription_cleanup {
            self.version(request.version)?;
            self.engine.recipient_identity(recipient)?;
        }
        // Old bound recipients may only drain/retire their existing subscription
        // tokens after reset/disconnect/end. Core token ownership checks still apply.
        let grant = self.controls.get(&recipient).cloned().unwrap_or_default();
        use ClientCommand::*;
        Ok(match request.command {
            Snapshot => ClientResponse::Snapshot {
                snapshot: self.engine.snapshot(recipient)?,
            },
            Subscribe { capacity } => {
                let (subscription, snapshot) = self.engine.subscribe(recipient, capacity)?;
                ClientResponse::Subscribed {
                    subscription,
                    snapshot,
                }
            }
            Poll {
                subscription,
                max_events,
            } => {
                if max_events == 0 || max_events > MAX_POLL_EVENTS {
                    return Err(Error::Limit);
                }
                ClientResponse::Polled {
                    poll: self.engine.poll(recipient, subscription, max_events)?,
                }
            }
            Stats { subscription } => ClientResponse::Stats {
                stats: self.engine.subscriber_stats(recipient, subscription)?,
            },
            Resubscribe {
                subscription,
                capacity,
            } => {
                let (subscription, snapshot) =
                    self.engine.resubscribe(recipient, subscription, capacity)?;
                ClientResponse::Subscribed {
                    subscription,
                    snapshot,
                }
            }
            Unsubscribe { subscription } => {
                self.engine.unsubscribe(recipient, subscription)?;
                ClientResponse::Acknowledged
            }
            CreateUser { name } => {
                allowed(grant.user_lifecycle)?;
                ClientResponse::UserCreated {
                    user: self.create_owned_user(recipient, &name)?,
                }
            }
            RenameUser { user, name } => {
                let owner = self.authorize_user(recipient, user, Family::Lifecycle)?;
                self.engine.rename_user(owner, user, &name)?;
                ClientResponse::Acknowledged
            }
            DisposeUser { user } => {
                let owner = self.authorize_user(recipient, user, Family::Lifecycle)?;
                self.engine.dispose_user(owner, user)?;
                self.user_owners.remove(&user);
                self.held.retain(|held| held.user != user);
                ClientResponse::Acknowledged
            }
            Cursor { user, control } => {
                let owner = self.authorize_user(recipient, user, Family::Cursor)?;
                ClientResponse::Cursor {
                    cursor: self.engine.cursor_control(owner, user, control)?,
                }
            }
            Input { user, input } => ClientResponse::InputAccepted {
                sequences: vec![self.submit(recipient, user, input)?],
            },
            InputAtRevision {
                user,
                input,
                transform_revision,
            } => {
                if self.engine.snapshot(recipient)?.screens.revision != transform_revision {
                    return Err(Error::StaleRevision);
                }
                ClientResponse::InputAccepted {
                    sequences: vec![self.submit(recipient, user, input)?],
                }
            }
            Click { user, button } => {
                let owner = self.authorize_user(recipient, user, Family::Pointer)?;
                ClientResponse::InputAccepted {
                    sequences: self.engine.click(owner, user, button)?.to_vec(),
                }
            }
            WindowQuery { window } => {
                allowed(!self.remote_windows.contains_key(&window))?;
                ClientResponse::Window {
                    window: self.engine.window_info(recipient, window)?,
                }
            }
            WindowTrack { window } => {
                allowed(!self.remote_windows.contains_key(&window))?;
                allowed(grant.window_tracking)?;
                ClientResponse::Window {
                    window: self.engine.track_window(recipient, window)?,
                }
            }
            WindowUntrack { window } => {
                allowed(!self.remote_windows.contains_key(&window))?;
                allowed(grant.window_tracking)?;
                self.engine.untrack_window(recipient, window)?;
                ClientResponse::Acknowledged
            }
            WindowControl { window, control } => {
                allowed(grant.window_control)?;
                if self.remote_windows.contains_key(&window) {
                    return Err(Error::Unsupported(
                        "remote window control requires asynchronous owner receipt",
                    ));
                }
                ClientResponse::WindowControlled {
                    acknowledgement: self.engine.control_window(
                        recipient,
                        window,
                        control,
                        &mut self.adapter,
                    )?,
                }
            }
            RemoteWindowQuery { reference } => {
                self.validate_remote_reference(reference)?;
                ClientResponse::Window {
                    window: self.engine.window_info(recipient, reference.window)?,
                }
            }
            RemoteWindowTrack { reference } => {
                self.validate_remote_reference(reference)?;
                allowed(grant.window_tracking)?;
                ClientResponse::Window {
                    window: self.engine.track_window(recipient, reference.window)?,
                }
            }
            RemoteWindowUntrack { reference } => {
                self.validate_remote_reference(reference)?;
                allowed(grant.window_tracking)?;
                self.engine.untrack_window(recipient, reference.window)?;
                ClientResponse::Acknowledged
            }
            RemoteWindowControl { reference, .. } => {
                self.validate_remote_reference(reference)?;
                return Err(Error::Unsupported(
                    "remote window control requires asynchronous owner receipt",
                ));
            }
            Disconnect => {
                self.disconnect(recipient)?;
                ClientResponse::Disconnected
            }
        })
    }

    /// Trusted embedding setup, not part of ClientCommand or browser dispatch.
    pub fn host_dispatch(&mut self, request: HostRequest) -> Result<HostResponse, Failure> {
        self.host_dispatch_inner(request).map_err(Into::into)
    }

    fn host_dispatch_inner(&mut self, request: HostRequest) -> Result<HostResponse, Error> {
        self.version(request.version)?;
        use HostCommand::*;
        Ok(match request.command {
            RegisterRecipient { local_identity } => HostResponse::RecipientRegistered {
                recipient: self.register_recipient(&local_identity)?,
            },
            CreateUser { owner, name } => HostResponse::UserCreated {
                user: self.create_owned_user(owner, &name)?,
            },
            ApproveEvents { recipient, grant } => {
                self.approve_events(recipient, grant)?;
                HostResponse::Acknowledged
            }
            ApproveControls { recipient, grant } => {
                self.approve_controls(recipient, grant)?;
                HostResponse::Acknowledged
            }
            DelegateWindow {
                window,
                recipient,
                access,
            } => {
                self.delegate_window(window, recipient, access)?;
                HostResponse::Acknowledged
            }
            RegisterOwnWindow {
                recipient,
                native_window,
            } => {
                self.engine.recipient_identity(recipient)?;
                let window = self
                    .adapter
                    .register(&mut self.engine, recipient, native_window)?;
                self.windows.insert(window, recipient);
                HostResponse::WindowRegistered {
                    window,
                    wake_message: self.adapter.wake_message()?,
                }
            }
            RefreshOwnWindow { window } => {
                self.known_window(window)?;
                HostResponse::Window {
                    window: self.adapter.refresh(&mut self.engine, window)?,
                }
            }
            RetireOwnWindow { window } => {
                self.known_window(window)?;
                self.adapter.retire(&mut self.engine, window)?;
                self.windows.remove(&window);
                HostResponse::Acknowledged
            }
            NotifyOwnWindow { window } => {
                self.known_window(window)?;
                self.adapter.notify(window)?;
                HostResponse::Acknowledged
            }
            AcknowledgeWake { window } => {
                self.known_window(window)?;
                self.adapter.acknowledge(window)?;
                HostResponse::Acknowledged
            }
            UpdateScreens { screens } => {
                self.update_screens(screens)?;
                HostResponse::Acknowledged
            }
            Reset { epoch, screens } => {
                self.reset(epoch, screens)?;
                HostResponse::Acknowledged
            }
            End => {
                self.end();
                HostResponse::Acknowledged
            }
        })
    }

    fn version(&self, version: u32) -> Result<(), Error> {
        if self.ended {
            return Err(Error::Ended);
        }
        if version != PROTOCOL_VERSION {
            return Err(Error::Unsupported("application protocol version"));
        }
        Ok(())
    }

    fn known_window(&self, window: WindowId) -> Result<(), Error> {
        if self.windows.contains_key(&window) {
            Ok(())
        } else {
            Err(Error::MissingWindow)
        }
    }

    fn validate_remote_reference(&self, reference: RemoteWindowRef) -> Result<(), Error> {
        if reference.epoch != self.epoch {
            return Err(Error::StaleRevision);
        }
        self.remote_window_active_owner(reference.window)?;
        Ok(())
    }

    fn remote_window_active_owner(&self, window: WindowId) -> Result<RecipientId, Error> {
        let owner = self.remote_window_owner(window)?;
        let events = self
            .event_grants
            .get(&owner)
            .ok_or(Error::MissingRecipient)?;
        let controls = self.controls.get(&owner).ok_or(Error::MissingRecipient)?;
        allowed(events.owned_windows && controls.window_publication)?;
        Ok(owner)
    }

    fn disconnect(&mut self, recipient: RecipientId) -> Result<(), Error> {
        let retiring: Vec<_> = self
            .held
            .iter()
            .filter(|held| held.producer == recipient)
            .copied()
            .collect();
        self.release(retiring)?;
        let windows: Vec<_> = self
            .windows
            .iter()
            .filter_map(|(window, owner)| (*owner == recipient).then_some(*window))
            .collect();
        let mut cleanup_error = None;
        for window in windows {
            if let Err(error) = self.adapter.retire(&mut self.engine, window) {
                cleanup_error = Some(error);
            }
            self.windows.remove(&window);
        }
        let remote: Vec<_> = self
            .remote_windows
            .iter()
            .filter_map(|(window, owner)| (*owner == recipient).then_some(*window))
            .collect();
        for window in remote {
            self.engine.destroy_window(window)?;
            self.remote_windows.remove(&window);
        }
        self.engine.disconnect_recipient(recipient)?;
        self.controls.remove(&recipient);
        self.event_grants.remove(&recipient);
        self.user_owners.retain(|_, owner| *owner != recipient);
        self.held
            .retain(|held| self.user_owners.contains_key(&held.user));
        if let Some(error) = cleanup_error {
            self.end(); // Retire all native bindings if one could not be safely removed.
            return Err(error);
        }
        Ok(())
    }

    pub fn reset(&mut self, epoch: u64, screens: ScreenState) -> Result<(), Error> {
        self.version(PROTOCOL_VERSION)?;
        screens.validate()?;
        if epoch == 0 || epoch == self.epoch {
            return Err(Error::Invalid("fresh epoch required"));
        }
        self.engine.reset(epoch, screens)?;
        self.adapter = A::default();
        self.windows.clear();
        self.remote_windows.clear();
        self.controls.clear();
        self.event_grants.clear();
        self.user_owners.clear();
        self.held.clear();
        self.epoch = epoch;
        Ok(())
    }

    pub fn end(&mut self) {
        self.engine.end();
        self.adapter = A::default();
        self.windows.clear();
        self.remote_windows.clear();
        self.controls.clear();
        self.event_grants.clear();
        self.user_owners.clear();
        self.held.clear();
        self.ended = true;
    }
}

fn remote_observation_state() -> WindowObservationState {
    let sampled_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok());
    WindowObservationState {
        source: WindowObservationSource::NativeClientReported,
        sampled_at_ms,
        stale: false,
    }
}

fn remote_info(id: WindowId, observed: WindowObservation) -> WindowInfo {
    WindowInfo {
        id,
        bounds: observed.bounds,
        metadata: observed.metadata,
        observation: remote_observation_state(),
        visible: observed.visible,
        minimized: observed.minimized,
        maximized: observed.maximized,
        click_through: observed.click_through,
        enabled: observed.enabled,
        topmost: observed.topmost,
        opacity: observed.opacity,
    }
}

impl<A: WindowBackend> Drop for Host<A> {
    fn drop(&mut self) {
        self.engine.end();
    }
}
fn allowed(allow: bool) -> Result<(), Error> {
    if allow { Ok(()) } else { Err(Error::Denied) }
}

fn scope_event(
    scope: &UserScope,
    user: UserId,
    recipient: RecipientId,
    owner: RecipientId,
) -> bool {
    match scope {
        UserScope::AllOwned => recipient == owner,
        UserScope::Only(users) => users.contains(&user),
    }
}
