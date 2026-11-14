//! GUI-thread mapping of private worker bindings to observation-only SDK users.
use super::*;
use crate::desktop_router::observation as source;

#[derive(Default)]
pub(super) struct Sources {
    screens: Option<ScreenState>,
    owner: Option<RecipientId>,
    next_request: u64,
    bindings: BTreeMap<source::BindingTag, Binding>,
}
struct Binding {
    identity: String,
    name: String,
    phase: Phase,
}
#[derive(Clone, Copy)]
enum Phase {
    Pending { request: u64 },
    Bound { user: UserId },
    Cancelled,
}
impl Sources {
    pub(super) fn empty(&self) -> bool {
        self.bindings.is_empty()
    }
    pub(super) fn new(screens: ScreenState) -> Self {
        Self {
            screens: Some(screens),
            next_request: 1,
            ..Self::default()
        }
    }
}

fn configured(value: source::ConfiguredCursor) -> ConfiguredDesktopCursor {
    ConfiguredDesktopCursor {
        shape: value.shape,
        size: value.size,
        color: value.color,
        visible: value.visible,
        follow_application: value.follow_application,
    }
}
fn native_key(value: source::WindowsVirtualKey) -> WindowsVirtualKey {
    WindowsVirtualKey {
        virtual_key: value.virtual_key,
        scan_code: value.scan_code,
        prefix: match value.prefix {
            source::ScanPrefix::None => ScanPrefix::None,
            source::ScanPrefix::E0 => ScanPrefix::E0,
            source::ScanPrefix::E1 => ScanPrefix::E1,
        },
    }
}
fn button(value: mux_core::desktop::MouseButton) -> MouseButton {
    match value {
        mux_core::desktop::MouseButton::Primary => MouseButton::Primary,
        mux_core::desktop::MouseButton::Secondary => MouseButton::Secondary,
        mux_core::desktop::MouseButton::Middle => MouseButton::Middle,
        mux_core::desktop::MouseButton::X1 => MouseButton::X1,
        mux_core::desktop::MouseButton::X2 => MouseButton::X2,
    }
}
fn record(value: source::Record) -> DesktopRecordV1 {
    let admission = match value.admission {
        source::Admission::LogicalOnly => DesktopAdmission::LogicalOnly,
        source::Admission::NativeSubmissionAccepted => DesktopAdmission::NativeSubmissionAccepted,
        source::Admission::SharedNativeHold => DesktopAdmission::SharedNativeHold,
    };
    let change = match value.change {
        source::Change::Motion { position } => DesktopObservation::Motion { position },
        source::Change::Button {
            button: value,
            transition,
        } => DesktopObservation::Button {
            button: button(value),
            transition,
        },
        source::Change::Key { key, transition } => DesktopObservation::Key {
            key: native_key(key),
            transition,
        },
        source::Change::Wheel { axis, units } => DesktopObservation::Wheel { axis, units },
        source::Change::TextUnit { unit } => DesktopObservation::TextUnit { unit },
        source::Change::Cursor { cursor } => DesktopObservation::Cursor {
            cursor: configured(cursor),
        },
    };
    DesktopRecordV1 {
        version: 1,
        sequence: value.sequence,
        screen_revision: value.screen_revision,
        admission,
        change,
    }
}

