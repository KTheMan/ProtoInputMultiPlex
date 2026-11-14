//! Explicit capabilities for clients of a GUI-owned routing session.
use crate::session::{Event, Request};
use mux_core::{Profile, desktop::Bounds};
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};

mod annotation_export;

#[derive(Clone, Serialize, Deserialize)]
pub enum Command {
    Configure {
        enabled: bool,
        user_ids: Vec<String>,
        allowed_pids: Vec<u32>,
        capture: bool,
        #[serde(default)]
        desktop_capture: bool,
        session_control: bool,
        app_preferences: bool,
    },
    Inspect {
        client: u64,
        sequence: u64,
    },
    Connect {
        client: u64,
        sequence: u64,
    },
    Execute {
        client: u64,
        sequence: u64,
        request: Box<Request>,
    },
    Capture {
        client: u64,
        sequence: u64,
        user: usize,
        #[serde(default)]
        query: Option<crate::screen_analysis::Query>,
    },
    Disconnect {
        client: u64,
        sequence: u64,
    },
}
impl Command {
    pub fn tag(&self) -> Option<(u64, u64)> {
        match self {
            Self::Configure { .. } => None,
            Self::Inspect { client, sequence }
            | Self::Connect { client, sequence }
            | Self::Execute {
                client, sequence, ..
            }
            | Self::Capture {
                client, sequence, ..
            }
            | Self::Disconnect { client, sequence } => Some((*client, *sequence)),
        }
    }
}
/// Application-scoped preferences exposed over the authenticated GUI bridge.
/// Startup registration is intentionally absent from this schema.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppPreferencesCommand {
    Inspect,
    Configure { settings: AppPreferencesSettings },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppPreferencesSettings {
    pub launch_minimized: bool,
    pub minimize_to_tray: bool,
    pub close_to_tray: bool,
    pub shortcuts: Vec<crate::app_preferences::Shortcut>,
    #[serde(default)]
    pub appearance_mode: Option<crate::app_preferences::AppearanceMode>,
    #[serde(default)]
    pub zoom_percent: Option<u16>,
    #[serde(default)]
    pub show_app_launcher: Option<bool>,
}

impl AppPreferencesSettings {
    pub fn apply_to(
        &self,
        current: &crate::app_preferences::Preferences,
    ) -> anyhow::Result<crate::app_preferences::Preferences> {
        let mut candidate = current.clone();
        candidate.launch_minimized = self.launch_minimized;
        candidate.minimize_to_tray = self.minimize_to_tray;
        candidate.close_to_tray = self.close_to_tray;
        candidate.shortcuts.clone_from(&self.shortcuts);
        if let Some(mode) = self.appearance_mode {
            candidate.appearance_mode = mode;
        }
        if let Some(zoom) = self.zoom_percent {
            candidate.zoom_percent = zoom;
        }
        if let Some(show_app_launcher) = self.show_app_launcher {
            candidate.show_app_launcher = show_app_launcher;
        }
        candidate.validate()?;
        Ok(candidate)
    }
}

#[cfg(test)]
mod app_preferences_tests {
    use super::*;

    #[test]
    fn preference_reply_distinguishes_document_from_live_shortcut_health() {
        let mut state: AppPreferencesState = serde_json::from_value(serde_json::json!({
            "acknowledged": true, "client": 1, "sequence": 2,
            "preferences": null, "error": null
        }))
        .unwrap();
        assert!(state.shortcut_status.is_none());
        state.shortcut_status = Some(crate::app_lifecycle::ShortcutStatus {
            active: false,
            error: Some("Saved chord is occupied".into()),
        });
        let value = serde_json::to_value(&state).unwrap();
        assert_eq!(value["shortcut_status"]["active"], false);
        assert_eq!(value["shortcut_status"]["error"], "Saved chord is occupied");
        assert!(state.error.is_none());
    }

    #[test]
    fn old_configure_shape_preserves_optional_preferences_and_explicit_fields_merge() {
        let old_shape: AppPreferencesSettings = serde_json::from_value(serde_json::json!({
            "launch_minimized": false,
            "minimize_to_tray": false,
            "close_to_tray": false,
            "shortcuts": []
        }))
        .unwrap();
        let current = crate::app_preferences::Preferences {
            appearance_mode: crate::app_preferences::AppearanceMode::Light,
            zoom_percent: 125,
            ..Default::default()
        };
        let merged = old_shape.apply_to(&current).unwrap();
        assert_eq!(merged.appearance_mode, current.appearance_mode);
        assert_eq!(merged.zoom_percent, 125);
        assert!(merged.show_app_launcher);

        let explicit: AppPreferencesSettings = serde_json::from_value(serde_json::json!({
            "launch_minimized": false,
            "minimize_to_tray": false,
            "close_to_tray": false,
            "shortcuts": [],
            "appearance_mode": "dark",
            "zoom_percent": 75,
            "show_app_launcher": false
        }))
        .unwrap();
        let merged = explicit.apply_to(&current).unwrap();
        assert_eq!(
            merged.appearance_mode,
            crate::app_preferences::AppearanceMode::Dark
        );
        assert_eq!(merged.zoom_percent, 75);
        assert!(!merged.show_app_launcher);
        assert!(!old_shape.apply_to(&merged).unwrap().show_app_launcher);

        let invalid = AppPreferencesSettings {
            zoom_percent: Some(76),
            ..explicit
        };
        assert!(invalid.apply_to(&current).is_err());
    }

