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
    #[serde(default)]
    pub approved_receivers: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_windows: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_pending: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_stale: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_uncertain: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_oldest_sample_age_ms: Option<u64>,
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
        self.window_oldest_sample_age_ms = self
            .window_oldest_sample_age_ms
            .map(|age| age.saturating_add(elapsed));
        if self.sample_age_ms > 2_000 {
            self.published_windows = None;
            self.window_pending = None;
            self.window_stale = None;
            self.window_uncertain = None;
            self.window_oldest_sample_age_ms = None;
        }
        if self.transport_sample_age_ms.is_none_or(|age| age > 2_000) {
            self.connected_clients = None;
            self.cleanup_pending = None;
            self.uncertain_admissions = None;
            self.queue_overflows = None;
        }
    }
}

#[derive(Clone, Default)]
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
    pub publish_windows: bool,
    pub window_identity_metadata: bool,
    pub window_observation: bool,
    pub window_tracking: bool,
    pub window_control: bool,
    pub selected_window: Option<WindowId>,
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
        let input_family = self.pointer_events
            || self.key_events
            || self.delegated
            || self.lifecycle
            || self.cursor
            || self.pointer_control
            || self.key_control;
        let window_family = self.publish_windows
            || self.window_identity_metadata
            || self.window_observation
            || self.window_tracking
            || self.window_control;
        ensure!(input_family || window_family, "Select a receiver capability first");
        ensure!(self.users.len() <= MAX_USERS, "Too many selected cooperative users");
        ensure!(
            !input_family || !self.users.is_empty(),
            "Select the exact cooperative users first"
        );
        ensure!(
            !input_family || self.pointer_events || self.key_events,
            "Explicitly approve pointer or keyboard observation first"
        );
        ensure!(
            !(self.window_tracking || self.window_control)
                || self.publish_windows
                || self.window_observation,
            "Approve window observation or publication first"
        );
        ensure!(
            !self.window_control || self.window_tracking,
            "Window control requires window tracking"
        );
        ensure!(
            !self.window_identity_metadata || self.publish_windows,
            "Window identity details require publication approval"
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
                owned_windows: self.publish_windows || self.window_observation,
            },
            ControlGrant {
                users: scope,
                delegated_users: self.delegated,
                user_lifecycle: self.lifecycle,
                cursor: self.cursor,
                pointer_input: self.pointer_control,
                key_input: self.key_control,
                window_publication: self.publish_windows,
                window_identity_metadata: self.window_identity_metadata,
                window_tracking: self.window_tracking,
                window_control: self.window_control,
            },
        ))
    }
}