impl Controller {
    pub fn desktop_rows(&self) -> Vec<(u64, String, bool)> {
        self.desktop
            .bindings
            .iter()
            .filter_map(|(tag, binding)| match binding.phase {
                Phase::Cancelled => None,
                Phase::Pending { .. } => Some((tag.token, binding.name.clone(), true)),
                Phase::Bound { .. } => Some((tag.token, binding.name.clone(), false)),
            })
            .collect()
    }
    pub fn desktop_counts(&self) -> (usize, usize) {
        self.desktop
            .bindings
            .values()
            .fold((0, 0), |(bound, pending), binding| match binding.phase {
                Phase::Bound { .. } => (bound + 1, pending),
                Phase::Pending { .. } => (bound, pending + 1),
                Phase::Cancelled => (bound, pending),
            })
    }
    pub fn desktop_identity_selected(&self, identity: &str) -> bool {
        self.desktop.bindings.values().any(|binding| {
            binding.identity == identity && !matches!(binding.phase, Phase::Cancelled)
        })
    }
    pub(super) fn selected_desktop_users(&self) -> bool {
        self.users
            .iter()
            .any(|user| user.desktop_observed && self.draft.users.contains(&user.id))
    }
    pub fn bind_desktop_source(
        &mut self,
        worker: &crate::session::Controller,
        identity: String,
        name: String,
    ) -> Result<()> {
        ensure!(self.active(), "Create an application event session first");
        ensure!(
            self.desktop.screens.is_some(),
            "Desktop screen mapping is unavailable"
        );
        ensure!(
            self.desktop.bindings.len() < MAX_USERS && !self.desktop_identity_selected(&identity),
            "This desktop user is already selected or the observation limit was reached"
        );
        let request = self.desktop.next_request;
        self.desktop.next_request = request
            .checked_add(1)
            .context("Desktop observation request sequence exhausted")?;
        ensure!(
            request != 0,
            "Desktop observation request sequence unavailable"
        );
        let tag = worker
            .bind_desktop_observation(request, identity.clone())
            .map_err(|reason| anyhow::anyhow!("Desktop observation rejected: {reason:?}"))?;
        self.desktop.bindings.insert(
            tag,
            Binding {
                identity,
                name,
                phase: Phase::Pending { request },
            },
        );
        Ok(())
    }
    pub fn unbind_desktop_source(
        &mut self,
        worker: &crate::session::Controller,
        token: u64,
    ) -> Result<()> {
        let tag = self
            .desktop
            .bindings
            .keys()
            .find(|tag| tag.token == token)
            .copied()
            .context("Desktop observation selection is no longer current")?;
        // The controller fence precedes retiring the public mapping; this never releases input.
        worker.unbind_desktop_observation(tag);
        self.retire_desktop_tag(tag);
        self.desktop.bindings.remove(&tag);
        Ok(())
    }
    fn retire_desktop_tag(&mut self, tag: source::BindingTag) {
        let Some(binding) = self.desktop.bindings.get_mut(&tag) else {
            return;
        };
        let old = std::mem::replace(&mut binding.phase, Phase::Cancelled);
        if let Phase::Bound { user } = old {
            let failed = if let (Some(bridge), Some(owner)) = (&mut self.bridge, self.desktop.owner)
            {
                bridge.is_desktop_observed(user)
                    && bridge.retire_desktop_observed(owner, user).is_err()
            } else {
                false
            };
            self.users.retain(|entry| entry.id != user);
            self.draft.users.remove(&user);
            for receiver in &mut self.receivers {
                receiver.draft.users.remove(&user);
            }
            if self.draft.focus == Some(user) {
                self.draft.focus = None;
            }
            if failed {
                // A failed authority retirement closes the complete event session.
                for binding in self.desktop.bindings.values_mut() {
                    binding.phase = Phase::Cancelled;
                }
                if let Some(bridge) = &mut self.bridge {
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
                self.error = Some(
                    "Desktop observation retirement was unconfirmed; the event session ended."
                        .into(),
                );
            }
        }
    }
    pub fn fence_desktop_sources(&mut self) {
        self.retire_all_desktop();
        self.error = Some(
            "Desktop observation transport was not private; its mappings were retired.".into(),
        );
    }
    pub(super) fn retire_all_desktop(&mut self) {
        let tags = self.desktop.bindings.keys().copied().collect::<Vec<_>>();
        for tag in tags {
            self.retire_desktop_tag(tag);
        }
    }
    pub fn poll_desktop_sources(&mut self, worker: Option<&crate::session::Controller>) {
        let Some(worker) = worker else {
            self.retire_all_desktop();
            self.desktop.bindings.clear();
            return;
        };
        let generation = worker.desktop_observation_generation();
        // Persistent loss is checked before the inbox and again before each fold.
        let dead = self
            .desktop
            .bindings
            .iter()
            .filter_map(|(tag, binding)| {
                (!self.active()
                    || tag.routing_generation != generation
                    || matches!(binding.phase, Phase::Cancelled)
                    || (matches!(binding.phase, Phase::Bound { .. })
                        && !worker.desktop_observation_live(*tag)))
                .then_some(*tag)
            })
            .collect::<Vec<_>>();
        for tag in dead {
            worker.unbind_desktop_observation(tag);
            self.retire_desktop_tag(tag);
            self.desktop.bindings.remove(&tag);
        }
        for delivery in worker.take_desktop_observations() {
            match delivery {
                source::Delivery::Lost { tag, .. } => {
                    let tags = tag.map_or_else(|| self.desktop.bindings.keys().copied().collect(), |tag| vec![tag]);
                    for tag in tags { self.retire_desktop_tag(tag); self.desktop.bindings.remove(&tag); }
                }
                source::Delivery::Rejected { request, tag, reason } => {
                    if self.desktop.bindings.get(&tag).is_some_and(|binding|
                        matches!(binding.phase, Phase::Pending { request: expected } if expected == request)) {
                        self.desktop.bindings.remove(&tag);
                        self.error = Some(format!("Desktop observation was not started: {reason:?}"));
                    }
                }
                source::Delivery::Bound(baseline) => {
                    let tag = baseline.tag;
                    let pending = self.desktop.bindings.get(&tag).is_some_and(|binding|
                        matches!(binding.phase, Phase::Pending { request } if request == baseline.request));
                    if !pending || !self.active() || !worker.desktop_observation_live(tag) { continue; }
                    let created = self.install_desktop_baseline(&baseline);
                    if let Err(error) = created {
                        worker.unbind_desktop_observation(tag);
                        self.retire_desktop_tag(tag);
                        self.desktop.bindings.remove(&tag);
                        self.error = Some(format!("Desktop observation was not started: {error:#}"));
                    } else if !worker.desktop_observation_live(tag) {
                        self.retire_desktop_tag(tag); self.desktop.bindings.remove(&tag);
                    }
                }
                source::Delivery::Record(value) => {
                    let tag = value.tag;
                    let user = self.desktop.bindings.get(&tag).and_then(|binding| match binding.phase {
                        Phase::Bound { user } => Some(user), _ => None,
                    });
                    let Some(user) = user else { continue; };
                    if !self.active() || !worker.desktop_observation_live(tag) {
                        self.retire_desktop_tag(tag); self.desktop.bindings.remove(&tag); continue;
                    }
                    let published = match (&mut self.bridge, self.desktop.owner) {
                        (Some(bridge), Some(owner)) => bridge.publish_desktop_observation(owner, user, record(value)).is_ok(),
                        _ => false,
                    };
                    if !published || !worker.desktop_observation_live(tag) {
                        worker.unbind_desktop_observation(tag);
                        self.retire_desktop_tag(tag); self.desktop.bindings.remove(&tag);
                        if !published { self.error = Some("Desktop observation became unconfirmed; bind the user again explicitly.".into()); }
                    }
                }
            }
        }
        // A loss observed during the batch still outranks the next transport drain.
        let lost = self
            .desktop
            .bindings
            .keys()
            .copied()
            .filter(|tag| !worker.desktop_observation_live(*tag))
            .collect::<Vec<_>>();
        for tag in lost {
            self.retire_desktop_tag(tag);
            self.desktop.bindings.remove(&tag);
        }
    }
    fn install_desktop_baseline(&mut self, value: &source::Baseline) -> Result<()> {
        ensure!(self.active() && self.desktop.bindings.get(&value.tag).is_some_and(|binding|
            matches!(binding.phase, Phase::Pending { request } if request == value.request)),
            "Desktop binding was cancelled or replaced");
        ensure!(
            self.desktop.screens.as_ref() == Some(&value.screens)
                && self
                    .bridge
                    .as_ref()
                    .is_some_and(|bridge| bridge.screens() == &value.screens),
            "Desktop screens changed; create a fresh event session with the current display mapping"
        );
        ensure!(
            value.buttons.len() <= 5
                && value
                    .buttons
                    .iter()
                    .all(|entry| (1..=5).contains(&entry.count)),
            "Desktop held-button baseline is invalid"
        );
        let baseline = DesktopBaselineV1 {
            version: 1,
            next_sequence: value.next_sequence,
            screens: value.screens.clone(),
            position: value.position,
            cursor: configured(value.cursor),
            held_buttons: value
                .buttons
                .iter()
                .map(|entry| button(entry.button))
                .collect(),
            native_held_keys: value.keys.iter().copied().map(native_key).collect(),
        };
        baseline.validate()?;
        let bridge = self
            .bridge
            .as_mut()
            .context("Application event session unavailable")?;
        let owner = if let Some(owner) = self.desktop.owner {
            owner
        } else {
            let HostResponse::RecipientRegistered { recipient } = bridge
                .host_command(HostRequest {
                    version: PROTOCOL_VERSION,
                    command: HostCommand::RegisterRecipient {
                        local_identity: "GUI private desktop observation provider".into(),
                    },
                })
                .sdk()?
            else {
                anyhow::bail!("Desktop provider unavailable");
            };
            self.desktop.owner = Some(recipient);
            recipient
        };
        let user = bridge.create_desktop_observed(owner, &value.name, baseline)?;
        let binding = self
            .desktop
            .bindings
            .get_mut(&value.tag)
            .context("Desktop binding was cancelled")?;
        binding.name.clone_from(&value.name);
        binding.phase = Phase::Bound { user };
        self.users.push(LocalUser {
            id: user,
            name: value.name.clone(),
            desktop_observed: true,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepared() -> (Controller, source::Baseline) {
        let bounds = mux_core::desktop::Bounds {
            left: 0,
            top: 0,
            right: 200,
            bottom: 100,
        };
        let screens = ScreenState {
            revision: 1,
            desktop: bounds,
            screens: vec![bounds],
        };
        let mut host = Host::<PlatformWindows>::new(71, screens.clone()).unwrap();
        let owner = host
            .register_recipient("GUI test cooperative owner")
            .unwrap();
        let (bridge, endpoint) = HostBridge::new(host);
        let mut controller = Controller::default();
        controller.bridge = Some(bridge);
        controller.pending_endpoint = Some(endpoint);
        controller.owner = Some(owner);
        controller.epoch = 71;
        controller.desktop = Sources::new(screens.clone());
        let tag = source::BindingTag {
            routing_generation: 3,
            binding_generation: 1,
            token: 1,
        };
        controller.desktop.bindings.insert(
            tag,
            Binding {
                identity: "private-seat".into(),
                name: "Selected virtual user".into(),
                phase: Phase::Pending { request: 1 },
            },
        );
        let value = source::Baseline {
            request: 1,
            tag,
            next_sequence: 1,
            name: "Confirmed virtual user".into(),
            screens,
            position: Point { x: 12, y: 34 },
            cursor: (&mux_core::desktop::CursorSettings::default()).into(),
            buttons: vec![source::ButtonCount {
                button: mux_core::desktop::MouseButton::Primary,
                count: 2,
            }],
            keys: vec![source::WindowsVirtualKey {
                virtual_key: 254,
                scan_code: 255,
                prefix: source::ScanPrefix::E1,
            }],
        };
        (controller, value)
    }

    #[test]
    fn cancelled_and_mismatched_baselines_never_create_public_users() {
        let (mut controller, mut baseline) = prepared();
        controller
            .desktop
            .bindings
            .get_mut(&baseline.tag)
            .unwrap()
            .phase = Phase::Cancelled;
        assert!(controller.install_desktop_baseline(&baseline).is_err());
        assert!(controller.users.is_empty());
        controller
            .desktop
            .bindings
            .get_mut(&baseline.tag)
            .unwrap()
            .phase = Phase::Pending { request: 1 };
        baseline.screens.revision = 2;
        assert!(controller.install_desktop_baseline(&baseline).is_err());
        assert!(controller.users.is_empty());
        assert!(controller.desktop.owner.is_none());
    }

    #[test]
    fn provider_retirement_removes_selected_authority_without_retiring_cooperative_users() {
        for retired_by_core in [false, true] {
            let (mut controller, baseline) = prepared();
            controller.create_user().unwrap();
            let cooperative = controller.users[0].id;
            controller.install_desktop_baseline(&baseline).unwrap();
            assert_eq!(controller.desktop_counts(), (1, 0));
            let observed = controller
                .users
                .iter()
                .find(|user| user.desktop_observed)
                .unwrap()
                .id;
            assert_ne!(observed, cooperative);
            assert_ne!(controller.desktop.owner, controller.owner);
            controller.draft.users = BTreeSet::from([cooperative, observed]);
            assert!(controller.selected_desktop_users());
            if retired_by_core {
                let owner = controller.desktop.owner.unwrap();
                let bridge = controller.bridge.as_mut().unwrap();
                assert!(
                    bridge
                        .publish_desktop_observation(
                            owner,
                            observed,
                            DesktopRecordV1 {
                                version: 1,
                                sequence: 2,
                                screen_revision: 1,
                                admission: DesktopAdmission::LogicalOnly,
                                change: DesktopObservation::Motion {
                                    position: Point { x: 15, y: 16 }
                                },
                            }
                        )
                        .is_err()
                );
                assert!(!bridge.is_desktop_observed(observed));
            }
            controller.retire_desktop_tag(baseline.tag);
            controller.retire_desktop_tag(baseline.tag);
            assert_eq!(controller.desktop_counts(), (0, 0));
            assert_eq!(controller.users.len(), 1);
            assert_eq!(controller.users[0].id, cooperative);
            assert_eq!(controller.draft.users, BTreeSet::from([cooperative]));
            assert!(controller.active());
            assert!(controller.error.is_none());
        }
    }
}