    #[test]
    fn remote_configure_preserves_profile_persistence_and_rejects_policy_fields() {
        let current = crate::app_preferences::Preferences {
            load_profile_at_startup: false,
            save_profile_on_exit: true,
            appearance_mode: crate::app_preferences::AppearanceMode::Light,
            ..Default::default()
        };
        let settings: AppPreferencesSettings = serde_json::from_value(serde_json::json!({
            "launch_minimized": true,
            "minimize_to_tray": true,
            "close_to_tray": false,
            "shortcuts": [],
            "appearance_mode": "dark"
        }))
        .unwrap();

        let configured = settings.apply_to(&current).unwrap();

        assert!(configured.launch_minimized);
        assert!(configured.minimize_to_tray);
        assert_eq!(
            configured.appearance_mode,
            crate::app_preferences::AppearanceMode::Dark
        );
        assert!(!configured.load_profile_at_startup);
        assert!(configured.save_profile_on_exit);

        for policy_field in [
            serde_json::json!({"load_profile_at_startup": true}),
            serde_json::json!({"save_profile_on_exit": false}),
        ] {
            let mut value = serde_json::json!({
                "launch_minimized": false,
                "minimize_to_tray": false,
                "close_to_tray": false,
                "shortcuts": []
            });
            value
                .as_object_mut()
                .unwrap()
                .extend(policy_field.as_object().unwrap().clone());
            assert!(serde_json::from_value::<AppPreferencesSettings>(value).is_err());
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppPreferencesState {
    pub acknowledged: bool,
    pub client: u64,
    pub sequence: u64,
    pub preferences: Option<crate::app_preferences::Preferences>,
    #[serde(default)]
    pub shortcut_status: Option<crate::app_lifecycle::ShortcutStatus>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Policy {
    pub enabled: bool,
    pub user_ids: Vec<String>,
    pub allowed_pids: Vec<u32>,
    pub capture: bool,
    pub desktop_capture: bool,
    pub session_control: bool,
    pub app_preferences: bool,
}

fn authorize_graph_scope(
    policy: &Policy,
    configured_pids: &[u32],
    active_user_ids: &[String],
    recipients: &[u32],
) -> Result<()> {
    ensure!(
        !configured_pids.is_empty(),
        "Connected graph changes require an explicit, nonempty desktop process allowlist"
    );
    ensure!(
        configured_pids
            .iter()
            .all(|pid| policy.allowed_pids.contains(pid)),
        "Agent Access must permit every process in the desktop routing allowlist"
    );
    ensure!(
        active_user_ids
            .iter()
            .all(|id| policy.user_ids.contains(id)),
        "Connected graph changes require permission for every active user"
    );
    ensure!(
        recipients
            .iter()
            .all(|pid| { configured_pids.contains(pid) && policy.allowed_pids.contains(pid) }),
        "A current selected input recipient is outside the authorized desktop process scope"
    );
    Ok(())
}

fn authorize_virtual_user_grants(
    slots: &[virtual_users::Slot],
    permitted_ids: &[String],
    receipt_target: Option<&str>,
) -> Result<Vec<String>> {
    let mut required = Vec::new();
    for slot in slots.iter().filter(|slot| slot.active) {
        let id = slot.seat.identity();
        if receipt_target == Some(id.as_str()) {
            ensure!(
                slot.clone_of.is_some(),
                "A creator receipt can only exempt its generated replica"
            );
        } else {
            ensure!(
                permitted_ids.contains(&id),
                "Connected virtual replica changes require permission for every active user"
            );
            required.push(id);
        }
    }
    Ok(required)
}

#[cfg(test)]
mod graph_scope_tests {
    use super::*;

    #[test]
    fn legacy_policy_and_configure_do_not_grant_desktop_capture() {
        let policy: Policy = serde_json::from_value(serde_json::json!({
            "capture": true, "allowed_pids": [10], "user_ids": ["a"]
        }))
        .unwrap();
        assert!(policy.capture);
        assert!(!policy.desktop_capture);
        let command: Command = serde_json::from_value(serde_json::json!({
            "Configure": {
                "enabled": true, "user_ids": ["a"], "allowed_pids": [10],
                "capture": true, "session_control": false, "app_preferences": false
            }
        }))
        .unwrap();
        assert!(matches!(
            command,
            Command::Configure {
                desktop_capture: false,
                ..
            }
        ));
    }

    #[test]
    fn whole_session_graph_scope_requires_explicit_pids_all_users_and_current_recipients() {
        let mut policy = Policy {
            enabled: true,
            user_ids: vec!["seat-a".into(), "seat-b".into()],
            allowed_pids: vec![10, 20],
            session_control: true,
            ..Default::default()
        };
        let users = vec!["seat-a".into(), "seat-b".into()];
        assert!(authorize_graph_scope(&policy, &[10, 20], &users, &[10, 20]).is_ok());

        assert!(
            authorize_graph_scope(&policy, &[], &users, &[])
                .unwrap_err()
                .to_string()
                .contains("nonempty")
        );
        assert!(
            authorize_graph_scope(&policy, &[10, 30], &users, &[10])
                .unwrap_err()
                .to_string()
                .contains("every process")
        );
        assert!(
            authorize_graph_scope(&policy, &[10], &users, &[20])
                .unwrap_err()
                .to_string()
                .contains("recipient")
        );

        policy.user_ids.pop();
        assert!(
            authorize_graph_scope(&policy, &[10, 20], &users, &[])
                .unwrap_err()
                .to_string()
                .contains("every active user")
        );
    }
}

fn authorize_magnifier_policy(
    policy: &Policy,
    command: &crate::magnifier_runtime::Command,
) -> Result<()> {
    use crate::magnifier_runtime::Command;
    match command {
        Command::Inspect => Ok(()),
        Command::Configure { settings, .. } => {
            ensure!(
                policy.session_control,
                "Session changes are disabled in Agent Access"
            );
            if settings.mode != mux_core::magnifier::Mode::Off {
                ensure!(policy.capture, "Target capture is disabled in Agent Access");
                ensure!(
                    settings.source == mux_core::magnifier::Source::SelectedTarget,
                    "Connected magnifier configuration requires the selected allowed target"
                );
            }
            Ok(())
        }
        Command::Refresh { .. } | Command::Export { .. } => {
            ensure!(policy.capture, "Target capture is disabled in Agent Access");
            Ok(())
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapturedFrame {
    pub png: Vec<u8>,
    pub bounds: Bounds,
    pub width: u32,
    pub height: u32,
    pub pid: u32,
    pub hwnd: usize,
    pub user_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analysis: Option<serde_json::Value>,
}
/// Worker-to-controller authority receipt. The public GUI pipe strips this
/// before serialization; it is never an identity token or target grant itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerAuthority {
    pub policy_generation: u64,
    pub active_user_ids: Vec<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application_events: Option<Box<crate::gui_application_events::PublicStatus>>,
    #[serde(default)]
    pub terminal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<crate::session_recovery::Projection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<WorkerAuthority>,
    pub acknowledged: bool,
    pub client: u64,
    pub sequence: u64,
    pub policy: Policy,
    pub clients: Vec<u64>,
    #[serde(default)]
    pub cleanup: Vec<ClientCleanupState>,
    pub events: Vec<Event>,
    pub profile: Option<Profile>,
    pub frame: Option<CapturedFrame>,
    /// Correlated private reply. The supervisor delivers this only to the
    /// matching connected client and sequence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapper_monitors: Option<crate::mapper_runtime::MonitorReceipt>,
    pub error: Option<String>,
}
/// Saved release obligations belonging to a revoked client, never authorization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientCleanupState {
    pub client: u64,
    pub pending_owners: usize,
    pub attempts: u64,
    pub last_error: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CaptureState {
    pub acknowledged: bool,
    pub frame: Option<CapturedFrame>,
    pub error: Option<String>,
}

use crate::{macro_studio, routing_engine::Engine, virtual_users};
use anyhow::{Context, Result, bail, ensure};
use mux_core::automation::Action;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::mpsc,
    time::{Duration, Instant},
};

pub struct Runtime {
    policy: Policy,
    clients: BTreeSet<u64>,
    sequence: BTreeMap<u64, u64>,
    created_virtual_users: BTreeMap<u64, BTreeSet<String>>,
    capture: Option<PendingCapture>,
    export: Option<PendingExport>,
    annotation_export: Option<annotation_export::Pending>,
    generation: u64,
    mode: Option<(mux_core::routing::Mode, bool)>,
    automatic: bool,
    cleanup: RetiredClients,
}

const CLEANUP_RETRY_INTERVAL: Duration = Duration::from_millis(250);
const MAX_CLIENT_SLOTS: usize = 8;
struct PendingCleanup {
    state: ClientCleanupState,
    due: Instant,
}
#[derive(Default)]
struct RetiredClients {
    pending: BTreeMap<u64, PendingCleanup>,
    last_attempted: u64,
}
impl RetiredClients {
    fn snapshot(&self) -> Vec<ClientCleanupState> {
        self.pending
            .values()
            .map(|pending| pending.state.clone())
            .collect()
    }
    fn enroll(&mut self, client: u64, now: Instant) {
        self.pending
            .entry(client)
            .or_insert_with(|| PendingCleanup {
                state: ClientCleanupState {
                    client,
                    pending_owners: 0,
                    attempts: 0,
                    last_error: String::new(),
                },
                due: now,
            });
    }
    fn attempt(
        &mut self,
        client: u64,
        now: Instant,
        release: impl FnOnce() -> (usize, Result<()>),
    ) -> bool {
        let Some(pending) = self.pending.get_mut(&client) else {
            return false;
        };
        let (owners, result) = release();
        self.last_attempted = client;
        if result.is_ok() && owners == 0 {
            self.pending.remove(&client);
            return true;
        }
        let error = result.err().map_or_else(
            || "Saved input releases are still pending".to_owned(),
            |error| format!("{error:#}").chars().take(512).collect(),
        );
        let changed = pending.state.pending_owners != owners || pending.state.last_error != error;
        pending.state.pending_owners = owners;
        pending.state.attempts = pending.state.attempts.saturating_add(1);
        pending.state.last_error = error;
        pending.due = now + CLEANUP_RETRY_INTERVAL;
        changed
    }
    fn poll(&mut self, now: Instant, release: impl FnOnce(u64) -> (usize, Result<()>)) -> bool {
        // One due client per tick, rotating past the previous attempt so a
        // repeatedly failing client cannot starve another retired connection.
        let client = self
            .pending
            .range((
                std::ops::Bound::Excluded(self.last_attempted),
                std::ops::Bound::Unbounded,
            ))
            .chain(self.pending.range(..=self.last_attempted))
            .find(|(_, pending)| pending.due <= now)
            .map(|(&client, _)| client);
        client.is_some_and(|client| self.attempt(client, now, || release(client)))
    }
}
struct PendingCapture {
    client: u64,
    sequence: u64,
    user: usize,
    window: mux_core::routing::Window,
    generation: u64,
    started: Instant,
    replied: bool,
    receiver: mpsc::Receiver<Result<CapturedFrame>>,
}

struct PendingExport {
    client: u64,
    sequence: u64,
    user: usize,
    user_id: String,
    window: mux_core::routing::Window,
    policy_generation: u64,
    settings_generation: u64,
    frame_generation: u64,
    started: Instant,
    replied: bool,
    decided: bool,
    approved: bool,
    job: crate::agent_export::ExportJob,
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;
    use mux_core::{desktop::CursorSettings, emulated::Packet, routing::Window};
    use std::{cell::RefCell, rc::Rc};

    #[derive(Default)]
    struct Delivered {
        held: BTreeSet<(u32, u32)>,
        fail_up: Option<(u32, u32)>,
        packets: Vec<Packet>,
        settings_calls: usize,
        pause_calls: Vec<(usize, bool)>,
        fail_pause: bool,
        selected_override: Option<Window>,
    }
    struct ReleaseEngine(Rc<RefCell<Delivered>>);
    impl Engine for ReleaseEngine {
        fn forwarding_supported(&self) -> bool {
            true
        }
        fn input(&mut self, packet: Packet) -> Result<()> {
            let mut state = self.0.borrow_mut();
            let key = (packet.device, packet.data);
            if packet.flags & 1 != 0 {
                // Desktop routing reports failed delivery in route status and
                // may return Ok while retaining the native hold. Exercise that
                // real contract rather than only an Engine returning Err.
                if state.fail_up == Some(key) {
                    return Ok(());
                }
                state.held.remove(&key);
            } else {
                state.held.insert(key);
            }
            state.packets.push(packet);
            Ok(())
        }
        fn selected_window(&self, user: usize) -> Result<Option<Window>> {
            if let Some(window) = self.0.borrow().selected_override {
                return Ok(Some(window));
            }
            Ok(Some(Window {
                pid: 10 + user as u32 * 10,
                hwnd: user + 1,
            }))
        }
        fn key_held(&self, device: u32, key: u32) -> bool {
            self.0.borrow().held.contains(&(device, key))
        }
        fn button_held(&self, _: u32, _: u32) -> bool {
            false
        }
        fn paused(&self) -> bool {
            false
        }
        fn release_all(&mut self) -> Result<()> {
            self.0.borrow_mut().held.clear();
            Ok(())
        }
        fn pause(&mut self, _: bool) -> Result<()> {
            Ok(())
        }
        fn user_pause_state(&self, user: usize) -> crate::routing_engine::UserPauseState {
            let state = self.0.borrow();
            crate::routing_engine::UserPauseState {
                user,
                supported: true,
                paused: state
                    .pause_calls
                    .iter()
                    .rev()
                    .find(|(index, _)| *index == user)
                    .is_some_and(|(_, paused)| *paused),
                pending_releases: usize::from(state.fail_pause && !state.pause_calls.is_empty()),
                error: None,
            }
        }
        fn pause_user(&mut self, user: usize, paused: bool) -> Result<()> {
            let mut state = self.0.borrow_mut();
            state.pause_calls.push((user, paused));
            ensure!(!state.fail_pause, "Saved pause release failed");
            Ok(())
        }
        fn settings(&mut self, _: usize, _: CursorSettings) -> Result<()> {
            self.0.borrow_mut().settings_calls += 1;
            Ok(())
        }
        fn tick(&mut self) -> Result<(bool, Vec<crate::router::State>)> {
            Ok((false, Vec::new()))
        }
    }

    #[test]
    fn completed_analysis_is_withheld_when_capture_authority_or_exact_target_changes() {
        for variant in 0..14 {
            let ocr = variant >= 7;
            let case = variant % 7;
            let profile:Profile=serde_json::from_value(serde_json::json!({"version":1,"desktop":{"allowed_pids":[10]},"seats":[
                {"id":"a","name":"A","mouse":"virtual://mouse/1","keyboard":"virtual://keyboard/1","target_pid":10}
            ]})).unwrap();
            let seats = profile.resolve(&[]).unwrap();
            let delivered = Rc::new(RefCell::new(Delivered::default()));
            let annotations = crate::annotation_runtime::Runtime::new(
                Box::new(ReleaseEngine(delivered.clone())),
                &profile,
                seats.clone(),
            )
            .unwrap();
            let mut router =
                crate::magnifier_runtime::Runtime::new(annotations, &profile, seats.clone())
                    .unwrap();
            let users = virtual_users::Runtime::new(&profile, seats.clone(), 99).unwrap();
            let automation = macro_studio::Runtime::new_with_ids(seats, vec!["a".into()]);
            let mut access = Runtime::new();
            access.policy = Policy {
                enabled: true,
                user_ids: vec!["a".into()],
                allowed_pids: vec![10],
                capture: true,
                desktop_capture: false,
                session_control: false,
                app_preferences: false,
            };
            access.clients.insert(11);
            let mut png = std::io::Cursor::new(Vec::new());
            image::write_buffer_with_format(
                &mut png,
                &[20, 30, 40, 255],
                1,
                1,
                image::ColorType::Rgba8,
                image::ImageFormat::Png,
            )
            .unwrap();
            let mut frame = CapturedFrame {
                png: png.into_inner(),
                bounds: Bounds {
                    left: 0,
                    top: 0,
                    right: 1,
                    bottom: 1,
                },
                width: 1,
                height: 1,
                pid: 10,
                hwnd: 1,
                user_id: "a".into(),
                analysis: None,
            };
            frame.analysis = if ocr {
                Some(
                    serde_json::json!({"type":"ocr","text":"private OCR result","lines":[],"snapshot_only":true}),
                )
            } else {
                Some(
                    crate::screen_analysis::analyze(
                        &frame,
                        &crate::screen_analysis::prepare_query(
                            crate::screen_analysis::Query::Pixel { x: 0, y: 0 },
                        )
                        .unwrap(),
                    )
                    .unwrap(),
                )
            };
            let (sender, receiver) = mpsc::sync_channel(1);
            sender.send(Ok(frame)).unwrap();
            access.capture = Some(PendingCapture {
                client: 11,
                sequence: 7,
                user: 0,
                window: Window { pid: 10, hwnd: 1 },
                generation: access.generation,
                started: Instant::now(),
                replied: false,
                receiver,
            });
            match case {
                1 => access.policy.capture = false,
                2 => access.policy.allowed_pids = vec![20],
                3 => {
                    access.clients.remove(&11);
                }
                4 => access.generation += 1,
                5 => delivered.borrow_mut().selected_override = Some(Window { pid: 10, hwnd: 2 }),
                6 => {
                    access.capture.as_mut().unwrap().started =
                        Instant::now() - Duration::from_secs(6)
                }
                _ => {}
            }
            let event = access
                .poll_capture(&users, &automation, &mut router, &profile)
                .unwrap()
                .unwrap();
            let Event::Agent(state) = event else {
                panic!("expected scoped capture reply");
            };
            if case == 0 {
                assert!(state.error.is_none());
                let analysis = state.frame.unwrap().analysis.unwrap();
                if ocr {
                    assert_eq!(analysis["text"], "private OCR result");
                } else {
                    assert_eq!(analysis["rgba"], serde_json::json!([20, 30, 40, 255]));
                }
            } else {
                assert!(state.error.is_some(), "case {case}");
                assert!(
                    state.frame.is_none(),
                    "case {case} leaked pixels or completed analysis"
                );
            }
            assert!(access.capture.is_none());
            assert!(delivered.borrow().packets.is_empty());
        }
    }

    #[test]
    fn user_pause_requires_own_scope_not_all_users_and_preserves_negative_receipt() {
        for case in 0..6 {
            let profile:Profile=serde_json::from_value(serde_json::json!({"version":1,"desktop":{"allowed_pids":[10,20]},"seats":[
                {"id":"a","name":"A","mouse":"virtual://mouse/1","keyboard":"virtual://keyboard/1","target_pid":10},
                {"id":"b","name":"B","mouse":"virtual://mouse/2","keyboard":"virtual://keyboard/2","target_pid":20}]})).unwrap();
            let seats = profile.resolve(&[]).unwrap();
            let delivered = Rc::new(RefCell::new(Delivered {
                fail_pause: case == 5,
                ..Default::default()
            }));
            let annotations = crate::annotation_runtime::Runtime::new(
                Box::new(ReleaseEngine(delivered.clone())),
                &profile,
                seats.clone(),
            )
            .unwrap();
            let mut router =
                crate::magnifier_runtime::Runtime::new(annotations, &profile, seats.clone())
                    .unwrap();
            let mut users = virtual_users::Runtime::new(&profile, seats.clone(), 99).unwrap();
            let mut automation =
                macro_studio::Runtime::new_with_ids(seats, vec!["a".into(), "b".into()]);
            let mut access = Runtime::new();
            let mut send = |command| {
                access
                    .command(
                        command,
                        &mut users,
                        &mut automation,
                        &mut router,
                        &profile,
                        99,
                    )
                    .unwrap()
                    .unwrap()
            };
            assert!(
                send(Command::Configure {
                    enabled: true,
                    user_ids: vec![if case == 1 { "b" } else { "a" }.into()],
                    allowed_pids: vec![if case == 2 { 20 } else { 10 }],
                    capture: false,
                    desktop_capture: false,
                    session_control: case != 0,
                    app_preferences: false
                })
                .error
                .is_none()
            );
            assert!(
                send(Command::Connect {
                    client: 11,
                    sequence: 0
                })
                .error
                .is_none()
            );
            let gui = (case == 3).then(|| crate::session::SettingsTicket {
                request: 1,
                identity: "a".into(),
            });
            let result = send(Command::Execute {
                client: 11,
                sequence: 1,
                request: Box::new(Request::UserPause {
                    user: 0,
                    paused: true,
                    gui,
                }),
            });
            assert_eq!(result.error.is_none(), case == 4);
            assert_eq!(delivered.borrow().pause_calls.len(), usize::from(case >= 4));
            let current = result
                .events
                .iter()
                .find_map(|event| {
                    if let Event::UserPauseStates(states) = event {
                        Some(states)
                    } else {
                        None
                    }
                })
                .unwrap();
            assert_eq!(current.len(), 1);
            assert_eq!(current[0].user, usize::from(case == 1));
            if case == 5 {
                assert!(result.events.iter().any(|event|matches!(event,Event::UserPause(state) if state.paused && state.pending_releases==1 && state.error.as_deref().is_some_and(|error|error.contains("Saved pause release failed")))));
            }
        }
    }

    #[test]
    fn live_application_focus_rejection_preserves_held_agent_input_and_settings() {
        use mux_core::keyboard::ApplicationFocus;
        for original in [
            ApplicationFocus::Compatibility,
            ApplicationFocus::NativeControls,
        ] {
            let mut profile: Profile = serde_json::from_value(serde_json::json!({
                "version":1,"desktop":{},"seats":[
                    {"id":"a","name":"A","mouse":"virtual://mouse/1",
                     "keyboard":"virtual://keyboard/1","target_pid":10}
                ]
            }))
            .unwrap();
            profile.seats[0].cursor.keyboard.application_focus = original;
            let seats = profile.resolve(&[]).unwrap();
            let keyboard = seats[0].keyboard.unwrap();
            let delivered = Rc::new(RefCell::new(Delivered::default()));
            let annotations = crate::annotation_runtime::Runtime::new(
                Box::new(ReleaseEngine(delivered.clone())),
                &profile,
                seats.clone(),
            )
            .unwrap();
            let mut router =
                crate::magnifier_runtime::Runtime::new(annotations, &profile, seats.clone())
                    .unwrap();
            let mut users = virtual_users::Runtime::new(&profile, seats.clone(), 99).unwrap();
            let mut automation = macro_studio::Runtime::new_with_ids(seats, vec!["a".into()]);
            let mut access = Runtime::new();
            let mut send = |command| {
                access
                    .command(
                        command,
                        &mut users,
                        &mut automation,
                        &mut router,
                        &profile,
                        99,
                    )
                    .unwrap()
                    .unwrap()
            };
            assert!(
                send(Command::Configure {
                    enabled: true,
                    user_ids: vec!["a".into()],
                    allowed_pids: vec![10],
                    capture: false,
                    desktop_capture: false,
                    session_control: true,
                    app_preferences: false,
                })
                .error
                .is_none()
            );
            assert!(
                send(Command::Connect {
                    client: 11,
                    sequence: 0
                })
                .error
                .is_none()
            );
            assert!(
                send(Command::Execute {
                    client: 11,
                    sequence: 1,
                    request: Box::new(Request::Input(Packet::key_scanned(
                        keyboard, 117, 64, 0, false,
                    ))),
                })
                .error
                .is_none()
            );
            let packets = delivered.borrow().packets.len();
            assert!(delivered.borrow().held.contains(&(keyboard, 117)));
            let mut changed = profile.seats[0].cursor.clone();
            changed.keyboard.application_focus = match original {
                ApplicationFocus::Compatibility => ApplicationFocus::NativeControls,
                ApplicationFocus::NativeControls => ApplicationFocus::Compatibility,
            };
            let rejected = send(Command::Execute {
                client: 11,
                sequence: 2,
                request: Box::new(Request::Settings {
                    index: 0,
                    settings: changed,
                    gui: None,
                }),
            });
            assert!(rejected.error.unwrap().contains("stop and restart"));
            assert_eq!(delivered.borrow().packets.len(), packets);
            assert!(delivered.borrow().held.contains(&(keyboard, 117)));
            assert_eq!(delivered.borrow().settings_calls, 0);
            assert!(
                send(Command::Execute {
                    client: 11,
                    sequence: 3,
                    request: Box::new(Request::Input(Packet::key_scanned(
                        keyboard, 117, 64, 0, true,
                    ))),
                })
                .error
                .is_none()
            );
            assert!(!delivered.borrow().held.contains(&(keyboard, 117)));
            // A later ordinary settings update still uses the original focus policy.
            let mut unchanged = profile.seats[0].cursor.clone();
            unchanged.size = 36.;
            assert!(
                send(Command::Execute {
                    client: 11,
                    sequence: 4,
                    request: Box::new(Request::Settings {
                        index: 0,
                        settings: unchanged,
                        gui: None,
                    }),
                })
                .error
                .is_none()
            );
            assert_eq!(delivered.borrow().settings_calls, 1);
            assert_eq!(
                users
                    .require_active(0)
                    .unwrap()
                    .seat
                    .cursor
                    .keyboard
                    .application_focus,
                original
            );
        }
    }

    #[test]
    fn disconnect_revokes_and_retries_saved_releases_without_harming_other_clients() {
        let profile: Profile = serde_json::from_value(serde_json::json!({
            "version":1,"concurrent":true,"seats":[
                {"id":"a","name":"A","mouse":"virtual://mouse/1","keyboard":"virtual://keyboard/1","target_pid":10},
                {"id":"b","name":"B","mouse":"virtual://mouse/2","keyboard":"virtual://keyboard/2","target_pid":20}
            ]
        })).unwrap();
        let seats = profile.resolve(&[]).unwrap();
        let keyboards = [seats[0].keyboard.unwrap(), seats[1].keyboard.unwrap()];
        let delivered = Rc::new(RefCell::new(Delivered::default()));
        let annotations = crate::annotation_runtime::Runtime::new(
            Box::new(ReleaseEngine(delivered.clone())),
            &profile,
            seats.clone(),
        )
        .unwrap();
        let mut router =
            crate::magnifier_runtime::Runtime::new(annotations, &profile, seats.clone()).unwrap();
        let mut users = virtual_users::Runtime::new(&profile, seats.clone(), 99).unwrap();
        let mut automation =
            macro_studio::Runtime::new_with_ids(seats, vec!["a".into(), "b".into()]);
        let mut access = Runtime::new();
        let configure = |enabled, duplicate| Command::Configure {
            enabled,
            user_ids: if duplicate {
                vec!["a".into(), "a".into()]
            } else {
                vec!["a".into(), "b".into()]
            },
            allowed_pids: vec![10, 20],
            capture: false,
            desktop_capture: false,
            session_control: false,
            app_preferences: false,
        };
        let mut send = |command| {
            access
                .command(
                    command,
                    &mut users,
                    &mut automation,
                    &mut router,
                    &profile,
                    99,
                )
                .unwrap()
                .unwrap()
        };
        assert!(send(configure(true, false)).error.is_none());
        for client in [11, 22] {
            assert!(
                send(Command::Connect {
                    client,
                    sequence: 0
                })
                .error
                .is_none()
            );
        }
        let key = |client, sequence, user, code| Command::Execute {
            client,
            sequence,
            request: Box::new(Request::Input(Packet::key_scanned(
                keyboards[user],
                code,
                code as i32 - 53,
                0,
                false,
            ))),
        };
        let first = send(key(11, 1, 0, 117));
        assert!(first.error.is_none(), "{:?}", first.error);
        assert!(send(key(11, 2, 1, 119)).error.is_none());
        assert!(send(key(22, 1, 1, 118)).error.is_none());
        delivered.borrow_mut().fail_up = Some((keyboards[0], 117));
        let retired = send(Command::Disconnect {
            client: 11,
            sequence: 3,
        });
        assert!(retired.error.is_none());
        assert_eq!(retired.clients, [22]);
        assert_eq!(retired.cleanup.len(), 1);
        assert_eq!(retired.cleanup[0].pending_owners, 1);
        assert!(delivered.borrow().held.contains(&(keyboards[0], 117)));
        assert!(!delivered.borrow().held.contains(&(keyboards[1], 119)));
        assert!(delivered.borrow().held.contains(&(keyboards[1], 118)));
        let count = delivered.borrow().packets.len();
        assert!(send(key(11, 4, 0, 116)).error.unwrap().contains("revoked"));
        assert!(
            send(Command::Inspect {
                client: 11,
                sequence: 5
            })
            .error
            .unwrap()
            .contains("revoked")
        );
        assert!(
            send(Command::Connect {
                client: 11,
                sequence: 0
            })
            .error
            .unwrap()
            .contains("cleanup")
        );
        assert_eq!(delivered.borrow().packets.len(), count);
        let now = Instant::now() + Duration::from_secs(1);
        assert!(
            access
                .poll_cleanup(now, &mut automation, router.as_mut())
                .is_none()
        ); // Same error: no telemetry flood.
        assert_eq!(access.cleanup.snapshot()[0].attempts, 2);
        delivered.borrow_mut().fail_up = None;
        let Some(Event::Agent(state)) = access.poll_cleanup(
            now + Duration::from_secs(1),
            &mut automation,
            router.as_mut(),
        ) else {
            panic!("Expected cleanup transition");
        };
        assert!(!state.acknowledged);
        assert!(state.cleanup.is_empty());
        assert_eq!(state.clients, [22]);
        assert!(delivered.borrow().held.contains(&(keyboards[1], 118)));
        let count = delivered.borrow().packets.len();
        assert!(
            access
                .poll_cleanup(
                    now + Duration::from_secs(2),
                    &mut automation,
                    router.as_mut()
                )
                .is_none()
        );
        assert_eq!(delivered.borrow().packets.len(), count);

        // Invalid policy is transactional. A valid disabling policy commits to
        // both layers and revokes all clients even when their release fails.
        let invalid = access
            .command(
                configure(true, true),
                &mut users,
                &mut automation,
                &mut router,
                &profile,
                99,
            )
            .unwrap()
            .unwrap();
        assert!(invalid.error.is_some());
        assert!(invalid.policy.enabled);
        assert_eq!(invalid.clients, [22]);
        delivered.borrow_mut().fail_up = Some((keyboards[1], 118));
        let disabled = access
            .command(
                configure(false, false),
                &mut users,
                &mut automation,
                &mut router,
                &profile,
                99,
            )
            .unwrap()
            .unwrap();
        assert!(disabled.error.is_none());
        assert!(!disabled.policy.enabled);
        assert!(disabled.clients.is_empty());
        assert_eq!(disabled.cleanup[0].client, 22);
        assert!(access.require(22).is_err());
        let denied = automation
            .agent_action(
                22,
                Action::Packet {
                    user: 1,
                    packet: Packet::key_scanned(keyboards[1], 116, 63, 0, false),
                },
                router.as_mut(),
            )
            .unwrap_err();
        assert!(
            denied
                .downcast_ref::<macro_studio::AgentScopeViolation>()
                .is_some()
        );
        delivered.borrow_mut().fail_up = None;
        assert!(
            access
                .poll_cleanup(
                    now + Duration::from_secs(3),
                    &mut automation,
                    router.as_mut()
                )
                .is_some()
        );
        assert!(access.cleanup.snapshot().is_empty());
        assert!(delivered.borrow().held.is_empty());
        for (device, code) in [
            (keyboards[0], 117),
            (keyboards[1], 118),
            (keyboards[1], 119),
        ] {
            assert_eq!(
                delivered
                    .borrow()
                    .packets
                    .iter()
                    .filter(|packet| packet.device == device
                        && packet.data == code
                        && packet.flags & 1 != 0)
                    .count(),
                1
            );
        }
    }

    struct MapperExecuteFixture {
        profile: Profile,
        users: virtual_users::Runtime,
        automation: macro_studio::Runtime,
        router: crate::magnifier_runtime::Runtime,
        access: Runtime,
        delivered: Rc<RefCell<Delivered>>,
        keyboard: u32,
    }
    impl MapperExecuteFixture {
        fn new(session_control: bool) -> Self {
            let profile: Profile = serde_json::from_value(serde_json::json!({
                "version":1, "desktop":{"allowed_pids":[10]}, "seats":[
                    {"id":"a", "name":"A", "mouse":"virtual://mouse/1",
                     "keyboard":"virtual://keyboard/1", "target_pid":10}
                ]
            }))
            .unwrap();
            let seats = profile.resolve(&[]).unwrap();
            let keyboard = seats[0].keyboard.unwrap();
            let delivered = Rc::new(RefCell::new(Delivered::default()));
            let annotations = crate::annotation_runtime::Runtime::new(
                Box::new(ReleaseEngine(delivered.clone())),
                &profile,
                seats.clone(),
            )
            .unwrap();
            let mut router =
                crate::magnifier_runtime::Runtime::new(annotations, &profile, seats.clone())
                    .unwrap();
            let users = virtual_users::Runtime::new(&profile, seats.clone(), 99).unwrap();
            let mut automation = macro_studio::Runtime::new_with_ids(seats, vec!["a".into()]);
            automation
                .initialize_mapper_profile(&profile, router.as_mut())
                .unwrap();
            let mut fixture = Self {
                profile,
                users,
                automation,
                router,
                access: Runtime::new(),
                delivered,
                keyboard,
            };
            assert!(
                fixture
                    .send(Command::Configure {
                        enabled: true,
                        user_ids: vec!["a".into()],
                        allowed_pids: vec![10],
                        capture: false,
                        desktop_capture: false,
                        session_control,
                        app_preferences: false,
                    })
                    .error
                    .is_none()
            );
            assert!(
                fixture
                    .send(Command::Connect {
                        client: 11,
                        sequence: 0
                    })
                    .error
                    .is_none()
            );
            fixture
        }
        fn send(&mut self, command: Command) -> State {
            self.access
                .command(
                    command,
                    &mut self.users,
                    &mut self.automation,
                    &mut self.router,
                    &self.profile,
                    99,
                )
                .unwrap()
                .unwrap()
        }
        fn mapper(&mut self, sequence: u64, command: crate::mapper_runtime::Command) -> State {
            self.send(Command::Execute {
                client: 11,
                sequence,
                request: Box::new(Request::Automation(
                    crate::automation_runtime::Command::Mapper(command),
                )),
            })
        }
    }
    fn mapper_own_receipt(state: &State) -> &crate::automation_runtime::State {
        state
            .events
            .iter()
            .rev()
            .find_map(|event| match event {
                Event::Automation(state) if state.acknowledged && state.mapper.acknowledged => {
                    Some(state)
                }
                _ => None,
            })
            .expect("command must retain its own authoritative mapper acknowledgement")
    }

    #[test]
    fn connected_mapper_domain_failure_is_negative_ack_with_retained_state_and_ordinary_hold() {
        use crate::mapper_runtime::Command as MapperCommand;
        let mut fixture = MapperExecuteFixture::new(true);
        let ordinary = fixture.send(Command::Execute {
            client: 11,
            sequence: 1,
            request: Box::new(Request::Input(Packet::key_scanned(
                fixture.keyboard,
                68,
                32,
                0,
                false,
            ))),
        });
        assert!(ordinary.error.is_none());
        let definition = serde_json::from_value(serde_json::json!({
            "version":1,
            "nodes":[
                {"id":"source", "kind":{"type":"key", "user":"a", "key":65}},
                {"id":"sink", "kind":{"type":"sink", "user":"a", "channel":{"type":"key_hold", "key":66}}}
            ],
            "edges":[{"from":{"node":"source", "port":"value"}, "to":{"node":"sink", "port":"value"}}]
        })).unwrap();
        let applied = fixture.mapper(
            2,
            MapperCommand::Apply {
                definition,
                expected_revision: 0,
                gui: None,
            },
        );
        assert!(applied.error.is_none());
        assert!(mapper_own_receipt(&applied).mapper.error.is_none());
        let armed = fixture.mapper(
            3,
            MapperCommand::Arm {
                expected_revision: 1,
                gui: None,
            },
        );
        assert!(armed.error.is_none());
        assert!(mapper_own_receipt(&armed).mapper.armed);
        let before = fixture.automation.state(false).mapper;
        let packets = fixture.delivered.borrow().packets.len();
        assert_eq!(fixture.automation.agent_pending_owners(11), 1);

        let rejected = fixture.mapper(
            4,
            MapperCommand::Apply {
                definition: mux_core::mapper::Definition::default(),
                expected_revision: 0,
                gui: None,
            },
        );
        assert!(rejected.acknowledged);
        assert!(
            rejected
                .error
                .as_ref()
                .is_some_and(|error| error.contains("Stale mapper revision"))
        );
        let receipt = mapper_own_receipt(&rejected);
        assert!(receipt.error.is_some());
        assert!(
            receipt
                .mapper
                .error
                .as_ref()
                .is_some_and(|error| error.contains("Stale mapper revision"))
        );
        assert!(receipt.mapper.armed);
        assert_eq!(receipt.mapper.revision, before.revision);
        assert_eq!(receipt.mapper.run, before.run);
        assert_eq!(receipt.mapper.definition, before.definition);
        assert!(receipt.mapper.reply_to.is_none());
        assert_eq!(fixture.delivered.borrow().packets.len(), packets);
        assert!(
            fixture
                .delivered
                .borrow()
                .held
                .contains(&(fixture.keyboard, 68))
        );
        assert_eq!(fixture.automation.agent_pending_owners(11), 1);

        let disarmed = fixture.mapper(
            5,
            MapperCommand::Disarm {
                expected_revision: 1,
                gui: None,
            },
        );
        assert!(disarmed.error.is_none());
        let receipt = mapper_own_receipt(&disarmed);
        assert!(receipt.mapper.error.is_none() && !receipt.mapper.armed);
        assert!(
            fixture
                .delivered
                .borrow()
                .held
                .contains(&(fixture.keyboard, 68))
        );
        assert!(
            fixture
                .send(Command::Execute {
                    client: 11,
                    sequence: 6,
                    request: Box::new(Request::ReleaseAll)
                })
                .error
                .is_none()
        );
        assert!(fixture.delivered.borrow().held.is_empty());
    }

    #[test]
    fn connected_mapper_inspect_needs_no_session_change_grant() {
        use crate::mapper_runtime::Command as MapperCommand;
        let mut fixture = MapperExecuteFixture::new(false);
        let inspected = fixture.mapper(1, MapperCommand::Inspect);
        assert!(inspected.error.is_none());
        let receipt = mapper_own_receipt(&inspected);
        assert!(!receipt.mapper.armed && receipt.mapper.error.is_none());
        let denied = fixture.mapper(
            2,
            MapperCommand::Arm {
                expected_revision: 0,
                gui: None,
            },
        );
        assert!(denied.error.is_some());
        assert!(fixture.mapper(3, MapperCommand::Inspect).error.is_none());
        assert!(fixture.delivered.borrow().packets.is_empty());
    }

    #[test]
    fn retries_are_due_bounded_and_fair_without_discarding_failed_owners() {
        let now = Instant::now();
        let mut cleanup = RetiredClients::default();
        for client in [1, 2] {
            cleanup.enroll(client, now);
            assert!(cleanup.attempt(client, now, || (1, Err(anyhow::anyhow!("Still pending")))));
        }
        let mut visited = Vec::new();
        assert!(!cleanup.poll(now, |_| panic!("Not due")));
        for step in 1..=4 {
            cleanup.poll(now + Duration::from_secs(step), |client| {
                visited.push(client);
                (1, Err(anyhow::anyhow!("Still pending")))
            });
        }
        assert_eq!(visited, [1, 2, 1, 2]);
        assert_eq!(cleanup.snapshot().len(), 2);
        assert!(
            cleanup
                .snapshot()
                .iter()
                .all(|row| row.attempts == 3 && row.pending_owners == 1)
        );
    }
}

#[cfg(test)]
mod cleanup_capacity_tests {
    use super::*;

    #[test]
    fn failed_retirements_keep_capacity_until_their_saved_releases_complete() {
        let mut access = Runtime::new();
        access.policy.enabled = true;
        let now = Instant::now();
        // Distinct retired identities cannot accumulate beyond the existing
        // admission budget, even though their physical pipes have closed.
        for client in 1..=MAX_CLIENT_SLOTS as u64 {
            access.connect(client).unwrap();
            access.cleanup.enroll(client, now);
            access.clients.remove(&client);
            access.cleanup.attempt(client, now, || {
                (1, Err(anyhow::anyhow!("Saved Up rejected")))
            });
        }
        assert!(
            access
                .connect(99)
                .unwrap_err()
                .to_string()
                .contains("capacity")
        );
        assert!(access.clients.is_empty());
        assert_eq!(access.cleanup.snapshot().len(), MAX_CLIENT_SLOTS);
        assert!(access.cleanup.attempt(1, now, || (0, Ok(()))));
        access.connect(99).unwrap();
        assert!(access.connect(100).is_err());
        assert_eq!(access.cleanup.snapshot().len(), MAX_CLIENT_SLOTS - 1);
        assert_eq!(access.clients, BTreeSet::from([99]));
    }
}

#[cfg(test)]
mod annotation_scope_tests {
    use super::*;
    use crate::annotation_runtime::{Command, ExportKind};

    #[test]
    fn per_user_and_global_annotation_capabilities_cannot_cross_scope() {
        let profile: Profile = serde_json::from_value(serde_json::json!({
            "version":1,"concurrent":true,"seats":[
                {"id":"a","name":"A","mouse":"virtual://mouse/1","keyboard":"virtual://keyboard/1","target_pid":1},
                {"id":"b","name":"B","mouse":"virtual://mouse/2","keyboard":"virtual://keyboard/2","target_pid":2}
            ]
        })).unwrap();
        let users =
            virtual_users::Runtime::new(&profile, profile.resolve(&[]).unwrap(), 99).unwrap();
        let mut access = Runtime::new();
        access.policy.user_ids = vec![profile.seats[0].identity()];
        assert!(
            access
                .authorize_annotation(&Command::ClearUser { user: 0 }, &users)
                .is_err()
        );
        access.policy.session_control = true;
        assert!(
            access
                .authorize_annotation(&Command::ClearUser { user: 0 }, &users)
                .is_ok()
        );
        for command in [
            Command::ClearUser { user: 1 },
            Command::ClearUser { user: usize::MAX },
            Command::ClearAll,
        ] {
            assert!(access.authorize_annotation(&command, &users).is_err());
        }
        let global_shortcut = Command::Configure {
            user: 0,
            settings: mux_core::annotation::Settings {
                clear_key: Some(0x77),
                clear_all: true,
                ..Default::default()
            },
        };
        assert!(
            access
                .authorize_annotation(&global_shortcut, &users)
                .is_err()
        );
        let global_clear_without_key = Command::Configure {
            user: 0,
            settings: mux_core::annotation::Settings {
                clear_all: true,
                ..Default::default()
            },
        };
        assert!(
            access
                .authorize_annotation(&global_clear_without_key, &users)
                .is_err()
        );
        let local_clear_without_key = Command::Configure {
            user: 0,
            settings: mux_core::annotation::Settings::default(),
        };
        assert!(
            access
                .authorize_annotation(&local_clear_without_key, &users)
                .is_ok()
        );
        access.policy.user_ids.push(profile.seats[1].identity());
        assert!(
            access
                .authorize_annotation(&Command::ClearAll, &users)
                .is_ok()
        );
        assert!(
            access
                .authorize_annotation(&global_shortcut, &users)
                .is_ok()
        );
        assert!(
            access
                .authorize_annotation(&global_clear_without_key, &users)
                .is_ok()
        );
        assert!(
            access
                .authorize_annotation(
                    &Command::Export {
                        user: 0,
                        path: "unwritten.png".into(),
                        kind: ExportKind::InkOnly,
                        ink_scope: crate::annotation_runtime::InkScope::AllUsers,
                        desktop_region: None,
                    },
                    &users
                )
                .is_err()
        );
    }

    #[test]
    fn magnifier_configuration_uses_separate_session_and_capture_grants() {
        use crate::magnifier_runtime::Command;
        let mut policy = Policy::default();
        let off = Command::Configure {
            user: 0,
            settings: mux_core::magnifier::Settings::default(),
        };
        assert!(authorize_magnifier_policy(&policy, &off).is_err());
        policy.session_control = true;
        assert!(authorize_magnifier_policy(&policy, &off).is_ok());

        let selected = Command::Configure {
            user: 0,
            settings: mux_core::magnifier::Settings {
                mode: mux_core::magnifier::Mode::Lens,
                source: mux_core::magnifier::Source::SelectedTarget,
                ..Default::default()
            },
        };
        assert!(authorize_magnifier_policy(&policy, &selected).is_err());
        policy.capture = true;
        assert!(authorize_magnifier_policy(&policy, &selected).is_ok());
        let desktop = Command::Configure {
            user: 0,
            settings: mux_core::magnifier::Settings {
                mode: mux_core::magnifier::Mode::Lens,
                source: mux_core::magnifier::Source::Desktop,
                ..Default::default()
            },
        };
        assert!(authorize_magnifier_policy(&policy, &desktop).is_err());
    }

    #[test]
    fn magnifier_refresh_uses_capture_grant_without_session_control() {
        use crate::magnifier_runtime::Command;
        let mut policy = Policy::default();
        assert!(authorize_magnifier_policy(&policy, &Command::Refresh { user: 0 }).is_err());
        policy.capture = true;
        assert!(authorize_magnifier_policy(&policy, &Command::Refresh { user: 0 }).is_ok());
        assert!(
            authorize_magnifier_policy(
                &policy,
                &Command::Export {
                    user: 0,
                    path: "frame.png".into()
                }
            )
            .is_ok()
        );
    }
}
impl Runtime {
    pub fn new() -> Self {
        Self {
            policy: Policy::default(),
            clients: BTreeSet::new(),
            sequence: BTreeMap::new(),
            created_virtual_users: BTreeMap::new(),
            capture: None,
            export: None,
            annotation_export: None,
            generation: 0,
            mode: None,
            automatic: false,
            cleanup: Default::default(),
        }
    }
    fn user(&self, user: usize, users: &virtual_users::Runtime) -> Result<()> {
        let slot = users.require_active(user)?;
        ensure!(
            self.policy.user_ids.contains(&slot.seat.identity()),
            "This user is not permitted by Agent Access"
        );
        let seat = users.resolved(user)?;
        ensure!(
            seat.mouse & mux_core::emulated::MASK == mux_core::emulated::BASE
                && seat
                    .keyboard
                    .is_none_or(|k| k & mux_core::emulated::MASK == mux_core::emulated::BASE),
            "Agent Access requires virtual devices"
        );
        Ok(())
    }
    fn target(&self, user: usize, router: &dyn Engine) -> Result<mux_core::routing::Window> {
        let window = router
            .selected_window(user)?
            .context("Select an allowed application window first")?;
        ensure!(
            self.policy.allowed_pids.contains(&window.pid),
            "Selected target is outside Agent Access process permissions"
        );
        Ok(window)
    }
    pub fn snapshot(
        &self,
        client: u64,
        sequence: u64,
        users: &virtual_users::Runtime,
        automation: &macro_studio::Runtime,
        router: &mut crate::magnifier_runtime::Runtime,
        base: &Profile,
    ) -> Result<State> {
        let (_, states) = router.tick()?;
        let mut profile = base.clone();
        let mut user_state = users.state();
        let annotation = router.annotation_state();
        let magnifier = router.magnifier_state();
        for user in &annotation.users {
            if let Some(slot) = user_state.slots.get_mut(user.user) {
                slot.seat.annotation = user.settings.clone();
            }
        }
        for user in &magnifier.users {
            if let Some(slot) = user_state.slots.get_mut(user.user) {
                slot.seat.magnifier = user.settings.clone();
            }
        }
        profile.seats = user_state
            .slots
            .iter()
            .map(|slot| slot.seat.clone())
            .collect();
        if let (Some(desktop), Some((mode, multi_keyboard))) = (&mut profile.desktop, self.mode) {
            desktop.mode = mode;
            desktop.multi_keyboard = multi_keyboard;
        }
        profile.replication = mux_core::replication::Definition::from_resolved(
            &profile.seats,
            &automation.state(false).edges,
        )?;
        Ok(State {
            authority: Some(WorkerAuthority {
                policy_generation: self.generation,
                active_user_ids: user_state
                    .slots
                    .iter()
                    .filter(|slot| slot.active)
                    .map(|slot| slot.seat.identity())
                    .collect(),
            }),
            acknowledged: true,
            client,
            sequence,
            policy: self.policy.clone(),
            clients: self.clients.iter().copied().collect(),
            cleanup: self.cleanup.snapshot(),
            events: vec![
                if router.paused() {
                    Event::Paused
                } else {
                    Event::Resumed
                },
                Event::Configured(profile.seats.len()),
                Event::VirtualUsers(user_state),
                Event::State(states),
                Event::UserPauseStates(
                    automation
                        .user_pause_states(router)
                        .into_iter()
                        .filter(|state| {
                            users.require_active(state.user).is_ok_and(|slot| {
                                self.policy.user_ids.contains(&slot.seat.identity())
                            })
                        })
                        .collect(),
                ),
                Event::Automation(automation.state(false)),
                Event::Output(automation.output_state()),
                Event::MacroStudio(automation.studio_state(None)),
                Event::Annotation(annotation),
                Event::Magnifier(magnifier),
            ],
            profile: Some(profile),
            ..Default::default()
        })
    }
    pub fn command(
        &mut self,
        command: Command,
        users: &mut virtual_users::Runtime,
        automation: &mut macro_studio::Runtime,
        router: &mut crate::magnifier_runtime::Runtime,
        base: &Profile,
        controller: u32,
    ) -> Result<Option<State>> {
        let (client, sequence) = match &command {
            Command::Configure { .. } => (0, 0),
            Command::Inspect { client, sequence }
            | Command::Connect { client, sequence }
            | Command::Execute {
                client, sequence, ..
            }
            | Command::Capture {
                client, sequence, ..
            }
            | Command::Disconnect { client, sequence } => (*client, *sequence),
        };
        let mut extra_events = Vec::new();
        let mut monitor_receipt = None;
        let result = (|| -> Result<bool> {
            if client != 0 && !matches!(command, Command::Connect { .. }) {
                ensure!(
                    sequence > self.sequence.get(&client).copied().unwrap_or(0),
                    "Stale agent command sequence"
                );
                self.sequence.insert(client, sequence);
            }
            match command {
                Command::Configure {
                    enabled,
                    user_ids,
                    allowed_pids,
                    capture,
                    desktop_capture,
                    session_control,
                    app_preferences,
                } => {
                    let policy = Policy {
                        enabled,
                        user_ids,
                        allowed_pids,
                        capture,
                        desktop_capture,
                        session_control,
                        app_preferences,
                    };
                    if enabled {
                        ensure!(
                            base.desktop.is_some() || base.concurrent,
                            "Agent Access requires desktop cursor routing or concurrent assigned routing"
                        );
                        ensure!(
                            !policy.user_ids.is_empty() && !policy.allowed_pids.is_empty(),
                            "Choose virtual users and explicit process permissions"
                        );
                        ensure!(
                            policy.allowed_pids.iter().all(|pid| *pid != 0
                                && *pid != controller
                                && *pid != std::process::id()),
                            "Controller processes cannot be agent targets"
                        );
                        if let Some(desktop) = &base.desktop {
                            ensure!(
                                desktop.allowed_pids.is_empty()
                                    || policy
                                        .allowed_pids
                                        .iter()
                                        .all(|p| desktop.allowed_pids.contains(p)),
                                "Agent targets must already be allowed by the GUI session"
                            );
                        }
                        for id in &policy.user_ids {
                            let slot = users
                                .state()
                                .slots
                                .into_iter()
                                .find(|s| s.active && s.seat.identity() == *id)
                                .context("Unknown or removed permitted user")?;
                            let seat = users.resolved(slot.user)?;
                            ensure!(
                                seat.mouse & mux_core::emulated::MASK == mux_core::emulated::BASE
                                    && seat.keyboard.is_none_or(|k| k & mux_core::emulated::MASK
                                        == mux_core::emulated::BASE),
                                "Agent Access requires virtual devices"
                            );
                        }
                    }
                    let generation = self
                        .generation
                        .checked_add(1)
                        .context("Agent policy generation exhausted")?;
                    automation.agent_policy(&policy, users)?;
                    // The validated policy is already installed in automation.
                    // Commit it visibly before cleanup; failed saved releases do
                    // not restore authorization or roll back only half a policy.
                    self.generation = generation;
                    self.policy = policy;
                    self.automatic = base.desktop.is_some();
                    for client in self.clients.iter().copied().collect::<Vec<_>>() {
                        self.retire_client(client, automation, router);
                    }
                    self.sequence.clear();
                    self.clear_virtual_user_receipts();
                }
                Command::Connect { .. } => {
                    self.connect(client)?;
                    automation.mapper_agent_connected(client);
                }
                Command::Disconnect { .. } => {
                    self.retire_client(client, automation, router);
                }
                Command::Inspect { .. } => {
                    ensure!(
                        self.policy.enabled && self.clients.contains(&client),
                        "Agent connection was revoked"
                    );
                }
                Command::Capture { user, query, .. } => {
                    self.require(client)?;
                    ensure!(
                        self.policy.capture,
                        "Target capture is disabled in Agent Access"
                    );
                    self.user(user, users)?;
                    let window = self.target(user, router)?;
                    self.begin_capture(
                        client,
                        sequence,
                        user,
                        window,
                        users.require_active(user)?.seat.identity(),
                        query,
                    )?;
                    return Ok(false);
                }
                Command::Execute { request, .. } => {
                    self.require(client)?;
                    if let Request::InspectMapperMonitors { request_id } = request.as_ref() {
                        monitor_receipt = Some(crate::mapper_runtime::MonitorReceipt {
                            request_id: *request_id,
                            snapshot: automation.mapper_monitor_snapshot(
                                crate::mapper_runtime::Actor::Agent(client),
                            ),
                        });
                        return Ok(true);
                    }
                    if let Request::Automation(crate::automation_runtime::Command::Mapper(
                        command,
                    )) = request.as_ref()
                    {
                        use crate::mapper_runtime::Command as MapperCommand;
                        ensure!(
                            match command {
                                MapperCommand::Inspect => true,
                                MapperCommand::Apply { gui, .. }
                                | MapperCommand::Arm { gui, .. }
                                | MapperCommand::Disarm { gui, .. } => gui.is_none(),
                            },
                            "GUI mapper correlation is unavailable to connected clients"
                        );
                    }
                    if let Request::Automation(command) = request.as_ref()
                        && matches!(
                            command,
                            crate::automation_runtime::Command::Graph { .. }
                                | crate::automation_runtime::Command::CancelReplication { .. }
                                | crate::automation_runtime::Command::Forwarding { .. }
                                | crate::automation_runtime::Command::Mapper(
                                    crate::mapper_runtime::Command::Apply { .. }
                                        | crate::mapper_runtime::Command::Arm { .. }
                                        | crate::mapper_runtime::Command::Disarm { .. }
                                )
                        )
                    {
                        self.authorize_graph_mutation(client, users, router, base)?;
                    }
                    if let Request::VirtualUsers(command) = request.as_ref()
                        && !matches!(command, virtual_users::Command::Inspect)
                    {
                        self.authorize_virtual_users_mutation(
                            client, command, users, router, base,
                        )?;
                    }
                    if let Request::Magnifier(crate::magnifier_runtime::Command::Export {
                        user,
                        path,
                    }) = request.as_ref()
                    {
                        self.begin_export(client, sequence, *user, path.clone(), users, router)?;
                        return Ok(false);
                    }
                    if let Request::Annotation(
                        command @ crate::annotation_runtime::Command::Export { .. },
                    ) = request.as_ref()
                    {
                        self.begin_annotation_export(
                            (client, sequence),
                            command.clone(),
                            users,
                            router,
                        )?;
                        return Ok(false);
                    }
                    if let Request::MacroStudio(macro_studio::Command::Inspect { user }) =
                        request.as_ref()
                    {
                        if let Some(user) = user {
                            self.user(*user, users)?;
                        }
                        extra_events.push(Event::MacroStudio(automation.studio_state(*user)));
                    } else if matches!(
                        request.as_ref(),
                        Request::Automation(crate::automation_runtime::Command::Inspect)
                    ) {
                        extra_events.push(Event::Automation(automation.state(true)));
                    }
                    if let Request::SelectWindow(selection) = request.as_ref() {
                        ensure!(
                            base.desktop.as_ref().is_some_and(|desktop| desktop
                                .allowed_pids
                                .contains(&selection.pid)),
                            "Connected selection requires the PID in the explicit GUI routing allowlist"
                        );
                    }
                    self.execute(
                        client,
                        *request,
                        users,
                        automation,
                        router,
                        &mut extra_events,
                    )?;
                }
            }
            Ok(true)
        })();
        if matches!(result, Ok(false)) {
            return Ok(None);
        }
        if let Some(receipt) = monitor_receipt {
            return Ok(Some(State {
                authority: Some(WorkerAuthority {
                    policy_generation: self.generation,
                    active_user_ids: users
                        .state()
                        .slots
                        .iter()
                        .filter(|slot| slot.active)
                        .map(|slot| slot.seat.identity())
                        .collect(),
                }),
                acknowledged: true,
                client,
                sequence,
                policy: self.policy.clone(),
                clients: self.clients.iter().copied().collect(),
                mapper_monitors: Some(receipt),
                ..State::default()
            }));
        }
        let mut state = self.snapshot(client, sequence, users, automation, router, base)?;
        if result.is_ok() {
            state.events.extend(extra_events);
        } else {
            // Only the rejected mapper command's own authoritative receipt is
            // retained on error; unrelated legacy partial events stay hidden.
            state.events.extend(extra_events.into_iter().filter(|event| {
                matches!(event, Event::Automation(state)
                    if state.acknowledged && state.mapper.acknowledged && state.mapper.error.is_some())
                    || matches!(event, Event::UserPause(state) if state.error.is_some())
            }));
        }
        state.error = result.err().map(|e| format!("{e:#}"));
        Ok(Some(state))
    }
    pub fn note_mode(&mut self, mode: mux_core::routing::Mode, multi_keyboard: bool) {
        self.mode = Some((mode, multi_keyboard));
    }
    fn connect(&mut self, client: u64) -> Result<()> {
        ensure!(
            client != 0 && self.policy.enabled,
            "Agent Access is disabled"
        );
        ensure!(
            !self.cleanup.pending.contains_key(&client),
            "Client input cleanup is still pending"
        );
        // A retired connection retains its slot until its saved releases finish.
        // Backpressure bounds snapshots/retry storage without dropping ownership.
        ensure!(
            self.clients.len() + self.cleanup.pending.len() < MAX_CLIENT_SLOTS,
            "Agent connection capacity includes pending input cleanup; wait for saved releases"
        );
        ensure!(self.clients.insert(client), "Client is already connected");
        Ok(())
    }
    fn retire_client(
        &mut self,
        client: u64,
        automation: &mut macro_studio::Runtime,
        router: &mut dyn Engine,
    ) {
        if !self.clients.contains(&client) {
            return;
        }
        let now = Instant::now();
        self.cleanup.enroll(client, now);
        self.clients.remove(&client);
        automation.mapper_agent_disconnected(client);
        self.sequence.remove(&client);
        self.created_virtual_users.remove(&client);
        self.cleanup.attempt(client, now, || {
            let result = automation.cancel_agent(client, router);
            let pending = automation.agent_pending_owners(client);
            automation.retire_agent_forwarding_tombstones(client);
            (pending, result)
        });
    }
    /// Called after the normal router tick has reconciled target exits. Only
    /// saved release actions are retried; no client command is replayed.
    pub fn poll_cleanup(
        &mut self,
        now: Instant,
        automation: &mut macro_studio::Runtime,
        router: &mut dyn Engine,
    ) -> Option<Event> {
        let changed = self.cleanup.poll(now, |client| {
            let result = automation.cancel_agent(client, router);
            let pending = automation.agent_pending_owners(client);
            automation.retire_agent_forwarding_tombstones(client);
            (pending, result)
        });
        changed.then(|| {
            Event::Agent(State {
                acknowledged: false,
                policy: self.policy.clone(),
                clients: self.clients.iter().copied().collect(),
                cleanup: self.cleanup.snapshot(),
                ..Default::default()
            })
        })
    }
    fn require(&self, client: u64) -> Result<()> {
        ensure!(
            self.policy.enabled && self.clients.contains(&client),
            "Agent connection was revoked"
        );
        Ok(())
    }
    fn remove_virtual_user_receipt(&mut self, id: &str) {
        self.created_virtual_users.retain(|_, created| {
            created.remove(id);
            !created.is_empty()
        });
    }
    fn clear_virtual_user_receipts(&mut self) {
        self.created_virtual_users.clear();
    }
    fn created_virtual_user(&self, client: u64, id: &str) -> bool {
        self.created_virtual_users
            .get(&client)
            .is_some_and(|created| created.contains(id))
    }
    fn authorize_virtual_users_mutation(
        &self,
        client: u64,
        command: &virtual_users::Command,
        users: &virtual_users::Runtime,
        router: &dyn Engine,
        base: &Profile,
    ) -> Result<()> {
        self.require(client)?;
        ensure!(
            self.policy.session_control,
            "Session changes are disabled in Agent Access"
        );
        let desktop = base
            .desktop
            .as_ref()
            .context("Connected virtual replicas require automatic desktop routing")?;
        ensure!(
            self.automatic,
            "Connected virtual replicas require automatic desktop routing"
        );
        ensure!(
            !desktop.allowed_pids.is_empty(),
            "Connected virtual replicas require an explicit nonempty desktop process allowlist"
        );
        ensure!(
            desktop
                .allowed_pids
                .iter()
                .all(|pid| self.policy.allowed_pids.contains(pid)),
            "Agent Access must permit every process in the desktop routing allowlist"
        );

        let (target_id, created_here) = match command {
            virtual_users::Command::Inspect => bail!("Inspect does not mutate virtual users"),
            virtual_users::Command::Start {
                source_id,
                target_pid,
                expected_revision,
                ..
            } => {
                ensure!(
                    expected_revision.is_some(),
                    "Connected virtual replica changes require the inspected roster revision"
                );
                let state = users.state();
                let source = state
                    .slots
                    .iter()
                    .find(|slot| slot.active && slot.seat.identity() == *source_id)
                    .context("Select an active source user")?;
                self.user(source.user, users)?;
                ensure!(
                    desktop.allowed_pids.contains(target_pid)
                        && self.policy.allowed_pids.contains(target_pid),
                    "Replica target PID must be in both the desktop and Agent Access process scopes"
                );
                (None, false)
            }
            virtual_users::Command::Stop {
                id,
                expected_revision,
            } => {
                ensure!(
                    expected_revision.is_some(),
                    "Connected virtual replica changes require the inspected roster revision"
                );
                let slot = users
                    .state()
                    .slots
                    .into_iter()
                    .find(|slot| slot.active && slot.seat.identity() == *id)
                    .context("Select an active generated user")?;
                ensure!(
                    slot.clone_of.is_some(),
                    "Original session users cannot be removed through clone lifecycle"
                );
                ensure!(
                    desktop.allowed_pids.contains(&slot.seat.target_pid)
                        && self.policy.allowed_pids.contains(&slot.seat.target_pid),
                    "Generated user target PID must remain in both authorized process scopes"
                );
                let created_here = self.created_virtual_user(client, id);
                (Some(id.as_str()), created_here)
            }
        };

        let active_slots = users.state().slots;
        let active_ids = authorize_virtual_user_grants(
            &active_slots,
            &self.policy.user_ids,
            if created_here { target_id } else { None },
        )?;
        let mut recipients = Vec::new();
        for slot in active_slots.iter().filter(|slot| slot.active) {
            let resolved = users.resolved(slot.user)?;
            ensure!(
                resolved.mouse & mux_core::emulated::MASK == mux_core::emulated::BASE
                    && resolved
                        .keyboard
                        .is_none_or(|keyboard| keyboard & mux_core::emulated::MASK
                            == mux_core::emulated::BASE),
                "Connected virtual replica changes cannot include physical or unavailable users"
            );
            if let Some(window) = router.selected_window(slot.user)? {
                recipients.push(window.pid);
            }
            if let Some(window) = router.keyboard_window(slot.user)? {
                recipients.push(window.pid);
            }
        }
        authorize_graph_scope(
            &self.policy,
            &desktop.allowed_pids,
            &active_ids,
            &recipients,
        )
    }
    fn require_all_users(&self, users: &virtual_users::Runtime) -> Result<()> {
        for slot in users.state().slots.iter().filter(|slot| slot.active) {
            self.user(slot.user, users)?;
        }
        Ok(())
    }
    fn authorize_graph_mutation(
        &self,
        client: u64,
        users: &virtual_users::Runtime,
        router: &dyn Engine,
        base: &Profile,
    ) -> Result<()> {
        self.require(client)?;
        ensure!(
            self.policy.session_control,
            "Session changes are disabled in Agent Access"
        );
        let desktop = base
            .desktop
            .as_ref()
            .context("Connected graph changes require automatic desktop routing")?;
        ensure!(
            self.automatic,
            "Connected graph changes require automatic desktop routing"
        );
        let active_users: Vec<_> = users
            .state()
            .slots
            .into_iter()
            .filter(|slot| slot.active)
            .map(|slot| slot.seat.identity())
            .collect();
        self.require_all_users(users)?;
        let mut recipients = Vec::new();
        for slot in users.state().slots.into_iter().filter(|slot| slot.active) {
            if let Some(window) = router.selected_window(slot.user)? {
                recipients.push(window.pid);
            }
            if let Some(window) = router.keyboard_window(slot.user)? {
                recipients.push(window.pid);
            }
        }
        authorize_graph_scope(
            &self.policy,
            desktop.allowed_pids.as_slice(),
            &active_users,
            &recipients,
        )
    }
    fn authorize_annotation(
        &self,
        command: &crate::annotation_runtime::Command,
        users: &virtual_users::Runtime,
    ) -> Result<()> {
        use crate::annotation_runtime::Command;
        ensure!(
            self.policy.session_control,
            "Session changes are disabled in Agent Access"
        );
        match command {
            Command::Configure { user, settings } => {
                self.user(*user, users)?;
                settings.validate()?;
                if settings.clear_all {
                    self.require_all_users(users)?;
                }
            }
            Command::ClearUser { user } => self.user(*user, users)?,
            Command::ClearAll => self.require_all_users(users)?,
            Command::Export { .. } => {
                bail!("Annotation export is unavailable through a live GUI connection")
            }
            Command::Inspect => (),
        }
        Ok(())
    }
    fn authorize_magnifier(
        &self,
        command: &crate::magnifier_runtime::Command,
        users: &virtual_users::Runtime,
        router: &crate::magnifier_runtime::Runtime,
    ) -> Result<()> {
        use crate::magnifier_runtime::Command;
        authorize_magnifier_policy(&self.policy, command)?;
        match command {
            Command::Inspect => Ok(()),
            Command::Configure { user, settings } => {
                self.user(*user, users)?;
                crate::magnifier_runtime::validate_settings(settings)?;
                if settings.mode != mux_core::magnifier::Mode::Off {
                    self.target(*user, router)?;
                }
                Ok(())
            }
            Command::Refresh { user } | Command::Export { user, .. } => self
                .require_capture_target(*user, users, router)
                .map(|_| ()),
        }
    }
    fn require_capture_target(
        &self,
        user: usize,
        users: &virtual_users::Runtime,
        router: &crate::magnifier_runtime::Runtime,
    ) -> Result<mux_core::routing::Window> {
        self.user(user, users)?;
        ensure!(
            self.policy.capture,
            "Target capture is disabled in Agent Access"
        );
        let magnifier = router.magnifier_state();
        let state = magnifier
            .users
            .get(user)
            .context("Unknown magnifier user")?;
        ensure!(
            state.settings.mode != mux_core::magnifier::Mode::Off
                && state.settings.source == mux_core::magnifier::Source::SelectedTarget,
            "Operation requires an enabled selected-target magnifier"
        );
        self.target(user, router)
    }
    fn begin_export(
        &mut self,
        client: u64,
        sequence: u64,
        user: usize,
        path: String,
        users: &virtual_users::Runtime,
        router: &crate::magnifier_runtime::Runtime,
    ) -> Result<()> {
        ensure!(
            self.export.is_none(),
            "A magnifier export is still in progress"
        );
        let window = self.require_capture_target(user, users, router)?;
        let user_id = users.require_active(user)?.seat.identity();
        let settings_generation = router
            .export_generation(user)
            .context("Unknown magnifier user")?;
        let frame = router.export_frame(user, window)?;
        let frame_generation = frame.generation;
        let job = crate::agent_export::ExportJob::start(frame, path)?;
        self.export = Some(PendingExport {
            client,
            sequence,
            user,
            user_id,
            window,
            policy_generation: self.generation,
            settings_generation,
            frame_generation,
            started: Instant::now(),
            replied: false,
            decided: false,
            approved: false,
            job,
        });
        Ok(())
    }

    fn export_still_authorized(
        &self,
        pending: &PendingExport,
        users: &virtual_users::Runtime,
        router: &crate::magnifier_runtime::Runtime,
    ) -> Result<()> {
        self.require(pending.client)?;
        ensure!(
            self.generation == pending.policy_generation,
            "Agent Access policy changed during magnifier export"
        );
        ensure!(
            users
                .require_active(pending.user)
                .is_ok_and(|slot| slot.seat.identity() == pending.user_id),
            "Magnifier export user changed or was removed"
        );
        ensure!(
            self.require_capture_target(pending.user, users, router)? == pending.window,
            "Selected target changed during magnifier export"
        );
        ensure!(
            router.export_generation(pending.user) == Some(pending.settings_generation),
            "Magnifier settings changed during export"
        );
        ensure!(
            router
                .export_frame(pending.user, pending.window)?
                .generation
                == pending.frame_generation,
            "Magnifier frame changed during export"
        );
        Ok(())
    }

    pub fn poll_export(
        &mut self,
        users: &virtual_users::Runtime,
        automation: &macro_studio::Runtime,
        router: &mut crate::magnifier_runtime::Runtime,
        base: &Profile,
    ) -> Result<Option<Event>> {
        let Some(mut pending) = self.export.take() else {
            return Ok(None);
        };
        let mut reply_error = None;

        if !pending.decided {
            if pending.started.elapsed() >= Duration::from_secs(5) {
                let _ = pending.job.approve(false);
                pending.decided = true;
                reply_error = Some("Magnifier export timed out before authorization".to_owned());
            } else if let Some(ready) = pending.job.poll_ready() {
                match ready {
                    Ok(_) => match self.export_still_authorized(&pending, users, router) {
                        Ok(()) => {
                            if let Err(error) = pending.job.approve(true) {
                                reply_error =
                                    Some(format!("Magnifier export approval failed: {error:#}"));
                            } else {
                                pending.approved = true;
                            }
                        }
                        Err(error) => {
                            let _ = pending.job.approve(false);
                            reply_error =
                                Some(format!("Magnifier export authorization failed: {error:#}"));
                        }
                    },
                    Err(error) => {
                        let _ = pending.job.approve(false);
                        reply_error =
                            Some(format!("Magnifier export preparation failed: {error:#}"));
                    }
                }
                pending.decided = true;
            }
        }

        let completed = pending.job.poll_done();
        if let Some(result) = completed {
            if !pending.replied {
                let (export, error) = match result {
                    Ok(export) => match self.export_still_authorized(&pending, users, router) {
                        Ok(()) => (Some(export), None),
                        Err(error) => (
                            None,
                            Some(format!(
                                "Magnifier export finished after its authorization changed: {error:#}; an already committed file cannot be rolled back"
                            )),
                        ),
                    },
                    Err(error) => (
                        None,
                        Some(reply_error.take().unwrap_or_else(|| format!("{error:#}"))),
                    ),
                };
                return self.export_reply(
                    pending,
                    (export, error),
                    users,
                    automation,
                    router,
                    base,
                );
            }
            return Ok(None);
        }

        if let Some(error) = reply_error
            && !pending.replied
        {
            pending.replied = true;
            let event = self.export_event(
                (pending.client, pending.sequence),
                (None, Some(error)),
                users,
                automation,
                router,
                base,
            )?;
            self.export = Some(pending);
            return Ok(Some(event));
        }
        if pending.approved
            && !pending.replied
            && pending.started.elapsed() >= Duration::from_secs(5)
        {
            pending.replied = true;
            let event = self.export_event(
                (pending.client, pending.sequence),
                (None, Some(
                    "Magnifier export exceeded the completion timeout after authorization; the file may still be published"
                        .to_owned(),
                )),
                users,
                automation,
                router,
                base,
            )?;
            // Keep the single export slot occupied until the worker actually finishes.
            self.export = Some(pending);
            return Ok(Some(event));
        }
        self.export = Some(pending);
        Ok(None)
    }

    fn export_reply(
        &mut self,
        pending: PendingExport,
        outcome: (Option<crate::magnifier_runtime::ExportInfo>, Option<String>),
        users: &virtual_users::Runtime,
        automation: &macro_studio::Runtime,
        router: &mut crate::magnifier_runtime::Runtime,
        base: &Profile,
    ) -> Result<Option<Event>> {
        self.export_event(
            (pending.client, pending.sequence),
            outcome,
            users,
            automation,
            router,
            base,
        )
        .map(Some)
    }

    fn export_event(
        &self,
        tag: (u64, u64),
        outcome: (Option<crate::magnifier_runtime::ExportInfo>, Option<String>),
        users: &virtual_users::Runtime,
        automation: &macro_studio::Runtime,
        router: &mut crate::magnifier_runtime::Runtime,
        base: &Profile,
    ) -> Result<Event> {
        let (export, error) = outcome;
        let mut state = self.snapshot(tag.0, tag.1, users, automation, router, base)?;
        let mut magnifier = router.magnifier_state();
        magnifier.acknowledged = true;
        magnifier.export = export;
        magnifier.error.clone_from(&error);
        state.events.push(Event::Magnifier(magnifier));
        state.error = error;
        Ok(Event::Agent(state))
    }
    fn execute(
        &mut self,
        client: u64,
        request: Request,
        users: &mut virtual_users::Runtime,
        automation: &mut macro_studio::Runtime,
        router: &mut crate::magnifier_runtime::Runtime,
        extra_events: &mut Vec<Event>,
    ) -> Result<()> {
        match request {
            Request::Input(packet) => {
                let user = self.packet_user(packet, users)?;
                automation.agent_action(client, Action::Packet { user, packet }, router)?;
            }
            Request::Batch(packets) => {
                ensure!(packets.len() <= 256, "Input batch is too large");
                let actions = packets
                    .into_iter()
                    .map(|packet| {
                        packet.validate()?;
                        let user = self.packet_user(packet, users)?;
                        Ok(Action::Packet { user, packet })
                    })
                    .collect::<Result<Vec<_>>>()?;
                for action in &actions {
                    automation.preflight_agent_action(client, action, router)?;
                }
                for action in actions {
                    automation.agent_action(client, action, router)?;
                }
            }
            Request::Text { user, text } => {
                self.user(user, users)?;
                automation.agent_action(client, Action::Text { user, text }, router)?;
            }
            Request::MoveTo { user, x, y } => {
                self.user(user, users)?;
                automation.agent_action(client, Action::MoveTo { user, x, y }, router)?;
            }
            Request::Tap { device, key } => {
                let packet = mux_windows::keyboard::packet_code(device, key, false)?;
                let user = self.packet_user(packet, users)?;
                automation.agent_action(client, Action::Packet { user, packet }, router)?;
                let up = mux_windows::keyboard::packet_code(device, key, true)?;
                automation.agent_action(client, Action::Packet { user, packet: up }, router)?;
            }
            Request::ReleaseAll => automation.cancel_agent(client, router)?,
            Request::Stop => {
                self.retire_client(client, automation, router);
            }
            Request::Agent(_) | Request::Capture { .. } | Request::InspectMapperMonitors { .. } => {
                bail!("Use the dedicated connection, capture or monitor request path")
            }
            Request::Automation(crate::automation_runtime::Command::Mapper(
                crate::mapper_runtime::Command::Inspect,
            )) => {
                extra_events.push(Event::Automation(automation.command(
                    crate::automation_runtime::Command::Mapper(
                        crate::mapper_runtime::Command::Inspect,
                    ),
                    router,
                )?));
            }
            Request::Output(crate::output_runtime::Command::Inspect)
            | Request::Automation(crate::automation_runtime::Command::Inspect)
            | Request::MacroStudio(macro_studio::Command::Inspect { .. })
            | Request::Annotation(crate::annotation_runtime::Command::Inspect)
            | Request::Magnifier(crate::magnifier_runtime::Command::Inspect) => {}
            Request::VirtualUsers(command) => {
                let start = matches!(&command, virtual_users::Command::Start { .. });
                let mutated_roster = !matches!(&command, virtual_users::Command::Inspect);
                let stop_id = match &command {
                    virtual_users::Command::Stop { id, .. } => Some(id.clone()),
                    _ => None,
                };
                let state = match users.command(command, automation, router) {
                    Ok(state) => state,
                    Err(error) => {
                        if let Some(id) = stop_id.as_deref()
                            && users
                                .state()
                                .slots
                                .iter()
                                .any(|slot| slot.seat.identity() == id && !slot.active)
                        {
                            self.remove_virtual_user_receipt(id);
                        }
                        return Err(error);
                    }
                };
                if start
                    && let Some(slot) = state.slots.last()
                    && slot.active
                    && slot.clone_of.is_some()
                {
                    self.created_virtual_users
                        .entry(client)
                        .or_default()
                        .insert(slot.seat.identity());
                }
                if let Some(id) = stop_id {
                    self.remove_virtual_user_receipt(&id);
                }
                extra_events.push(Event::VirtualUsers(state));
                if mutated_roster {
                    let mut automation_state = automation.state(true);
                    automation_state.acknowledged = true;
                    extra_events.push(Event::Automation(automation_state));
                }
            }
            other => {
                if !matches!(
                    &other,
                    Request::Magnifier(
                        crate::magnifier_runtime::Command::Refresh { .. }
                            | crate::magnifier_runtime::Command::Export { .. }
                    )
                ) {
                    ensure!(
                        self.policy.session_control,
                        "Session changes are disabled in Agent Access"
                    );
                }
                match other {
                    Request::SelectWindow(selection) => {
                        ensure!(
                            self.automatic,
                            "Window selection requires automatic desktop routing"
                        );
                        self.user(selection.user, users)?;
                        ensure!(
                            self.policy.allowed_pids.contains(&selection.pid),
                            "Selected PID is outside Agent Access permissions"
                        );
                        crate::window_selection::apply(&selection, automation, router)?;
                        extra_events.push(Event::WindowSelected(
                            crate::window_selection::Acknowledgement {
                                selection,
                                error: None,
                            },
                        ));
                    }
                    Request::Annotation(command) => {
                        self.authorize_annotation(&command, users)?;
                        if let crate::annotation_runtime::Command::Configure { user, .. } = &command
                        {
                            automation.require_idle_user_input(*user)?;
                            ensure!(
                                router.annotation_configuration_idle(*user)?,
                                "Release annotation input before changing annotation settings"
                            );
                            let (_, states) = router.tick()?;
                            let state = states
                                .get(*user)
                                .context("Annotation user has no routing state")?;
                            ensure!(
                                state.buttons.is_empty()
                                    && state.keys.is_empty()
                                    && !state.captured,
                                "Release held input before changing annotation settings"
                            );
                        }
                        let state = router.annotation_command(command)?;
                        extra_events.push(Event::Annotation(state));
                    }
                    Request::Output(crate::output_runtime::Command::Configure {
                        user,
                        settings,
                    }) => {
                        self.user(user, users)?;
                        let state = automation.output_command(
                            crate::output_runtime::Command::Configure { user, settings },
                            router,
                        )?;
                        users.output_settings(user, state.users[user].settings.clone());
                    }
                    Request::Magnifier(command) => {
                        self.authorize_magnifier(&command, users, router)?;
                        let configured_user = match &command {
                            crate::magnifier_runtime::Command::Configure { user, .. } => {
                                Some(*user)
                            }
                            _ => None,
                        };
                        let state = router.magnifier_command(command)?;
                        if let Some(user) = configured_user {
                            let settings = state
                                .users
                                .get(user)
                                .ok_or_else(|| anyhow::anyhow!("Magnifier user state is missing"))?
                                .settings
                                .clone();
                            users.magnifier_settings(user, settings);
                        }
                        extra_events.push(Event::Magnifier(state));
                    }
                    Request::MacroStudio(command) => {
                        ensure!(
                            self.policy.session_control,
                            "Session changes are disabled in Agent Access"
                        );
                        match &command {
                            macro_studio::Command::Record { user, active } => {
                                self.user(*user, users)?;
                                if *active {
                                    self.target(*user, router)?;
                                }
                            }
                            macro_studio::Command::Load { user, .. }
                            | macro_studio::Command::Reset { user }
                            | macro_studio::Command::Play { user, .. }
                            | macro_studio::Command::Cancel { user } => {
                                self.user(*user, users)?;
                                if matches!(&command, macro_studio::Command::Play { .. }) {
                                    self.target(*user, router)?;
                                }
                            }
                            macro_studio::Command::Edit { user, edit } => {
                                self.user(*user, users)?;
                                if let mux_core::macro_authoring::Edit::Remap { from, to } = edit {
                                    ensure!(
                                        from == user,
                                        "macro copy source must match its selected lane"
                                    );
                                    self.user(*from, users)?;
                                    self.user(*to, users)?;
                                }
                            }
                            macro_studio::Command::Generate { user, settings } => {
                                self.user(*user, users)?;
                                ensure!(
                                    settings.user == *user,
                                    "generator user must match its selected lane"
                                );
                            }
                            macro_studio::Command::Inspect { .. } => unreachable!(
                                "Macro Studio inspection is handled before mutation dispatch"
                            ),
                        }
                        let state = automation.agent_studio_command(client, command, router)?;
                        extra_events.push(Event::MacroStudio(state));
                    }
                    Request::Automation(command)
                        if matches!(
                            &command,
                            crate::automation_runtime::Command::Graph { .. }
                                | crate::automation_runtime::Command::CancelReplication { .. }
                                | crate::automation_runtime::Command::Forwarding { .. }
                                | crate::automation_runtime::Command::Mapper(_)
                        ) =>
                    {
                        let is_mapper =
                            matches!(&command, crate::automation_runtime::Command::Mapper(_));
                        let state = automation.command(command, router)?;
                        let mapper_error = if is_mapper { state.error.clone() } else { None };
                        extra_events.push(Event::Automation(state));
                        if let Some(error) = mapper_error {
                            bail!("Input Mapper command failed: {error}");
                        }
                    }
                    Request::Settings {
                        index,
                        settings,
                        gui,
                    } => {
                        ensure!(
                            gui.is_none(),
                            "GUI settings correlation is unavailable to connected clients"
                        );
                        self.user(index, users)?;
                        mux_windows::monitors::validate_settings(&settings)?;
                        // Enabled Agent Access is restricted to desktop cursor routing.
                        crate::router::validate_keyboard_settings(
                            &settings,
                            users.resolved(index)?,
                            true,
                        )?;
                        settings
                            .keyboard
                            .validate_application_route(self.automatic)?;
                        settings.keyboard.validate_live_update(
                            users.require_active(index)?.seat.cursor.keyboard,
                        )?;
                        automation.interrupt_user(index, router)?;
                        if users.require_active(index)?.seat.cursor.keyboard != settings.keyboard {
                            automation.keyboard_settings_change(index, router)?;
                        }
                        automation.release_user_input_claims(index, router)?;
                        router.settings(index, settings.clone())?;
                        users.settings(index, settings);
                    }
                    Request::UserPause { user, paused, gui } => {
                        ensure!(
                            gui.is_none(),
                            "GUI pause correlation is unavailable to connected clients"
                        );
                        self.user(user, users)?;
                        self.target(user, router)?;
                        match automation.user_pause(user, paused, router) {
                            Ok(state) => extra_events.push(Event::UserPause(state)),
                            Err(error) => {
                                let mut state = automation.user_pause_state(user, router);
                                state.error = Some(format!("{error:#}"));
                                extra_events.push(Event::UserPause(state));
                                return Err(error);
                            }
                        }
                    }
                    Request::Pause => {
                        automation.cancel(router)?;
                        automation.stop_recording();
                        router.pause(true)?;
                    }
                    Request::Resume => router.pause(false)?,
                    Request::Mode {
                        mode,
                        multi_keyboard,
                    } => {
                        ensure!(
                            self.automatic,
                            "Mode changes require automatic desktop routing"
                        );
                        ensure!(
                            users
                                .state()
                                .slots
                                .iter()
                                .filter(|s| s.active)
                                .all(|s| self.policy.user_ids.contains(&s.seat.identity())),
                            "Changing mode requires permission for every active user"
                        );
                        crate::window_selection::change_mode(
                            router,
                            mode,
                            multi_keyboard,
                            |router| {
                                automation.cancel(router)?;
                                automation.stop_recording();
                                Ok(())
                            },
                        )?;
                        self.note_mode(mode, multi_keyboard);
                    }
                    _ => bail!(
                        "This session mutation is unavailable through a live GUI connection; use its GUI control"
                    ),
                }
            }
        }
        Ok(())
    }
    fn packet_user(
        &self,
        packet: mux_core::emulated::Packet,
        users: &virtual_users::Runtime,
    ) -> Result<usize> {
        packet.validate()?;
        let user = users
            .state()
            .slots
            .iter()
            .find(|s| {
                s.active
                    && users.resolved(s.user).is_ok_and(|r| {
                        if packet.kind == 0 {
                            r.mouse == packet.device
                        } else {
                            r.keyboard == Some(packet.device)
                        }
                    })
            })
            .map(|s| s.user)
            .context("Unknown virtual device")?;
        self.user(user, users)?;
        Ok(user)
    }
    pub fn standalone_capture(
        &mut self,
        user: usize,
        users: &virtual_users::Runtime,
        router: &dyn Engine,
        base: &Profile,
        query: Option<crate::screen_analysis::Query>,
    ) -> Result<()> {
        let window = router
            .selected_window(user)?
            .context("Select an application window first")?;
        let allowed = base
            .desktop
            .as_ref()
            .map(|d| d.allowed_pids.as_slice())
            .unwrap_or(&[]);
        ensure!(
            !allowed.is_empty() && allowed.contains(&window.pid),
            "Capture requires an explicit allowed process in desktop routing"
        );
        self.begin_capture(
            0,
            0,
            user,
            window,
            users.require_active(user)?.seat.identity(),
            query,
        )
    }
    fn begin_capture(
        &mut self,
        client: u64,
        sequence: u64,
        user: usize,
        window: mux_core::routing::Window,
        user_id: String,
        query: Option<crate::screen_analysis::Query>,
    ) -> Result<()> {
        ensure!(self.capture.is_none(), "A capture is still in progress");
        let (tx, receiver) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("agent-target-capture".into())
            .spawn(move || {
                let result = (|| -> Result<CapturedFrame> {
                    // Preparation is bounded and precedes any native capture.
                    // Analysis remains on this worker so poll_capture rechecks
                    // current capture authority and exact target after all CPU work.
                    let query = query
                        .map(crate::screen_analysis::prepare_query)
                        .transpose()?;
                    crate::screen_analysis::capture_selected(window, user_id, query.as_ref(), None)
                })();
                let _ = tx.send(result);
            })?;
        self.capture = Some(PendingCapture {
            client,
            sequence,
            user,
            window,
            generation: self.generation,
            started: Instant::now(),
            replied: false,
            receiver,
        });
        Ok(())
    }
    pub fn poll_capture(
        &mut self,
        users: &virtual_users::Runtime,
        automation: &macro_studio::Runtime,
        router: &mut crate::magnifier_runtime::Runtime,
        base: &Profile,
    ) -> Result<Option<Event>> {
        let Some(job) = &mut self.capture else {
            return Ok(None);
        };
        let (result, finished) = match job.receiver.try_recv() {
            Ok(result) => (Some(result), true),
            Err(mpsc::TryRecvError::Disconnected) => {
                (Some(Err(anyhow::anyhow!("Capture worker exited"))), true)
            }
            Err(_) if !job.replied && job.started.elapsed() >= Duration::from_secs(5) => (
                Some(Err(anyhow::anyhow!(
                    "Target capture timed out; routing remains active"
                ))),
                false,
            ),
            Err(_) => (None, false),
        };
        let Some(mut result) = result else {
            return Ok(None);
        };
        if job.started.elapsed() >= Duration::from_secs(5) {
            result = Err(anyhow::anyhow!(
                "Target capture timed out; routing remains active"
            ));
        }
        let (client, sequence, user, window, generation, replied) = (
            job.client,
            job.sequence,
            job.user,
            job.window,
            job.generation,
            job.replied,
        );
        job.replied = true;
        if finished {
            self.capture = None;
        }
        if replied {
            return Ok(None);
        }
        if users.require_active(user).is_err()
            || router.selected_window(user).ok().flatten() != Some(window)
            || (client != 0
                && (generation != self.generation
                    || self.require(client).is_err()
                    || !self.policy.capture
                    || !self.policy.allowed_pids.contains(&window.pid)))
        {
            result = Err(anyhow::anyhow!(
                "Capture permission or selected target changed"
            ));
        }
        let (frame, error) = match result {
            Ok(frame) => (Some(frame), None),
            Err(e) => (None, Some(format!("{e:#}"))),
        };
        if client == 0 {
            return Ok(Some(Event::Capture(CaptureState {
                acknowledged: true,
                frame,
                error,
            })));
        }
        let mut state = self.snapshot(client, sequence, users, automation, router, base)?;
        state.frame = frame;
        state.error = error;
        Ok(Some(Event::Agent(state)))
    }
}

#[cfg(test)]
mod virtual_user_scope_tests {
    use super::*;

    fn slots() -> Vec<virtual_users::Slot> {
        let profile: Profile = serde_json::from_value(serde_json::json!({
            "version": 1,
            "concurrent": true,
            "seats": [
                {"id":"original-a","name":"A","mouse":"virtual://mouse/1","keyboard":"virtual://keyboard/1","target_pid":10},
                {"id":"clone-b","name":"B","mouse":"virtual://mouse/2","keyboard":"virtual://keyboard/2","target_pid":20}
            ]
        }))
        .unwrap();
        profile
            .seats
            .into_iter()
            .enumerate()
            .map(|(user, seat)| virtual_users::Slot {
                user,
                clone_of: (user == 1).then(|| "original-a".to_owned()),
                seat,
                active: true,
            })
            .collect()
    }

    #[test]
    fn creator_receipt_exempts_only_its_active_generated_target() {
        let slots = slots();
        let permitted = vec!["original-a".to_owned()];
        assert_eq!(
            authorize_virtual_user_grants(&slots, &permitted, Some("clone-b")).unwrap(),
            permitted
        );
        assert!(authorize_virtual_user_grants(&slots, &permitted, None).is_err());
        assert!(authorize_virtual_user_grants(&slots, &[], Some("original-a")).is_err());
        assert!(authorize_virtual_user_grants(&slots, &[], Some("unknown")).is_err());
    }

    #[test]
    fn receipt_cleanup_removes_retired_ids_and_revoke_clears_all_clients() {
        let mut runtime = Runtime::new();
        runtime
            .created_virtual_users
            .insert(7, BTreeSet::from(["clone-a".to_owned()]));
        runtime
            .created_virtual_users
            .insert(8, BTreeSet::from(["clone-b".to_owned()]));
        assert!(runtime.created_virtual_user(7, "clone-a"));
        assert!(!runtime.created_virtual_user(8, "clone-a"));
        runtime.remove_virtual_user_receipt("clone-a");
        assert!(!runtime.created_virtual_users.contains_key(&7));
        assert!(runtime.created_virtual_users.contains_key(&8));
        runtime.clear_virtual_user_receipts();
        assert!(runtime.created_virtual_users.is_empty());
    }
}
