//! GUI-owned, default-closed authority for profile setup while routing is stopped.
use anyhow::{Result, ensure};
use mux_core::Profile;
use mux_core::{Device, DeviceKind, emulated};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::{
        Arc, Mutex, Weak,
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    time::Duration,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    /// Read callback activity on the authenticated broker worker, without a GUI request.
    Activity,
    Inspect,
    ApplicationEventStatus,
    RefreshDiscovery,
    Configure {
        expected_revision: u64,
        document: String,
    },
    ConfigurePersistence {
        expected_revision: u64,
        policy: crate::app_preferences::ProfilePersistence,
    },
    Save {
        expected_revision: u64,
    },
    Load {
        expected_revision: u64,
    },
    Import {
        expected_revision: u64,
    },
    Export {
        expected_revision: u64,
    },
}

pub enum PreparedCommand {
    Inspect,
    ApplicationEventStatus,
    RefreshDiscovery,
    Configure {
        expected_revision: u64,
        profile: Box<Profile>,
    },
    ConfigurePersistence {
        expected_revision: u64,
        policy: crate::app_preferences::ProfilePersistence,
    },
    Save {
        expected_revision: u64,
    },
    Load {
        expected_revision: u64,
    },
    Import {
        expected_revision: u64,
    },
    Export {
        expected_revision: u64,
    },
}