pub(crate) struct LocalUser {
    pub id: UserId,
    pub name: String,
}
/// Independent master consent for the local owner's users. Receiver edits never
/// mutate this authority or derive it from a union of receiver grants.
#[derive(Default)]
pub(crate) struct OwnerConsent {
    pub pointer_events: bool,
    pub key_events: bool,
    pub lifecycle: bool,
    pub cursor: bool,
    pub pointer_control: bool,
    pub key_control: bool,
}
pub(crate) struct Receiver {
    pub row: u64,
    draft: Draft,
    recipient: Option<RecipientId>,
    permit: Option<ClientPermit>,
    approved_origin: Option<String>,
}
pub(crate) struct Controller {
    bridge: Option<HostBridge>,
    owner: Option<RecipientId>,
    receivers: Vec<Receiver>,
    selected: Option<usize>,
    next_row: u64,
    listener_origins: BTreeSet<String>,
    pub owner_consent: OwnerConsent,
    actor: Option<actor::Actor>,
    native_subscription: Option<SubscriptionId>,
    pending_endpoint: Option<NetworkEndpoint>,
    pub native: ClientState,
    pub users: Vec<LocalUser>,
    pub draft: Draft,
    pub error: Option<String>,
    epoch: u64,
    ended: bool,
    copy_pending: Option<(u64, u64)>,
}
impl Default for Controller {
    fn default() -> Self {
        Self {
            bridge: None,
            owner: None,
            receivers: Vec::new(),
            selected: None,
            next_row: 1,
            listener_origins: BTreeSet::new(),
            owner_consent: OwnerConsent::default(),
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
            copy_pending: None,
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
        self.active()
            && self.actor.is_none()
            && self.pending_endpoint.is_some()
            && self
                .receivers
                .iter()
                .any(|receiver| receiver.recipient.is_some())
    }
    fn selected_receiver(&self) -> Option<&Receiver> {
        self.selected.and_then(|index| self.receivers.get(index))
    }
    pub fn receiver_rows(&self) -> Vec<(u64, bool)> {
        self.receivers
            .iter()
            .map(|receiver| (receiver.row, receiver.recipient.is_some()))
            .collect()
    }
    pub fn selected_row(&self) -> Option<u64> {
        self.selected_receiver().map(|receiver| receiver.row)
    }
    pub fn selected_approved(&self) -> bool {
        self.selected_receiver()
            .is_some_and(|receiver| receiver.recipient.is_some())
    }
    pub fn published_windows(&self) -> Vec<WindowInfo> {
        self.bridge
            .as_ref()
            .map(|bridge| {
                bridge
                    .window_inventory()
                    .into_iter()
                    .map(|(info, _)| info)
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn selected_window_reference(&self) -> Result<String> {
        let window = self
            .draft
            .selected_window
            .context("Select a current native window first")?;
        ensure!(
            self.published_windows().iter().any(|info| info.id == window),
            "Selected native window is no longer published"
        );
        Ok(serde_json::to_string(&RemoteWindowRef {
            epoch: self.epoch,
            window,
        })?)
    }
    pub fn can_issue(&self) -> bool {
        self.listener_active()
            && self
                .selected_receiver()
                .is_some_and(|receiver| receiver.recipient.is_some() && receiver.permit.is_none())
    }
    pub fn connection_issued(&self) -> bool {
        self.listener_active()
            && self
                .selected_receiver()
                .and_then(|receiver| receiver.permit.as_ref())
                .is_some_and(ClientPermit::issued)
    }
    fn cancel_copy(&mut self) {
        self.copy_pending = None;
        if let Some(actor) = &self.actor {
            actor.clear_copy();
        }
    }
    pub fn select_receiver(&mut self, row: u64) -> Result<()> {
        let index = self
            .receivers
            .iter()
            .position(|receiver| receiver.row == row)
            .context("Event receiver row is unavailable")?;
        self.cancel_copy();
        if let Some(previous) = self.selected {
            self.receivers[previous].draft = self.draft.clone();
        }
        let name = self.draft.name.clone();
        let focus = self.draft.focus;
        self.draft = self.receivers[index].draft.clone();
        self.draft.name = name;
        self.draft.focus = focus;
        self.selected = Some(index);
        Ok(())
    }
    pub fn add_receiver(&mut self) -> Result<()> {
        ensure!(self.active(), "Create a cooperative session first");
        ensure!(
            self.origin_editable(),
            "End this session before adding an origin or receiver row"
        );
        ensure!(
            self.receivers.len() < 8,
            "At most eight event receiver rows are available"
        );
        let row = self.next_row;
        self.next_row = self
            .next_row
            .checked_add(1)
            .context("Receiver row IDs exhausted")?;
        self.receivers.push(Receiver {
            row,
            draft: Draft {
                ttl: "120".into(),
                ..Draft::default()
            },
            recipient: None,
            permit: None,
            approved_origin: None,
        });
        self.select_receiver(row)
    }
    pub fn origin_editable(&self) -> bool {
        self.actor.is_none()
    }
    pub fn retired(&self) -> bool {
        (!self.active()) && self.actor.as_ref().is_none_or(actor::Actor::retired)
    }
    pub fn status(&self) -> PublicStatus {
        let actor = self.actor.as_ref().map(actor::Actor::snapshot);
        let network = actor.as_ref().and_then(|state| state.service.as_ref());
        let windows = self.bridge.as_ref().map(HostBridge::window_status);
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
            browser_approved: self
                .receivers
                .iter()
                .any(|receiver| receiver.recipient.is_some()),
            approved_receivers: self
                .receivers
                .iter()
                .filter(|receiver| receiver.recipient.is_some())
                .count(),
            published_windows: windows.as_ref().map(|status| status.registered),
            window_pending: windows.as_ref().map(|status| status.pending),
            window_stale: windows.as_ref().map(|status| status.stale),
            window_uncertain: windows.as_ref().map(|status| status.uncertain),
            window_oldest_sample_age_ms: windows
                .as_ref()
                .and_then(|status| status.oldest_sample_age_ms),
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
        self.receivers.clear();
        self.selected = None;
        self.listener_origins.clear();
        self.owner_consent = OwnerConsent::default();
        self.cancel_copy();
        self.actor = None;
        self.epoch = epoch;
        self.ended = false;
        self.native = ClientState::default();
        self.users.clear();
        self.draft.users.clear();
        self.draft.focus = None;
        self.draft.selected_window = None;
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
    pub fn select_all_visible_users(&mut self) -> Result<()> {
        ensure!(self.active(), "Create a cooperative session first");
        ensure!(
            self.selected_row().is_some(),
            "Select an event receiver first"
        );
        self.draft.users = self.users.iter().map(|user| user.id).collect();
        Ok(())
    }
    pub fn apply_owner_consent(&mut self) -> Result<()> {
        ensure!(self.active(), "Create a cooperative session first");
        ensure!(
            !self.owner_consent.key_control || self.owner_consent.key_events,
            "Master keyboard control requires master keyboard observation"
        );
        ensure!(
            !(self.owner_consent.pointer_control || self.owner_consent.cursor)
                || self.owner_consent.pointer_events,
            "Master pointer/cursor control requires master pointer observation"
        );
        // All remote leases are fenced before any ALL_OWNED master mutation,
        // including widening: no old receiver gains authority by implication.
        self.revoke_all();
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
                    grant: Grant {
                        users: UserScope::AllOwned,
                        pointer_events: self.owner_consent.pointer_events,
                        key_events: self.owner_consent.key_events,
                        owned_windows: false,
                    },
                },
            })
            .sdk()?;
        bridge
            .host_command(HostRequest {
                version: PROTOCOL_VERSION,
                command: HostCommand::ApproveControls {
                    recipient: owner,
                    grant: ControlGrant {
                        users: UserScope::AllOwned,
                        delegated_users: false,
                        user_lifecycle: self.owner_consent.lifecycle,
                        cursor: self.owner_consent.cursor,
                        pointer_input: self.owner_consent.pointer_control,
                        key_input: self.owner_consent.key_control,
                        window_publication: false,
                        window_identity_metadata: false,
                        window_tracking: false,
                        window_control: false,
                    },
                },
            })
            .sdk()?;
        if !self.owner_consent.pointer_events && !self.owner_consent.key_events {
            return Ok(());
        }
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
        Ok(())
    }
    pub fn approve(&mut self) -> Result<()> {
        ensure!(self.active(), "Create a cooperative session first");
        let index = self
            .selected
            .context("Add and select an event receiver first")?;
        let (events, controls) = self.draft.approvals()?;
        ensure!(
            self.draft.selected_window.is_none() || self.draft.window_observation,
            "Approve observation of the selected window first"
        );
        crate::sdk_listener::validate_origins(std::slice::from_ref(&self.draft.origin))?;
        ensure!(
            !self.draft.origin.is_empty(),
            "Enter an exact HTTP(S) receiver origin first"
        );
        ensure!(
            self.actor.is_none() || self.listener_origins.contains(&self.draft.origin),
            "This origin was not approved before listener start; end the session to change origins"
        );
        ensure!(
            self.draft
                .users
                .iter()
                .all(|id| self.users.iter().any(|user| user.id == *id)),
            "User selection is stale"
        );
        let owner = self.owner.context("Native consumer unavailable")?;
        let ready = self
            .bridge
            .as_ref()
            .context("Cooperative session unavailable")?
            .host()
            .ready_for(owner)?;
        ensure!(
            !events.pointer_events || ready.events.pointer_events,
            "Apply master pointer observation consent first"
        );
        ensure!(
            !events.key_events || ready.events.key_events,
            "Apply master keyboard observation consent first"
        );
        ensure!(
            !controls.pointer_input || ready.controls.pointer_input,
            "Apply master pointer control consent first"
        );
        ensure!(
            !controls.key_input || ready.controls.key_input,
            "Apply master keyboard control consent first"
        );
        ensure!(
            !controls.cursor || ready.controls.cursor,
            "Apply master cursor consent first"
        );
        ensure!(
            !controls.user_lifecycle || ready.controls.user_lifecycle,
            "Apply master lifecycle consent first"
        );
        let delegated_window = self.draft.selected_window;
        let delegated_access = WindowAccess {
            observe: self.draft.window_observation,
            control: self.draft.window_control,
        };
        if let Some(window) = delegated_window {
            ensure!(
                self.bridge.as_ref().is_some_and(|bridge| {
                    bridge.window_inventory().iter().any(|(info, publisher)| {
                        info.id == window
                            && self.receivers[index].recipient != Some(*publisher)
                            && self.receivers.iter().any(|receiver| {
                                receiver.recipient == Some(*publisher)
                                    && receiver.draft.publish_windows
                            })
                    })
                }),
                "Selected native window is no longer published"
            );
        }
        self.revoke();
        let bridge = self
            .bridge
            .as_mut()
            .context("Cooperative session unavailable")?;
        let HostResponse::RecipientRegistered { recipient } = bridge
            .host_command(HostRequest {
                version: PROTOCOL_VERSION,
                command: HostCommand::RegisterRecipient {
                    local_identity: "GUI-approved event receiver (native/browser)".into(),
                },
            })
            .sdk()?
        else {
            anyhow::bail!("Event receiver unavailable");
        };
        // Record immediately so any partial approval failure is explicitly retired.
        self.receivers[index].recipient = Some(recipient);
        let approved = (|| -> Result<()> {
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
            if let Some(window) = delegated_window {
                bridge
                    .host_command(HostRequest {
                        version: PROTOCOL_VERSION,
                        command: HostCommand::DelegateWindow {
                            window,
                            recipient,
                            access: delegated_access,
                        },
                    })
                    .sdk()?;
            }
            Ok(())
        })();
        if let Err(error) = approved {
            self.revoke();
            return Err(error);
        }
        self.receivers[index].draft = self.draft.clone();
        self.receivers[index].approved_origin = Some(self.draft.origin.clone());
        Ok(())
    }
    fn approved_origins(&self) -> Result<Vec<String>> {
        let origins: BTreeSet<_> = self
            .receivers
            .iter()
            .filter(|receiver| receiver.recipient.is_some())
            .filter_map(|receiver| receiver.approved_origin.clone())
            .collect();
        ensure!(
            !origins.is_empty(),
            "Approve at least one event receiver origin first"
        );
        let origins: Vec<_> = origins.into_iter().collect();
        crate::sdk_listener::validate_origins(&origins)?;
        Ok(origins)
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
        let origins = self.approved_origins()?;
        self.listener_origins = origins.iter().cloned().collect();
        let endpoint = self
            .pending_endpoint
            .take()
            .context("Event network endpoint is unavailable")?;
        self.actor = Some(actor::Actor::start(
            endpoint, origins, root, self.epoch, guards,
        )?);
        Ok(())
    }
    pub fn issue(&mut self) -> Result<()> {
        ensure!(
            self.can_issue(),
            "Start the listener and approve a fresh selected receiver first"
        );
        let index = self.selected.context("Select an event receiver first")?;
        let seconds: u64 = self
            .draft
            .ttl
            .parse()
            .context("TTL must be 1 through 300 seconds")?;
        ensure!(
            (1..=300).contains(&seconds),
            "TTL must be 1 through 300 seconds"
        );
        let receiver = self.receivers[index]
            .recipient
            .context("Approve the selected event receiver first")?;
        let origin = self.receivers[index]
            .approved_origin
            .as_ref()
            .context("Receiver origin unavailable")?;
        let permit = self
            .bridge
            .as_mut()
            .context("Cooperative session unavailable")?
            .approve_client(receiver, origin, Duration::from_secs(seconds))?;
        self.receivers[index].permit = Some(permit.clone());
        if let Err(error) = self
            .actor
            .as_ref()
            .context("Event listener unavailable")?
            .issue(permit)
        {
            self.revoke();
            return Err(error);
        }
        Ok(())
    }
    pub fn request_copy(&mut self) -> Result<()> {
        ensure!(
            self.connection_issued(),
            "Issue a live selected event receiver connection first"
        );
        let receiver = self
            .selected_receiver()
            .context("Select an event receiver first")?;
        let permit = receiver
            .permit
            .clone()
            .context("Issue an event-only connection first")?;
        let ticket = (receiver.row, permit.id());
        self.actor
            .as_ref()
            .context("Event listener unavailable")?
            .copy(permit)?;
        self.copy_pending = Some(ticket);
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
        let (row, id) = self.copy_pending?;
        let receiver = self.selected_receiver()?;
        let permit = receiver.permit.as_ref()?;
        if receiver.row != row || permit.id() != id || !permit.issued() {
            self.cancel_copy();
            return None;
        }
        self.bridge
            .as_ref()?
            .host()
            .ready_for(receiver.recipient?)
            .ok()?;
        let copied = self.actor.as_ref()?.take_copy(id);
        if copied.is_some() {
            self.copy_pending = None;
        }
        copied
    }
    fn revoke_index(&mut self, index: usize) {
        if let Some(recipient) = self.receivers[index].recipient.take()
            && let Some(bridge) = &mut self.bridge
        {
            bridge.revoke_recipient(recipient);
            // This also retires approved recipients which never acquired a permit.
            let _ = bridge.dispatch_local(
                recipient,
                ClientRequest {
                    version: PROTOCOL_VERSION,
                    command: ClientCommand::Disconnect,
                },
            );
        }
        self.receivers[index].permit = None;
        self.receivers[index].approved_origin = None;
    }
    pub fn revoke(&mut self) {
        self.cancel_copy();
        if let Some(index) = self.selected {
            self.revoke_index(index);
        }
    }
    fn revoke_all(&mut self) {
        self.cancel_copy();
        for index in 0..self.receivers.len() {
            self.revoke_index(index);
        }
    }
    pub fn disable_publication(&mut self) -> Result<()> {
        self.owner_consent = OwnerConsent::default();
        self.apply_owner_consent()
    }
    pub fn stop(&mut self) {
        self.revoke_all();
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
        let mut missing = Vec::new();
        if let Some(bridge) = &mut self.bridge {
            let _report: crate::application_event_transport::TickReport = bridge.drain();
            missing = self
                .receivers
                .iter()
                .enumerate()
                .filter_map(|(index, receiver)| {
                    let retired = receiver
                        .recipient
                        .is_some_and(|recipient| bridge.host().ready_for(recipient).is_err())
                        || receiver
                            .permit
                            .as_ref()
                            .is_some_and(|permit| !permit.live());
                    retired.then_some(index)
                })
                .collect();
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
                                        for receiver in &mut self.receivers {
                                            receiver.draft.users.remove(&id);
                                        }
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
        for index in missing {
            if self.selected == Some(index) {
                self.cancel_copy();
            }
            self.revoke_index(index);
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
    const ORIGIN: &str = "https://consumer.example";

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
    fn master(controller: &mut Controller) {
        controller.owner_consent.pointer_events = true;
        controller.owner_consent.key_events = true;
        controller.owner_consent.pointer_control = true;
        controller.owner_consent.key_control = true;
        controller.apply_owner_consent().unwrap();
    }
    fn receiver(controller: &mut Controller, keys: bool, delegated: bool) -> u64 {
        controller.add_receiver().unwrap();
        controller.draft.users = controller.users.iter().map(|user| user.id).collect();
        controller.draft.origin = ORIGIN.into();
        controller.draft.pointer_events = true;
        controller.draft.key_events = keys;
        controller.draft.pointer_control = true;
        controller.draft.key_control = keys;
        controller.draft.delegated = delegated;
        controller.approve().unwrap();
        controller.selected_row().unwrap()
    }
    // Deliberately uses the actual bridge approval API without launching network,
    // descriptor IO, GUI, native input, or a helper process.
    fn permit(controller: &mut Controller) -> ClientPermit {
        let index = controller.selected.unwrap();
        let recipient = controller.receivers[index].recipient.unwrap();
        let permit = controller
            .bridge
            .as_mut()
            .unwrap()
            .approve_client(recipient, ORIGIN, Duration::from_secs(120))
            .unwrap();
        controller.receivers[index].permit = Some(permit.clone());
        permit
    }
    fn remote_key(
        controller: &mut Controller,
        recipient: RecipientId,
        user: UserId,
        transition: Transition,
    ) -> std::result::Result<ClientResponse, Failure> {
        controller.bridge.as_mut().unwrap().dispatch_local(
            recipient,
            ClientRequest {
                version: PROTOCOL_VERSION,
                command: ClientCommand::Input {
                    user,
                    input: Input::Key {
                        key: KeyCode(4),
                        transition,
                    },
                },
            },
        )
    }
    #[test]
    fn window_only_approvals_keep_publication_separate_from_observation() {
        let mut controller = local();
        controller.add_receiver().unwrap();
        controller.draft.origin = ORIGIN.into();
        controller.draft.publish_windows = true;
        controller.draft.window_tracking = true;
        controller.approve().unwrap();
        let publisher = controller.selected_receiver().unwrap().recipient.unwrap();
        let publisher_ready = controller.bridge.as_ref().unwrap().host().ready_for(publisher).unwrap();
        assert!(publisher_ready.events.owned_windows);
        assert!(publisher_ready.controls.window_publication);
        assert!(!publisher_ready.events.pointer_events);
        assert!(!publisher_ready.events.key_events);
        assert_eq!(publisher_ready.events.users, UserScope::Only(BTreeSet::new()));

        controller.add_receiver().unwrap();
        controller.draft.origin = ORIGIN.into();
        controller.draft.window_observation = true;
        controller.draft.window_tracking = true;
        controller.draft.window_control = true;
        controller.approve().unwrap();
        let observer = controller.selected_receiver().unwrap().recipient.unwrap();
        let observer_ready = controller.bridge.as_ref().unwrap().host().ready_for(observer).unwrap();
        assert!(observer_ready.events.owned_windows);
        assert!(!observer_ready.controls.window_publication);
        assert!(!observer_ready.controls.window_identity_metadata);
        assert!(observer_ready.controls.window_tracking);
        assert!(observer_ready.controls.window_control);
        assert_eq!(observer_ready.events.users, UserScope::Only(BTreeSet::new()));
        assert!(controller.published_windows().is_empty());
    }
    #[test]
    fn select_visible_users_stages_exact_current_scope_until_explicit_apply() {
        let mut controller = local();
        controller.create_user().unwrap();
        controller.create_user().unwrap();
        assert!(controller.select_all_visible_users().is_err());
        controller.add_receiver().unwrap();
        let original: BTreeSet<_> = controller.users.iter().map(|user| user.id).collect();
        controller.select_all_visible_users().unwrap();
        assert_eq!(controller.draft.users, original);
        assert_eq!(controller.status().approved_receivers, 0);
        assert!(!controller.status().pointer_export && !controller.status().keyboard_export);
        assert!(!controller.draft.pointer_events && !controller.draft.key_events);
        assert!(
            !controller.draft.delegated
                && !controller.draft.pointer_control
                && !controller.draft.key_control
        );
        assert!(controller.approve().is_err());
        assert!(controller.actor.is_none());
        master(&mut controller);
        controller.draft.origin = ORIGIN.into();
        controller.draft.pointer_events = true;
        controller.approve().unwrap();
        let first = controller.selected_receiver().unwrap().recipient.unwrap();
        let first_permit = permit(&mut controller);
        controller.create_user().unwrap();
        assert_eq!(controller.draft.users, original);
        assert_eq!(
            controller
                .bridge
                .as_ref()
                .unwrap()
                .host()
                .ready_for(first)
                .unwrap()
                .events
                .users,
            UserScope::Only(original.clone())
        );
        controller.select_all_visible_users().unwrap();
        let expanded: BTreeSet<_> = controller.users.iter().map(|user| user.id).collect();
        assert_eq!(expanded.len(), 3);
        assert_eq!(controller.draft.users, expanded);
        assert!(first_permit.live());
        assert_eq!(
            controller
                .bridge
                .as_ref()
                .unwrap()
                .host()
                .ready_for(first)
                .unwrap()
                .events
                .users,
            UserScope::Only(original)
        );
        controller.approve().unwrap();
        assert!(!first_permit.live());
        let second = controller.selected_receiver().unwrap().recipient.unwrap();
        assert_ne!(first, second);
        assert_eq!(
            controller
                .bridge
                .as_ref()
                .unwrap()
                .host()
                .ready_for(second)
                .unwrap()
                .events
                .users,
            UserScope::Only(expanded)
        );
        controller.stop();
        assert!(controller.select_all_visible_users().is_err());
    }

    #[test]
    fn creation_script_and_new_rows_never_select_or_grant_remote_users() {
        let mut controller = local();
        controller.create_user().unwrap();
        controller.create_user().unwrap();
        assert_eq!(controller.users.len(), 2);
        assert!(controller.draft.users.is_empty());
        assert_eq!(controller.status().approved_receivers, 0);
        assert!(controller.actor.is_none());
        assert!(!controller.status().pointer_export && !controller.status().keyboard_export);
        controller.add_receiver().unwrap();
        controller.draft.apply_script_fields(
            &BTreeMap::from([
                ("application_event_origin".into(), ORIGIN.into()),
                ("application_event_key_events".into(), "true".into()),
            ]),
            true,
        );
        assert!(!controller.draft.key_events);
        assert!(controller.approve().is_err());
        master(&mut controller);
        assert!(controller.draft.users.is_empty());
        assert!(controller.approve().is_err());
    }
    #[test]
    fn two_receivers_share_owner_user_ids_with_independent_grants_and_permits() {
        let mut controller = local();
        controller.create_user().unwrap();
        controller.create_user().unwrap();
        let ids: BTreeSet<_> = controller.users.iter().map(|user| user.id).collect();
        master(&mut controller);
        let a = receiver(&mut controller, true, true);
        let first = controller.selected_receiver().unwrap().recipient.unwrap();
        let a_permit = permit(&mut controller);
        let b = receiver(&mut controller, false, false);
        let second = controller.selected_receiver().unwrap().recipient.unwrap();
        let b_permit = permit(&mut controller);
        assert_ne!(a_permit.id(), b_permit.id());
        assert!(a_permit.live() && b_permit.live());
        assert!(!a_permit.issued() && !b_permit.issued());
        assert_eq!(controller.status().approved_receivers, 2);
        assert_eq!(
            controller.approved_origins().unwrap(),
            vec![ORIGIN.to_owned()]
        );
        let bridge = controller.bridge.as_ref().unwrap();
        let first_ready = bridge.host().ready_for(first).unwrap();
        let second_ready = bridge.host().ready_for(second).unwrap();
        assert_eq!(first_ready.events.users, UserScope::Only(ids.clone()));
        assert_eq!(second_ready.events.users, UserScope::Only(ids.clone()));
        assert!(first_ready.events.key_events && first_ready.controls.key_input);
        assert!(!second_ready.events.key_events && !second_ready.controls.delegated_users);
        assert!(controller.status().keyboard_export);
        assert_eq!(
            controller
                .native
                .users()
                .keys()
                .copied()
                .collect::<BTreeSet<_>>(),
            ids
        );
        controller.copy_pending = Some((b, b_permit.id()));
        controller.select_receiver(a).unwrap();
        assert!(controller.copy_pending.is_none());
        controller.copy_pending = Some((a, a_permit.id()));
        controller.revoke();
        assert!(controller.copy_pending.is_none() && controller.take_copy().is_none());
        assert!(!a_permit.live() && b_permit.live());
        assert_eq!(controller.selected_row(), Some(a));
        assert!(
            controller
                .bridge
                .as_ref()
                .unwrap()
                .host()
                .ready_for(first)
                .is_err()
        );
        assert!(
            controller
                .bridge
                .as_ref()
                .unwrap()
                .host()
                .ready_for(second)
                .is_ok()
        );
        controller.approve().unwrap();
        assert!(b_permit.live());
        assert_eq!(controller.selected_row(), Some(a));
        assert_eq!(controller.status().approved_receivers, 2);
    }
    #[test]
    fn master_revocation_fences_all_permits_and_releases_delegated_holds() {
        let mut controller = local();
        controller.create_user().unwrap();
        controller.create_user().unwrap();
        let ids: Vec<_> = controller.users.iter().map(|user| user.id).collect();
        master(&mut controller);
        receiver(&mut controller, true, false);
        let first = controller.selected_receiver().unwrap().recipient.unwrap();
        let a_permit = permit(&mut controller);
        assert!(remote_key(&mut controller, first, ids[0], Transition::Down).is_err());
        receiver(&mut controller, true, true);
        let second = controller.selected_receiver().unwrap().recipient.unwrap();
        let b_permit = permit(&mut controller);
        remote_key(&mut controller, second, ids[0], Transition::Down).unwrap();
        controller.tick(&egui::Context::default());
        assert_eq!(
            controller.native.users()[&ids[0]].held_keys,
            Some(vec![KeyCode(4)])
        );
        controller.disable_publication().unwrap();
        assert!(!a_permit.live() && !b_permit.live());
        assert_eq!(controller.status().approved_receivers, 0);
        assert!(!controller.status().pointer_export && !controller.status().keyboard_export);
        assert!(remote_key(&mut controller, second, ids[0], Transition::Down).is_err());
        master(&mut controller);
        assert_eq!(controller.status().approved_receivers, 0);
        assert_eq!(
            controller.native.users()[&ids[0]].held_keys,
            Some(Vec::new())
        );
        assert_eq!(
            controller
                .users
                .iter()
                .map(|user| user.id)
                .collect::<Vec<_>>(),
            ids
        );
        receiver(&mut controller, true, true);
        let after = permit(&mut controller);
        controller.stop();
        assert!(!after.live() && controller.retired());
        assert!(controller.native.users().is_empty());
    }
    #[test]
    fn repeated_unissued_replacement_retires_recipients_and_preserves_other_row() {
        let mut controller = local();
        controller.create_user().unwrap();
        controller.create_user().unwrap();
        master(&mut controller);
        receiver(&mut controller, true, true);
        let other = controller.selected_receiver().unwrap().recipient.unwrap();
        let other_permit = permit(&mut controller);
        let row = receiver(&mut controller, false, false);
        for _ in 0..(MAX_RECIPIENTS + 8) {
            let previous = controller.selected_receiver().unwrap().recipient.unwrap();
            controller.approve().unwrap();
            assert!(
                controller
                    .bridge
                    .as_ref()
                    .unwrap()
                    .host()
                    .ready_for(previous)
                    .is_err()
            );
        }
        assert_eq!(controller.selected_row(), Some(row));
        assert!(other_permit.live());
        assert!(
            controller
                .bridge
                .as_ref()
                .unwrap()
                .host()
                .ready_for(other)
                .unwrap()
                .events
                .key_events
        );
        assert_eq!(controller.users.len(), 2);
        // Draft origin changes do not widen the allowlist derived from approvals.
        controller.draft.origin = "https://unapproved.example".into();
        assert_eq!(
            controller.approved_origins().unwrap(),
            vec![ORIGIN.to_owned()]
        );
        controller.approve().unwrap();
        assert_eq!(
            controller.approved_origins().unwrap(),
            vec![ORIGIN.to_owned(), "https://unapproved.example".to_owned()]
        );
    }
}
