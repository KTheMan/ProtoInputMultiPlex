//! Explicit virtual-only application session, owned by the GUI event thread.
mod actor;
use crate::application_event_transport::{ClientPermit, HostBridge, NetworkEndpoint};
use anyhow::{Context, Result, ensure};
use eframe::egui;
use mux_application_sdk::*;
use mux_core::desktop::Bounds;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

trait SdkResult<T> {
    fn sdk(self) -> Result<T>;
}
impl<T> SdkResult<T> for std::result::Result<T, Failure> {
    fn sdk(self) -> Result<T> {
        self.map_err(|error| anyhow::anyhow!("Cooperative SDK request rejected ({:?})", error.code))
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PublicStatus {
    pub phase: String,
    pub session_active: bool,
    pub local_virtual_users: usize,
    pub native_observed_users: usize,
    pub native_sequence: u64,
    pub pointer_export: bool,
    pub keyboard_export: bool,
    pub browser_approved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connected_clients: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_pending: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertain_admissions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_overflows: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_sample_age_ms: Option<u64>,
    pub retired: bool,
    pub sample_age_ms: u64,
}

impl PublicStatus {
    /// Publication can age a worker sample, but can never make it fresh again.
    pub(crate) fn age_by(&mut self, elapsed: Duration) {
        let elapsed = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        self.sample_age_ms = self.sample_age_ms.saturating_add(elapsed);
        self.transport_sample_age_ms = self
            .transport_sample_age_ms
            .map(|age| age.saturating_add(elapsed));
        if self.transport_sample_age_ms.is_none_or(|age| age > 2_000) {
            self.connected_clients = None;
            self.cleanup_pending = None;
            self.uncertain_admissions = None;
            self.queue_overflows = None;
        }
    }
}

#[derive(Default)]
pub(crate) struct Draft {
    pub name: String,
    pub origin: String,
    pub ttl: String,
    pub users: BTreeSet<UserId>,
    pub focus: Option<UserId>,
    pub pointer_events: bool,
    pub key_events: bool,
    pub delegated: bool,
    pub lifecycle: bool,
    pub cursor: bool,
    pub pointer_control: bool,
    pub key_control: bool,
}
impl Draft {
    pub fn apply_script_fields(&mut self, fields: &BTreeMap<String, String>, editing_origin: bool) {
        if let Some(name) = fields
            .get("application_event_user_name")
            .filter(|value| value.len() <= 128)
        {
            self.name.clone_from(name);
        }
        if editing_origin
            && let Some(origin) = fields
                .get("application_event_origin")
                .filter(|value| value.len() <= 4096)
        {
            self.origin.clone_from(origin);
        }
        if let Some(ttl) = fields
            .get("application_event_ttl")
            .filter(|value| value.len() <= 3)
        {
            self.ttl.clone_from(ttl);
        }
        // Consent and user scope change only through separately observed controls.
    }
    fn approvals(&self) -> Result<(Grant, ControlGrant)> {
        ensure!(
            !self.users.is_empty() && self.users.len() <= MAX_USERS,
            "Select the exact cooperative users first"
        );
        ensure!(
            self.pointer_events || self.key_events,
            "Explicitly approve pointer or keyboard observation first"
        );
        ensure!(
            !self.key_control || self.key_events,
            "Keyboard controls require separate keyboard observation approval"
        );
        ensure!(
            !(self.pointer_control || self.cursor) || self.pointer_events,
            "Pointer/cursor controls require pointer observation approval"
        );
        let scope = UserScope::Only(self.users.clone());
        Ok((
            Grant {
                users: scope.clone(),
                pointer_events: self.pointer_events,
                key_events: self.key_events,
                owned_windows: false,
            },
            ControlGrant {
                users: scope,
                delegated_users: self.delegated,
                user_lifecycle: self.lifecycle,
                cursor: self.cursor,
                pointer_input: self.pointer_control,
                key_input: self.key_control,
                window_tracking: false,
                window_control: false,
            },
        ))
    }
}

pub(crate) struct LocalUser {
    pub id: UserId,
    pub name: String,
}
pub(crate) struct Controller {
    bridge: Option<HostBridge>,
    owner: Option<RecipientId>,
    receiver: Option<RecipientId>,
    permit: Option<ClientPermit>,
    actor: Option<actor::Actor>,
    native_subscription: Option<SubscriptionId>,
    pending_endpoint: Option<NetworkEndpoint>,
    pub native: ClientState,
    pub users: Vec<LocalUser>,
    pub draft: Draft,
    pub error: Option<String>,
    epoch: u64,
    ended: bool,
    copy_pending: bool,
}
impl Default for Controller {
    fn default() -> Self {
        Self {
            bridge: None,
            owner: None,
            receiver: None,
            permit: None,
            actor: None,
            native_subscription: None,
            pending_endpoint: None,
            native: ClientState::default(),
            users: Vec::new(),
            draft: Draft {
                name: "Virtual user".into(),
                ttl: "120".into(),
                ..Draft::default()
            },
            error: None,
            epoch: 0,
            ended: false,
            copy_pending: false,
        }
    }
}
impl Controller {
    pub fn active(&self) -> bool {
        self.bridge.is_some() && !self.ended
    }
    pub fn listener_active(&self) -> bool {
        self.actor.as_ref().is_some_and(actor::Actor::active)
    }
    pub fn can_start_listener(&self) -> bool {
        self.active() && self.actor.is_none() && self.pending_endpoint.is_some()
    }
    pub fn can_issue(&self) -> bool {
        self.listener_active() && self.receiver.is_some() && self.permit.is_none()
    }
    pub fn connection_issued(&self) -> bool {
        self.permit.as_ref().is_some_and(|permit| {
            self.actor
                .as_ref()
                .is_some_and(|actor| actor.snapshot().issued == Some(permit.id()))
        })
    }
    pub fn origin_editable(&self) -> bool {
        self.actor.as_ref().is_none_or(actor::Actor::retired)
    }
    pub fn retired(&self) -> bool {
        (!self.active()) && self.actor.as_ref().is_none_or(actor::Actor::retired)
    }
    pub fn status(&self) -> PublicStatus {
        let actor = self.actor.as_ref().map(actor::Actor::snapshot);
        let network = actor.as_ref().and_then(|state| state.service.as_ref());
        let owner = self.bridge.as_ref().and_then(|bridge| {
            self.owner
                .and_then(|owner| bridge.host().ready_for(owner).ok())
        });
        let mut status = PublicStatus {
            phase: actor.as_ref().map_or_else(
                || {
                    if self.active() {
                        "local".into()
                    } else {
                        "off".into()
                    }
                },
                |state| state.phase.clone(),
            ),
            session_active: self.active(),
            local_virtual_users: self.users.len(),
            native_observed_users: self.native.users().len(),
            native_sequence: self.native.last_sequence(),
            pointer_export: owner
                .as_ref()
                .is_some_and(|ready| ready.events.pointer_events),
            keyboard_export: owner.as_ref().is_some_and(|ready| ready.events.key_events),
            browser_approved: self.receiver.is_some(),
            connected_clients: network.map(|state| state.connected_clients),
            cleanup_pending: network.map(|state| state.cleanup_pending),
            uncertain_admissions: network.map(|state| state.uncertain_admissions),
            queue_overflows: network.map(|state| state.queue_overflows),
            transport_sample_age_ms: actor
                .as_ref()
                .and_then(|state| state.service_sampled_at)
                .map(|sampled| u64::try_from(sampled.elapsed().as_millis()).unwrap_or(u64::MAX)),
            retired: self.retired(),
            sample_age_ms: 0,
        };
        status.age_by(Duration::ZERO);
        status
    }
    pub fn create_session(&mut self, bounds: Bounds) -> Result<()> {
        ensure!(
            self.retired(),
            "Finish the previous application-event cleanup first"
        );
        let token = crate::gui_bridge::random_token()?;
        let epoch = u64::from_str_radix(&token[..16], 16).context("create cooperative epoch")?;
        ensure!(epoch != 0, "Could not create a fresh cooperative epoch");
        let mut host = Host::<PlatformWindows>::new(
            epoch,
            ScreenState {
                revision: 1,
                desktop: bounds,
                screens: vec![bounds],
            },
        )?;
        let owner = host.register_recipient("GUI cooperative native consumer")?;
        let (bridge, endpoint) = HostBridge::new(host);
        // Retain the endpoint until an explicit network action. Creating a session
        // performs no network work, no grants and no virtual-user creation.
        self.pending_endpoint = Some(endpoint);
        self.bridge = Some(bridge);
        self.owner = Some(owner);
        self.receiver = None;
        self.permit = None;
        self.actor = None;
        self.epoch = epoch;
        self.ended = false;
        self.native = ClientState::default();
        self.users.clear();
        self.draft.users.clear();
        self.draft.focus = None;
        self.error = None;
        Ok(())
    }
    pub fn create_user(&mut self) -> Result<()> {
        ensure!(self.active(), "Create a cooperative session first");
        let owner = self.owner.context("Native consumer unavailable")?;
        let response = self
            .bridge
            .as_mut()
            .context("Cooperative session unavailable")?
            .host_command(HostRequest {
                version: PROTOCOL_VERSION,
                command: HostCommand::CreateUser {
                    owner,
                    name: self.draft.name.clone(),
                },
            })
            .sdk()?;
        let HostResponse::UserCreated { user } = response else {
            anyhow::bail!("Unexpected user-creation reply");
        };
        self.users.push(LocalUser {
            id: user,
            name: self.draft.name.clone(),
        });
        Ok(())
    }
    pub fn approve(&mut self) -> Result<()> {
        ensure!(self.active(), "Create a cooperative session first");
        let (events, controls) = self.draft.approvals()?;
        ensure!(
            self.draft
                .users
                .iter()
                .all(|id| self.users.iter().any(|user| user.id == *id)),
            "User selection is stale"
        );
        self.revoke();
        let owner = self.owner.context("Native consumer unavailable")?;
        let bridge = self
            .bridge
            .as_mut()
            .context("Cooperative session unavailable")?;
        if let Some(subscription) = self.native_subscription.take() {
            let _ = bridge.dispatch_local(
                owner,
                ClientRequest {
                    version: PROTOCOL_VERSION,
                    command: ClientCommand::Unsubscribe { subscription },
                },
            );
        }
        self.native.disconnect();
        bridge
            .host_command(HostRequest {
                version: PROTOCOL_VERSION,
                command: HostCommand::ApproveEvents {
                    recipient: owner,
                    grant: events.clone(),
                },
            })
            .sdk()?;
        let mut native_controls = controls.clone();
        native_controls.delegated_users = false;
        bridge
            .host_command(HostRequest {
                version: PROTOCOL_VERSION,
                command: HostCommand::ApproveControls {
                    recipient: owner,
                    grant: native_controls,
                },
            })
            .sdk()?;
        let ClientResponse::Subscribed {
            subscription,
            snapshot,
        } = bridge
            .dispatch_local(
                owner,
                ClientRequest {
                    version: PROTOCOL_VERSION,
                    command: ClientCommand::Subscribe { capacity: 256 },
                },
            )
            .sdk()?
        else {
            anyhow::bail!("Native snapshot unavailable");
        };
        self.native.install_snapshot(snapshot, self.epoch)?;
        self.native_subscription = Some(subscription);
        let HostResponse::RecipientRegistered { recipient } = bridge
            .host_command(HostRequest {
                version: PROTOCOL_VERSION,
                command: HostCommand::RegisterRecipient {
                    local_identity: "GUI-approved browser event receiver".into(),
                },
            })
            .sdk()?
        else {
            anyhow::bail!("Browser receiver unavailable");
        };
        self.receiver = Some(recipient);
        bridge
            .host_command(HostRequest {
                version: PROTOCOL_VERSION,
                command: HostCommand::ApproveEvents {
                    recipient,
                    grant: events,
                },
            })
            .sdk()?;
        bridge
            .host_command(HostRequest {
                version: PROTOCOL_VERSION,
                command: HostCommand::ApproveControls {
                    recipient,
                    grant: controls,
                },
            })
            .sdk()?;
        self.receiver = Some(recipient);
        Ok(())
    }
    pub fn start_listener(
        &mut self,
        root: PathBuf,
        guards: Arc<crate::gui::RuntimeGuards>,
    ) -> Result<()> {
        ensure!(self.active(), "Create a cooperative session first");
        ensure!(
            self.actor.is_none(),
            "This cooperative listener is already started; end the session before restarting"
        );
        crate::sdk_listener::validate_origins(std::slice::from_ref(&self.draft.origin))?;
        ensure!(
            !self.draft.origin.is_empty(),
            "Enter one exact HTTP(S) origin first"
        );
        let endpoint = self
            .pending_endpoint
            .take()
            .context("Event network endpoint is unavailable")?;
        self.actor = Some(actor::Actor::start(
            endpoint,
            self.draft.origin.clone(),
            root,
            self.epoch,
            guards,
        )?);
        Ok(())
    }
    pub fn issue(&mut self) -> Result<()> {
        ensure!(
            self.permit.is_none(),
            "Approve a fresh receiver before issuing another connection"
        );
        let seconds: u64 = self
            .draft
            .ttl
            .parse()
            .context("TTL must be 1 through 300 seconds")?;
        ensure!(
            (1..=300).contains(&seconds),
            "TTL must be 1 through 300 seconds"
        );
        let actor = self
            .actor
            .as_ref()
            .filter(|actor| actor.active())
            .context("Start the listener first")?;
        let receiver = self
            .receiver
            .context("Explicitly approve the browser scope first")?;
        let permit = self
            .bridge
            .as_mut()
            .context("Cooperative session unavailable")?
            .approve_client(receiver, &self.draft.origin, Duration::from_secs(seconds))?;
        if let Err(error) = actor.issue(permit.clone()) {
            self.bridge.as_mut().unwrap().revoke_recipient(receiver);
            self.receiver = None;
            return Err(error);
        }
        self.permit = Some(permit);
        Ok(())
    }
    pub fn request_copy(&mut self) -> Result<()> {
        self.actor
            .as_ref()
            .context("Event listener unavailable")?
            .copy(
                self.permit
                    .clone()
                    .context("Issue an event-only connection first")?,
            )?;
        self.copy_pending = true;
        Ok(())
    }
    pub fn can_control(&self, keys: bool, cursor: bool) -> bool {
        let Some(user) = self.draft.focus else {
            return false;
        };
        if self.native.state() != &StreamState::Live || !self.native.users().contains_key(&user) {
            return false;
        }
        let Some(ready) = self.bridge.as_ref().and_then(|bridge| {
            self.owner
                .and_then(|owner| bridge.host().ready_for(owner).ok())
        }) else {
            return false;
        };
        if keys {
            ready.events.key_events && ready.controls.key_input
        } else if cursor {
            ready.events.pointer_events && ready.controls.cursor
        } else {
            ready.events.pointer_events && ready.controls.pointer_input
        }
    }
    pub fn local_command(&mut self, command: ClientCommand) -> Result<()> {
        ensure!(self.active(), "Cooperative session is not active");
        let owner = self.owner.context("Native consumer unavailable")?;
        self.bridge
            .as_mut()
            .context("Cooperative session unavailable")?
            .dispatch_local(
                owner,
                ClientRequest {
                    version: PROTOCOL_VERSION,
                    command,
                },
            )
            .sdk()?;
        Ok(())
    }
    pub fn move_focused(&mut self) -> Result<()> {
        ensure!(
            self.can_control(false, false),
            "Pointer control is not approved for this user"
        );
        let user = self.draft.focus.context("Select the input user first")?;
        let position = self
            .native
            .users()
            .get(&user)
            .and_then(|view| view.pointer.as_ref())
            .context("Pointer state unavailable")?
            .position;
        let screens = self.native.screens().context("Screen state unavailable")?;
        self.local_command(ClientCommand::InputAtRevision {
            user,
            input: Input::Motion {
                position: Point {
                    x: position.x.saturating_add(80).min(screens.desktop.right - 1),
                    y: position
                        .y
                        .saturating_add(40)
                        .min(screens.desktop.bottom - 1),
                },
            },
            transform_revision: screens.revision,
        })
    }
    pub fn key_pulse(&mut self) -> Result<()> {
        ensure!(
            self.can_control(true, false),
            "Separate keyboard observation and controls are required"
        );
        let user = self.draft.focus.context("Select the input user first")?;
        self.local_command(ClientCommand::Input {
            user,
            input: Input::Key {
                key: KeyCode(4),
                transition: Transition::Down,
            },
        })?;
        if let Err(error) = self.local_command(ClientCommand::Input {
            user,
            input: Input::Key {
                key: KeyCode(4),
                transition: Transition::Up,
            },
        }) {
            self.stop();
            return Err(error);
        }
        Ok(())
    }
    pub fn take_copy(&mut self) -> Option<String> {
        if !self.copy_pending {
            return None;
        }
        let permit = self.permit.as_ref()?;
        // Ready verifies current authority before a pending private Copy result can
        // leave Rust memory. Revocation/replacement clears the matching permit.
        self.bridge
            .as_ref()?
            .host()
            .ready_for(self.receiver?)
            .ok()?;
        let copied = self.actor.as_ref()?.take_copy(permit.id());
        if copied.is_some() {
            self.copy_pending = false;
        }
        copied
    }
    pub fn revoke(&mut self) {
        self.copy_pending = false;
        if let Some(actor) = &self.actor {
            actor.clear_copy();
        }
        if let Some(receiver) = self.receiver.take()
            && let Some(bridge) = &mut self.bridge
        {
            bridge.revoke_recipient(receiver);
            // An approved receiver without an issued permit still belongs to
            // this explicit UI setup and must not accumulate on replacement.
            let _ = bridge.dispatch_local(
                receiver,
                ClientRequest {
                    version: PROTOCOL_VERSION,
                    command: ClientCommand::Disconnect,
                },
            );
        }
        self.permit = None;
    }
    pub fn disable_publication(&mut self) -> Result<()> {
        self.revoke();
        if let (Some(owner), Some(bridge)) = (self.owner, &mut self.bridge) {
            if let Some(subscription) = self.native_subscription.take() {
                let _ = bridge.dispatch_local(
                    owner,
                    ClientRequest {
                        version: PROTOCOL_VERSION,
                        command: ClientCommand::Unsubscribe { subscription },
                    },
                );
            }
            bridge
                .host_command(HostRequest {
                    version: PROTOCOL_VERSION,
                    command: HostCommand::ApproveEvents {
                        recipient: owner,
                        grant: Grant::default(),
                    },
                })
                .sdk()?;
            bridge
                .host_command(HostRequest {
                    version: PROTOCOL_VERSION,
                    command: HostCommand::ApproveControls {
                        recipient: owner,
                        grant: ControlGrant::default(),
                    },
                })
                .sdk()?;
        }
        self.native.disconnect();
        Ok(())
    }
    pub fn stop(&mut self) {
        self.revoke();
        if let Some(bridge) = &mut self.bridge {
            if let (Some(owner), Some(subscription)) = (self.owner, self.native_subscription.take())
            {
                let _ = bridge.dispatch_local(
                    owner,
                    ClientRequest {
                        version: PROTOCOL_VERSION,
                        command: ClientCommand::Unsubscribe { subscription },
                    },
                );
            }
            bridge.stop();
        }
        if let Some(actor) = &self.actor {
            actor.request_stop();
        }
        self.native.disconnect();
        self.native_subscription = None;
        self.pending_endpoint = None;
        self.users.clear();
        self.draft.users.clear();
        self.ended = true;
    }
    pub fn tick(&mut self, ctx: &egui::Context) {
        let mut receiver_missing = false;
        if let Some(bridge) = &mut self.bridge {
            let _report: crate::application_event_transport::TickReport = bridge.drain();
            receiver_missing = self
                .receiver
                .is_some_and(|receiver| bridge.host().ready_for(receiver).is_err());
            if self.native.state() == &StreamState::Live
                && let (Some(owner), Some(subscription)) = (self.owner, self.native_subscription)
            {
                match bridge.dispatch_local(
                    owner,
                    ClientRequest {
                        version: PROTOCOL_VERSION,
                        command: ClientCommand::Poll {
                            subscription,
                            max_events: MAX_POLL_EVENTS,
                        },
                    },
                ) {
                    Ok(ClientResponse::Polled { poll }) => {
                        let lifecycle = match &poll {
                            Poll::Events { events } => events
                                .iter()
                                .filter_map(|envelope| match &envelope.event {
                                    Event::UserDisposed { user } => Some((*user, None)),
                                    Event::UserUpdated { user } => {
                                        Some((user.id, Some(user.name.clone())))
                                    }
                                    _ => None,
                                })
                                .collect::<Vec<_>>(),
                            _ => Vec::new(),
                        };
                        match self.native.fold_poll(poll) {
                            Ok(FoldOutcome::Applied(_)) => {
                                for (id, name) in lifecycle {
                                    if let Some(name) = name {
                                        if let Some(user) =
                                            self.users.iter_mut().find(|user| user.id == id)
                                        {
                                            user.name = name;
                                        }
                                    } else {
                                        self.users.retain(|user| user.id != id);
                                        self.draft.users.remove(&id);
                                        if self.draft.focus == Some(id) {
                                            self.draft.focus = None;
                                        }
                                    }
                                }
                            }
                            Ok(FoldOutcome::Ended(_)) => {
                                self.native_subscription = None;
                                self.error = Some("Native event stream ended; apply explicit approvals for a fresh snapshot.".into());
                            }
                            _ => {
                                self.error = Some("Native event stream needs a fresh snapshot; apply explicit approvals.".into());
                            }
                        }
                    }
                    _ => {
                        self.native.disconnect();
                        self.native_subscription = None;
                    }
                }
            }
        }
        if receiver_missing {
            self.revoke();
        }
        if let Some(actor) = &self.actor
            && let Some(error) = actor.snapshot().error
        {
            self.error = Some(error);
        }
        if self.active() || !self.retired() {
            ctx.request_repaint_after(Duration::from_millis(10));
        }
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn local() -> Controller {
        let bounds = Bounds {
            left: -100,
            top: 0,
            right: 1000,
            bottom: 800,
        };
        let mut host = Host::<PlatformWindows>::new(
            71,
            ScreenState {
                revision: 1,
                desktop: bounds,
                screens: vec![bounds],
            },
        )
        .unwrap();
        let owner = host.register_recipient("native test consumer").unwrap();
        let (bridge, endpoint) = HostBridge::new(host);
        let mut controller = Controller::default();
        controller.bridge = Some(bridge);
        controller.pending_endpoint = Some(endpoint);
        controller.owner = Some(owner);
        controller.epoch = 71;
        controller
    }
    #[test]
    fn creation_and_script_fields_never_grant_or_select_users() {
        let mut controller = local();
        controller.create_user().unwrap();
        controller.create_user().unwrap();
        assert_eq!(controller.users.len(), 2);
        assert!(controller.draft.users.is_empty());
        assert!(controller.receiver.is_none());
        assert!(controller.actor.is_none());
        assert!(!controller.status().pointer_export && !controller.status().keyboard_export);
        controller.draft.apply_script_fields(
            &BTreeMap::from([
                (
                    "application_event_origin".into(),
                    "https://consumer.example".into(),
                ),
                ("application_event_key_events".into(), "true".into()),
            ]),
            true,
        );
        assert!(!controller.draft.key_events);
        assert!(controller.approve().is_err());
    }
    #[test]
    fn receiver_replacement_keeps_two_users_and_does_not_leak_unissued_recipients() {
        let mut controller = local();
        controller.create_user().unwrap();
        controller.create_user().unwrap();
        let ids: Vec<_> = controller.users.iter().map(|user| user.id).collect();
        controller.draft.users = ids.iter().copied().collect();
        controller.draft.pointer_events = true;
        for _ in 0..(MAX_RECIPIENTS + 8) {
            controller.approve().unwrap();
        }
        assert_eq!(
            controller
                .native
                .users()
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            ids
        );
        assert!(
            controller
                .native
                .users()
                .values()
                .all(|user| user.held_keys.is_none())
        );
        controller.disable_publication().unwrap();
        assert!(controller.receiver.is_none());
        assert!(controller.native.users().is_empty());
        assert_eq!(
            controller
                .users
                .iter()
                .map(|user| user.id)
                .collect::<Vec<_>>(),
            ids
        );
        assert!(!controller.status().pointer_export && !controller.status().keyboard_export);
    }
    #[test]
    fn keyboard_and_delegation_require_distinct_explicit_approvals() {
        let mut draft = Draft {
            users: BTreeSet::from([UserId {
                epoch: 71,
                local: 1,
            }]),
            pointer_events: true,
            key_control: true,
            ..Draft::default()
        };
        assert!(draft.approvals().is_err());
        draft.key_events = true;
        let (events, controls) = draft.approvals().unwrap();
        assert!(events.key_events && controls.key_input);
        assert!(!controls.delegated_users);
        draft.delegated = true;
        assert!(draft.approvals().unwrap().1.delegated_users);
    }
    #[test]
    fn gui_stop_retires_actual_native_stream_and_held_virtual_state() {
        let mut controller = local();
        controller.create_user().unwrap();
        let user = controller.users[0].id;
        controller.draft.users.insert(user);
        controller.draft.key_events = true;
        controller.draft.key_control = true;
        controller.approve().unwrap();
        controller
            .local_command(ClientCommand::Input {
                user,
                input: Input::Key {
                    key: KeyCode(4),
                    transition: Transition::Down,
                },
            })
            .unwrap();
        controller.tick(&egui::Context::default());
        assert_eq!(
            controller.native.users()[&user].held_keys,
            Some(vec![KeyCode(4)])
        );
        let owner = controller.owner.unwrap();
        controller.stop();
        assert!(controller.native.users().is_empty());
        assert!(controller.users.is_empty());
        assert!(controller.retired());
        assert!(
            controller
                .bridge
                .as_ref()
                .unwrap()
                .host()
                .ready_for(owner)
                .is_err()
        );
    }
}