impl Command {
    pub fn prepare(self) -> Result<PreparedCommand> {
        Ok(match self {
            Self::Activity => anyhow::bail!("Activity must be read directly by the setup broker"),
            Self::Inspect => PreparedCommand::Inspect,
            Self::ApplicationEventStatus => PreparedCommand::ApplicationEventStatus,
            Self::RefreshDiscovery => PreparedCommand::RefreshDiscovery,
            Self::Configure {
                expected_revision,
                document,
            } => {
                let (profile, _) = crate::profile_store::decode_with_report(document.as_bytes())?;
                PreparedCommand::Configure {
                    expected_revision,
                    profile: Box::new(profile),
                }
            }
            Self::Save { expected_revision } => PreparedCommand::Save { expected_revision },
            Self::ConfigurePersistence {
                expected_revision,
                policy,
            } => PreparedCommand::ConfigurePersistence {
                expected_revision,
                policy,
            },
            Self::Load { expected_revision } => PreparedCommand::Load { expected_revision },
            Self::Import { expected_revision } => PreparedCommand::Import { expected_revision },
            Self::Export { expected_revision } => PreparedCommand::Export { expected_revision },
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reply {
    pub client: u64,
    pub sequence: u64,
    pub value: Value,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<Profile>,
}

pub struct Request {
    pub client: u64,
    pub sequence: u64,
    pub epoch: u64,
    pub command: PreparedCommand,
    pub decision: Arc<crate::app_lifecycle::CommitDecision>,
    reply: SyncSender<Reply>,
}

/// Dropping a broker wait cancels only work whose commit has not been claimed.
/// A claimed preference transaction is still installed in the global GUI state.
pub struct PendingReply {
    receiver: Receiver<Reply>,
    decision: Arc<crate::app_lifecycle::CommitDecision>,
}

impl PendingReply {
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Reply, RecvTimeoutError> {
        self.receiver.recv_timeout(timeout)
    }
}

impl Drop for PendingReply {
    fn drop(&mut self) {
        self.decision.cancel();
    }
}

impl Request {
    pub fn reply(self, value: Value, error: Option<String>, profile: Option<Profile>) {
        let _ = self.reply.try_send(Reply {
            client: self.client,
            sequence: self.sequence,
            value,
            error,
            profile,
        });
    }
}

#[derive(Default)]
struct Authority {
    epoch: u64,
    enabled: bool,
    clients: BTreeSet<u64>,
    decisions: BTreeMap<(u64, u64), Weak<crate::app_lifecycle::CommitDecision>>,
}

impl Authority {
    fn cancel_pending(&mut self, client: Option<u64>) {
        self.decisions.retain(|(owner, _), weak| {
            let Some(decision) = weak.upgrade() else {
                return false;
            };
            if client.is_none_or(|client| client == *owner) {
                decision.cancel();
            }
            true
        });
    }
}

#[derive(Clone)]
pub struct Handle {
    activity: Option<crate::gui_activity::Observer>,
    state: Arc<Mutex<Authority>>,
    sender: SyncSender<Request>,
}

pub fn channel() -> (Handle, Receiver<Request>) {
    let (sender, receiver) = mpsc::sync_channel(16);
    (
        Handle {
            activity: None,
            state: Arc::new(Mutex::new(Authority::default())),
            sender,
        },
        receiver,
    )
}

pub fn public_devices(devices: &[Device]) -> Value {
    Value::Array(
        devices
            .iter()
            .map(|device| {
                serde_json::json!({
                    "path": device.path,
                    "kind": device.kind,
                })
            })
            .collect(),
    )
}

pub fn configured_virtual_devices(profile: &Profile) -> Value {
    let mut paths = BTreeMap::new();
    for seat in &profile.seats {
        for (path, kind) in [
            (Some(seat.mouse.as_str()), DeviceKind::Mouse),
            (seat.keyboard.as_deref(), DeviceKind::Keyboard),
        ] {
            let Some(path) = path else { continue };
            if emulated::parse(path)
                .ok()
                .flatten()
                .is_some_and(|device| device.kind == kind)
            {
                paths.insert(path.to_owned(), kind);
            }
        }
    }
    Value::Array(
        paths
            .into_iter()
            .map(|(path, kind)| {
                serde_json::json!({
                    "path": path,
                    "kind": kind,
                    "source": "configured_profile",
                })
            })
            .collect(),
    )
}

pub fn check_mutation(expected: u64, current: u64, dirty: bool, file_pending: bool) -> Result<()> {
    ensure!(
        expected == current,
        "Stopped setup revision changed; inspect again"
    );
    ensure!(!file_pending, "A profile file operation is already pending");
    ensure!(
        !dirty,
        "GUI has staged profile edits; apply or discard them before setup changes"
    );
    Ok(())
}

pub fn validate_new_device_paths(
    current: &Profile,
    candidate: &Profile,
    fresh: bool,
    devices: &[Device],
) -> Result<()> {
    // Most stopped profiles are virtual-only. Build physical indexes only if a
    // physical path is actually encountered, and never search the old roster
    // once per seat: a complete profile may contain many seats.
    let mut previous_by_id: Option<HashMap<String, &mux_core::Seat>> = None;
    let mut discovered: Option<(HashSet<String>, HashSet<String>)> = None;
    for seat in &candidate.seats {
        for (path, kind) in [
            (Some(seat.mouse.as_str()), DeviceKind::Mouse),
            (seat.keyboard.as_deref(), DeviceKind::Keyboard),
        ] {
            let Some(path) = path else { continue };
            if let Some(virtual_device) = emulated::parse(path)? {
                ensure!(
                    virtual_device.kind == kind,
                    "Virtual device kind does not match the seat assignment: {path}"
                );
                continue;
            }
            let previous = previous_by_id
                .get_or_insert_with(|| {
                    current
                        .seats
                        .iter()
                        .map(|old| (old.identity(), old))
                        .collect()
                })
                .get(&seat.identity())
                .copied();
            let old = match kind {
                DeviceKind::Mouse => previous.map(|old| old.mouse.as_str()),
                DeviceKind::Keyboard => previous.and_then(|old| old.keyboard.as_deref()),
            };
            if old.is_some_and(|old| old.eq_ignore_ascii_case(path)) {
                continue;
            }
            ensure!(
                fresh,
                "Fresh device discovery is required for a new physical assignment"
            );
            let inventory = discovered.get_or_insert_with(|| {
                let mut mice = HashSet::with_capacity(devices.len());
                let mut keyboards = HashSet::with_capacity(devices.len());
                for device in devices {
                    let paths = match device.kind {
                        DeviceKind::Mouse => &mut mice,
                        DeviceKind::Keyboard => &mut keyboards,
                    };
                    paths.insert(device.path.to_ascii_lowercase());
                }
                (mice, keyboards)
            });
            let paths = match kind {
                DeviceKind::Mouse => &inventory.0,
                DeviceKind::Keyboard => &inventory.1,
            };
            ensure!(
                paths.contains(&path.to_ascii_lowercase()),
                "New physical device path was not found in fresh discovery: {path}"
            );
        }
    }
    Ok(())
}

impl Handle {
    pub fn attach_activity(&mut self, observer: crate::gui_activity::Observer) {
        self.activity = Some(observer);
    }

    pub fn activity_snapshot(&self) -> Option<crate::gui_activity::Snapshot> {
        self.activity
            .as_ref()
            .map(crate::gui_activity::Observer::snapshot)
    }

    /// This read cannot enqueue GUI work or change profile/routing authority.
    pub fn authorized_activity(&self, client: u64, sequence: u64, epoch: u64) -> Result<Reply> {
        ensure!(
            self.permits(client, epoch),
            "Stopped setup connection was revoked"
        );
        let activity = self.activity_snapshot();
        ensure!(
            self.permits(client, epoch),
            "Stopped setup connection was revoked"
        );
        Ok(Reply {
            client,
            sequence,
            value: serde_json::json!({"gui_activity": activity}),
            error: None,
            profile: None,
        })
    }

    pub fn enable(&self) -> u64 {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.cancel_pending(None);
        state.epoch = state.epoch.checked_add(1).expect("setup epoch exhausted");
        state.enabled = true;
        state.clients.clear();
        state.epoch
    }

    pub fn revoke(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.cancel_pending(None);
        state.epoch = state.epoch.checked_add(1).expect("setup epoch exhausted");
        state.enabled = false;
        state.clients.clear();
    }

    pub fn enabled(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .enabled
    }

    pub fn admit(&self, client: u64) -> Result<u64> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ensure!(state.enabled, "Stopped setup access is disabled");
        state.clients.insert(client);
        Ok(state.epoch)
    }

    pub fn remove(&self, client: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.cancel_pending(Some(client));
        state.clients.remove(&client);
    }

    pub fn cancel_pending(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cancel_pending(None);
    }

    pub fn permits(&self, client: u64, epoch: u64) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.enabled && state.epoch == epoch && state.clients.contains(&client)
    }

    pub fn submit(
        &self,
        client: u64,
        sequence: u64,
        epoch: u64,
        command: PreparedCommand,
    ) -> Result<PendingReply> {
        let (reply, receiver) = mpsc::sync_channel(1);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ensure!(
            state.enabled && state.epoch == epoch && state.clients.contains(&client),
            "Stopped setup connection was revoked"
        );
        state.decisions.retain(|_, weak| weak.strong_count() > 0);
        ensure!(
            !state.decisions.contains_key(&(client, sequence)),
            "Stopped setup request already pending"
        );
        let decision = Arc::new(crate::app_lifecycle::CommitDecision::default());
        self.sender
            .try_send(Request {
                client,
                sequence,
                epoch,
                command,
                decision: decision.clone(),
                reply,
            })
            .map_err(|_| anyhow::anyhow!("Stopped setup GUI queue is unavailable or busy"))?;
        state
            .decisions
            .insert((client, sequence), Arc::downgrade(&decision));
        Ok(PendingReply { receiver, decision })
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;

    #[test]
    fn persistence_requires_a_complete_narrow_policy() {
        let request = serde_json::json!({"operation":"configure_persistence", "expected_revision":7,
            "policy":{"load_profile_at_startup":false,"save_profile_on_exit":true}});
        assert!(matches!(
            serde_json::from_value::<Command>(request.clone())
                .unwrap()
                .prepare()
                .unwrap(),
            PreparedCommand::ConfigurePersistence {
                expected_revision: 7,
                ..
            }
        ));
        let mut incomplete = request.clone();
        incomplete["policy"]
            .as_object_mut()
            .unwrap()
            .remove("save_profile_on_exit");
        assert!(serde_json::from_value::<Command>(incomplete).is_err());
        let mut unrelated = request;
        unrelated["policy"]["launch_at_login"] = true.into();
        assert!(serde_json::from_value::<Command>(unrelated).is_err());
    }

    #[test]
    fn abandoned_wait_cancels_only_its_unclaimed_request() {
        let (handle, requests) = channel();
        let epoch = handle.enable();
        handle.admit(1).unwrap();
        let first_wait = handle
            .submit(1, 1, epoch, PreparedCommand::Inspect)
            .unwrap();
        let first = requests.try_recv().unwrap();
        let second_wait = handle
            .submit(1, 2, epoch, PreparedCommand::Inspect)
            .unwrap();
        let second = requests.try_recv().unwrap();
        drop(first_wait);
        assert!(first.decision.cancelled());
        assert!(!second.decision.cancelled());
        assert!(second.decision.claim());
        drop(second_wait);
        assert!(second.decision.claimed());
        assert!(!second.decision.cancelled());
    }

    #[test]
    fn revoke_and_revision_change_cancel_before_claim_without_reversing_commits() {
        let (handle, requests) = channel();
        let epoch = handle.enable();
        handle.admit(1).unwrap();
        handle.admit(2).unwrap();
        let _wait1 = handle
            .submit(1, 1, epoch, PreparedCommand::Inspect)
            .unwrap();
        let first = requests.try_recv().unwrap();
        let _wait2 = handle
            .submit(2, 1, epoch, PreparedCommand::Inspect)
            .unwrap();
        let second = requests.try_recv().unwrap();
        handle.remove(1);
        assert!(first.decision.cancelled());
        assert!(!second.decision.cancelled());
        assert!(second.decision.claim());
        handle.cancel_pending();
        handle.revoke();
        assert!(second.decision.claimed());
        assert!(!second.decision.cancelled());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_is_optional_read_only_and_requires_current_admission() {
        let (mut handle, requests) = channel();
        assert!(handle.activity_snapshot().is_none());
        assert!(handle.authorized_activity(7, 1, 0).is_err());
        let probe = crate::gui_activity::Probe::new();
        handle.attach_activity(probe.observer());
        let epoch = handle.enable();
        handle.admit(7).unwrap();
        assert!(handle.authorized_activity(8, 1, epoch).is_err());
        assert!(handle.authorized_activity(7, 1, epoch + 1).is_err());
        let reply = handle.authorized_activity(7, 4, epoch).unwrap();
        assert_eq!((reply.client, reply.sequence), (7, 4));
        assert!(reply.error.is_none());
        assert!(reply.profile.is_none());
        let activity: crate::gui_activity::Snapshot =
            serde_json::from_value(reply.value["gui_activity"].clone()).unwrap();
        assert!(activity.observation.is_some());
        assert!(matches!(
            requests.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        // Activity cannot accidentally fall through the GUI preparation path.
        assert!(Command::Activity.prepare().is_err());
        handle.revoke();
        assert!(handle.authorized_activity(7, 5, epoch).is_err());
    }

    #[test]
    fn event_status_uses_the_admitted_cancellable_setup_queue() {
        let (handle, requests) = channel();
        let prepared = || Command::ApplicationEventStatus.prepare().unwrap();
        assert!(handle.submit(7, 1, 0, prepared()).is_err());
        let epoch = handle.enable();
        assert!(handle.submit(7, 1, epoch, prepared()).is_err());
        handle.admit(7).unwrap();
        assert!(handle.submit(8, 1, epoch, prepared()).is_err());
        assert!(handle.submit(7, 1, epoch + 1, prepared()).is_err());
        let waiting = handle.submit(7, 1, epoch, prepared()).unwrap();
        let request = requests.try_recv().unwrap();
        assert!(matches!(
            &request.command,
            PreparedCommand::ApplicationEventStatus
        ));
        assert!(!request.decision.cancelled());
        handle.revoke();
        assert!(!handle.permits(request.client, request.epoch));
        assert!(request.decision.cancelled());
        assert!(handle.submit(7, 2, epoch, prepared()).is_err());
        drop(waiting);
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn default_closed_and_revoke_invalidates_existing_client() {
        let (handle, _) = channel();
        assert!(!handle.enabled());
        assert!(handle.admit(7).is_err());
        let epoch = handle.enable();
        assert_eq!(handle.admit(7).unwrap(), epoch);
        assert!(handle.permits(7, epoch));
        handle.revoke();
        assert!(!handle.permits(7, epoch));
        assert!(
            handle
                .submit(7, 1, epoch, PreparedCommand::Inspect)
                .is_err()
        );
    }

    #[test]
    fn disconnect_removes_only_its_client_and_new_epoch_requires_readmission() {
        let (handle, _) = channel();
        let first = handle.enable();
        handle.admit(1).unwrap();
        handle.admit(2).unwrap();
        handle.remove(1);
        assert!(!handle.permits(1, first));
        assert!(handle.permits(2, first));
        let second = handle.enable();
        assert_ne!(first, second);
        assert!(!handle.permits(2, first));
        assert!(!handle.permits(2, second));
        handle.admit(2).unwrap();
        assert!(handle.permits(2, second));
    }

    #[test]
    fn queued_request_loses_authority_before_gui_commit_after_revoke() {
        let (handle, queued) = channel();
        let epoch = handle.enable();
        handle.admit(9).unwrap();
        let _waiting = handle
            .submit(9, 3, epoch, PreparedCommand::Inspect)
            .unwrap();
        handle.revoke();
        let request = queued.try_recv().unwrap();
        assert_eq!(
            (request.client, request.sequence, request.epoch),
            (9, 3, epoch)
        );
        assert!(!handle.permits(request.client, request.epoch));
    }

    #[test]
    fn stale_revision_and_dirty_draft_refuse_replacement() {
        assert!(check_mutation(4, 5, false, false).is_err());
        assert!(check_mutation(5, 5, true, false).is_err());
        assert!(check_mutation(5, 5, false, true).is_err());
        assert!(check_mutation(5, 5, false, false).is_ok());
    }

    #[test]
    fn public_inventory_redacts_raw_input_handle() {
        let inventory = public_devices(&[Device {
            path: "device-path".into(),
            kind: DeviceKind::Mouse,
            handle: 0xdeadbeef,
        }]);
        let text = inventory.to_string();
        assert!(text.contains("device-path"));
        assert!(!text.contains("3735928559"));
        assert!(!text.contains("handle"));
    }

    #[test]
    fn new_physical_assignment_is_compared_by_seat_identity() {
        let current: Profile = serde_json::from_value(serde_json::json!({
            "version": 1, "desktop": {},
            "seats": [{"id":"seat-a", "name":"A", "mouse":"disconnected-old"}]
        }))
        .unwrap();
        let inventory = [Device {
            path: "connected-new".into(),
            kind: DeviceKind::Mouse,
            handle: 19,
        }];
        assert!(validate_new_device_paths(&current, &current, false, &[]).is_ok());
        let mut changed = current.clone();
        changed.seats[0].mouse = "connected-new".into();
        assert!(validate_new_device_paths(&current, &changed, false, &inventory).is_err());
        assert!(validate_new_device_paths(&current, &changed, true, &inventory).is_ok());
        changed.seats[0].id = "new-seat".into();
        changed.seats[0].mouse = "disconnected-old".into();
        assert!(validate_new_device_paths(&current, &changed, false, &[]).is_err());
    }

    #[test]
    fn mixed_profile_virtual_projection_is_separate_from_physical_inventory() {
        let profile: Profile = serde_json::from_value(serde_json::json!({
            "version": 1, "desktop": {},
            "seats": [
                {"id":"v", "name":"Virtual", "mouse":"virtual://mouse/23", "keyboard":"virtual://keyboard/23"},
                {"id":"p", "name":"Physical", "mouse":"physical-path"}
            ]
        })).unwrap();
        let virtual_rows = configured_virtual_devices(&profile);
        let physical_rows = public_devices(&[Device {
            path: "physical-path".into(),
            kind: DeviceKind::Mouse,
            handle: 9001,
        }]);
        assert_eq!(virtual_rows.as_array().unwrap().len(), 2);
        assert!(virtual_rows.to_string().contains("virtual://mouse/23"));
        assert!(!virtual_rows.to_string().contains("physical-path"));
        assert!(physical_rows.to_string().contains("physical-path"));
        assert!(!physical_rows.to_string().contains("9001"));
        assert!(!virtual_rows.to_string().contains("handle"));
    }

    #[test]
    fn virtual_device_kind_must_match_mouse_or_keyboard_slot() {
        let current = Profile::default();
        let wrong: Profile = serde_json::from_value(serde_json::json!({
            "version": 1, "desktop": {},
            "seats": [{"id":"wrong", "name":"Wrong", "mouse":"virtual://keyboard/1"}]
        }))
        .unwrap();
        assert!(validate_new_device_paths(&current, &wrong, false, &[]).is_err());
    }

    #[test]
    fn many_seat_reorder_keeps_identity_matched_disconnected_paths() {
        let seats: Vec<_> = (1..=1_536)
            .map(|id| {
                serde_json::json!({
                    "id": format!("seat-{id}"),
                    "name": format!("User {id}"),
                    "mouse": format!("physical-mouse-{id}"),
                })
            })
            .collect();
        let current: Profile = serde_json::from_value(serde_json::json!({
            "version": 1, "desktop": {}, "seats": seats,
        }))
        .unwrap();
        let mut reordered = current.clone();
        reordered.seats.reverse();
        assert!(validate_new_device_paths(&current, &reordered, false, &[]).is_ok());

        reordered.seats[0].mouse = "new-physical-mouse".into();
        let inventory = [Device {
            path: "new-physical-mouse".into(),
            kind: DeviceKind::Mouse,
            handle: 77,
        }];
        assert!(validate_new_device_paths(&current, &reordered, false, &inventory).is_err());
        assert!(validate_new_device_paths(&current, &reordered, true, &inventory).is_ok());

        let mut all_virtual = current.clone();
        for (index, seat) in all_virtual.seats.iter_mut().enumerate() {
            seat.mouse = format!("virtual://mouse/{}", index + 1);
        }
        assert!(validate_new_device_paths(&current, &all_virtual, false, &[]).is_ok());
    }
}
