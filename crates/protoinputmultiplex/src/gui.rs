#[path = "gui_application_event_dashboard.rs"]
mod application_event_dashboard;

use crate::{
    keyboard_slider,
    session::{Controller, Event as SessionEvent, Request},
};
use anyhow::{Context, Result, bail, ensure};
use eframe::egui::{self, Color32, RichText};
use mux_core::{Device, DeviceKind, Pointer, Profile, Seat, emulated};
use mux_windows::raw_input::{Capture, Event};
use mux_windows::targets::{self, Target};
use ouroboros_ui::{
    Mode, Theme,
    atoms::{Button, ButtonVariant, Checkbox, Divider, Heading, Input, Surface, Text},
    cells::MenuItem,
    molecules::Collapsible,
    organisms::{Dialog, Select},
    theme::typography,
    tokens::core,
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, mpsc::Receiver},
    time::{Duration, Instant},
};

/// Installation and data leases shared with detached GUI owners until their work ends.
pub(crate) struct RuntimeGuards {
    _installation: Option<File>,
    _data: Option<crate::config_maintenance::RuntimeLease>,
}

#[cfg(test)]
impl RuntimeGuards {
    pub(crate) fn for_test(root: &std::path::Path) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            _installation: None,
            _data: crate::config_maintenance::runtime_lease(root)?,
        }))
    }
}

const COLORS: [Color32; 6] = [
    core::BLUE_400,
    core::RED_500,
    core::GREEN_500,
    core::TEAL_300,
    core::AMBER_500,
    core::ZINC_300,
];

const SESSION_EVENTS_PER_FRAME: usize = 256;

fn cursor_draft_bounds_error(
    settings: &mux_core::desktop::CursorSettings,
    desktop: Option<mux_core::desktop::Bounds>,
    window: Option<mux_core::desktop::Bounds>,
    monitor: Option<mux_core::desktop::Bounds>,
) -> Option<String> {
    if let Some(desktop) = desktop {
        return settings
            .resolve_bounds(desktop, window, monitor)
            .err()
            .map(|error| format!("{error:#}"));
    }
    (settings.bounds_mode == mux_core::desktop::BoundsMode::Region).then(|| {
        "Desktop bounds are unavailable. Wait for display discovery before applying a region."
            .into()
    })
}

#[cfg(test)]
#[test]
fn cursor_draft_uses_discovered_bounds_only_when_needed() {
    use mux_core::desktop::{Bounds, BoundsMode, CursorSettings};
    let desktop = Bounds {
        left: -200,
        top: -100,
        right: 300,
        bottom: 400,
    };
    assert_eq!(
        cursor_draft_bounds_error(&CursorSettings::default(), None, None, None),
        None
    );
    let mut region = CursorSettings {
        bounds_mode: BoundsMode::Region,
        region: Some(Bounds {
            left: -210,
            top: -50,
            right: -110,
            bottom: 50,
        }),
        ..Default::default()
    };
    assert!(
        cursor_draft_bounds_error(&region, None, None, None)
            .unwrap()
            .contains("display discovery")
    );
    assert_eq!(
        cursor_draft_bounds_error(&region, Some(desktop), None, None),
        None
    );
    region.region = Some(Bounds {
        left: 400,
        top: 50,
        right: 500,
        bottom: 100,
    });
    assert!(
        cursor_draft_bounds_error(&region, Some(desktop), None, None)
            .unwrap()
            .contains("does not overlap")
    );
}

// Only a routing failure with the same reachable retained diagnostic may be
// concise. Broker failures use their separate lifecycle feedback paths.
fn routing_failure_footer(error: String, recovery: &crate::session_recovery::Snapshot) -> String {
    if recovery.phase == crate::session_recovery::Phase::Quarantined
        && recovery.error.as_deref() == Some(error.as_str())
    {
        "Routing cleanup is incomplete. Open Recovery details.".into()
    } else {
        error
    }
}

// Run only after a new Controller has been constructed successfully. Retained
// terminal inspection keeps its page until this fresh-session boundary.
fn initialize_controller_agent_access(
    page: &mut crate::agent_ui::AgentPage,
    app: &mut crate::gui_bridge::AppHandle,
    controller: u64,
) {
    page.reset_session();
    app.begin_controller(controller);
}

fn stop_and_latch_before_broker_cleanup(
    request_stop: impl FnOnce(),
    latch_stopping: impl FnOnce(),
    cleanup_broker: impl FnOnce(),
) {
    request_stop();
    latch_stopping();
    cleanup_broker();
}

fn latch_routing_stopping(
    session_ready: &mut bool,
    session_busy: &mut bool,
    session_stopping: &mut bool,
    quit_after_stop: &mut bool,
    quit_after: bool,
    status: &mut String,
) {
    *session_ready = false;
    *session_busy = true;
    *session_stopping = true;
    *quit_after_stop |= quit_after;
    *status = "Stopping routing and releasing virtual input.".into();
}

/// Script-only handler entries; an internal Restore after Quit is another entry.
#[derive(Default, serde::Serialize)]
struct LifecycleTrace {
    entries: u64,
    stop_received: u64,
    quit_received: u64,
    last_action: Option<String>,
    last_action_frame: Option<u64>,
}
impl LifecycleTrace {
    fn record(&mut self, action: crate::app_lifecycle::LifecycleAction, frame: u64) {
        use crate::app_lifecycle::LifecycleAction as Action;
        self.entries = self.entries.saturating_add(1);
        match action {
            Action::Stop => self.stop_received = self.stop_received.saturating_add(1),
            Action::Quit => self.quit_received = self.quit_received.saturating_add(1),
            _ => {}
        }
        self.last_action = Some(format!("{action:?}"));
        self.last_action_frame = Some(frame);
    }
}

fn drain_session_events(events: &Receiver<SessionEvent>) -> Vec<SessionEvent> {
    events.try_iter().take(SESSION_EVENTS_PER_FRAME).collect()
}

fn merge_runtime_roster(
    profile: &mut Profile,
    state: &crate::virtual_users::State,
    base_count: usize,
) -> Result<()> {
    ensure!(
        state.slots.len() >= profile.seats.len() && profile.seats.len() >= base_count,
        "Runtime user slots must remain append-only"
    );
    ensure!(
        state
            .slots
            .iter()
            .enumerate()
            .all(|(index, slot)| slot.user == index),
        "Runtime user slot order changed"
    );
    ensure!(
        profile
            .seats
            .iter()
            .zip(&state.slots)
            .all(|(seat, slot)| seat.identity() == slot.seat.identity()),
        "Runtime user identities changed"
    );
    profile.seats.extend(
        state
            .slots
            .iter()
            .skip(profile.seats.len())
            .map(|slot| slot.seat.clone()),
    );
    let active: BTreeSet<_> = state
        .slots
        .iter()
        .filter(|slot| slot.active)
        .map(|slot| slot.seat.identity())
        .collect();
    profile
        .replication
        .edges
        .retain(|edge| active.contains(&edge.source) && active.contains(&edge.target));
    Ok(())
}

#[cfg(test)]
mod macro_file_dispatch_tests {
    use super::*;

    #[test]
    fn quit_receipt_survives_following_internal_restore_entry() {
        use crate::app_lifecycle::LifecycleAction as Action;
        let mut trace = LifecycleTrace::default();
        trace.record(Action::Stop, 10);
        trace.record(Action::Quit, 11);
        trace.record(Action::Restore, 11);
        let value = serde_json::to_value(&trace).unwrap();
        assert_eq!(value["entries"], 3);
        assert_eq!(value["stop_received"], 1);
        assert_eq!(value["quit_received"], 1);
        assert_eq!(value["last_action"], "Restore");
        assert_eq!(value["last_action_frame"], 11);
    }

    #[test]
    fn stop_and_gui_latch_precede_blocking_broker_cleanup() {
        use std::sync::{
            Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc,
        };

        struct Latches {
            ready: bool,
            busy: bool,
            stopping: bool,
            quit_after_stop: bool,
            status: String,
        }

        let stop_count = Arc::new(AtomicUsize::new(0));
        let latches = Arc::new(Mutex::new(Latches {
            ready: true,
            busy: false,
            stopping: false,
            quit_after_stop: false,
            status: String::new(),
        }));
        let descriptor_present = Arc::new(AtomicBool::new(true));
        let cleanup_started = Arc::new(AtomicBool::new(false));
        let (cleanup_entered_tx, cleanup_entered_rx) = mpsc::channel();
        let (allow_cleanup_tx, allow_cleanup_rx) = mpsc::channel();

        let stop_count_on_thread = Arc::clone(&stop_count);
        let latches_on_thread = Arc::clone(&latches);
        let descriptor_present_on_thread = Arc::clone(&descriptor_present);
        let cleanup_started_on_thread = Arc::clone(&cleanup_started);
        let cleanup = std::thread::spawn(move || {
            stop_and_latch_before_broker_cleanup(
                || {
                    stop_count_on_thread.fetch_add(1, Ordering::AcqRel);
                },
                || {
                    let mut latches = latches_on_thread.lock().unwrap();
                    let latches = &mut *latches;
                    latch_routing_stopping(
                        &mut latches.ready,
                        &mut latches.busy,
                        &mut latches.stopping,
                        &mut latches.quit_after_stop,
                        true,
                        &mut latches.status,
                    );
                },
                || {
                    cleanup_started_on_thread.store(true, Ordering::Release);
                    cleanup_entered_tx.send(()).unwrap();
                    allow_cleanup_rx
                        .recv_timeout(Duration::from_secs(2))
                        .expect("test did not release deliberately blocked broker cleanup");
                    descriptor_present_on_thread.store(false, Ordering::Release);
                },
            );
        });

        cleanup_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("broker cleanup did not start within the test bound");
        assert_eq!(stop_count.load(Ordering::Acquire), 1);
        {
            let latches = latches.lock().unwrap();
            assert!(!latches.ready);
            assert!(latches.busy);
            assert!(latches.stopping);
            assert!(latches.quit_after_stop);
            assert_eq!(
                latches.status,
                "Stopping routing and releasing virtual input."
            );
        }
        assert!(cleanup_started.load(Ordering::Acquire));
        assert!(descriptor_present.load(Ordering::Acquire));
        allow_cleanup_tx.send(()).unwrap();
        cleanup.join().unwrap();
        assert!(!descriptor_present.load(Ordering::Acquire));
    }

    #[test]
    fn delayed_load_is_bound_to_original_ready_session_and_mutation() {
        let clicked = crate::gui_macro_file::ContextId {
            session: 7,
            mutation: 12,
            macro_revision: 4,
        };
        let live = MacroLoadConditions {
            has_session: true,
            ready: true,
            stopping: false,
            busy: false,
            recording: false,
            playing: false,
            studio_active: false,
        };
        assert!(macro_load_can_dispatch(clicked, clicked, live));
        assert!(!macro_load_can_dispatch(
            clicked,
            crate::gui_macro_file::ContextId {
                session: 8,
                mutation: 12,
                macro_revision: 4,
            },
            live
        ));
        assert!(!macro_load_can_dispatch(
            clicked,
            crate::gui_macro_file::ContextId {
                session: 7,
                mutation: 13,
                macro_revision: 4,
            },
            live
        ));
        assert!(!macro_load_can_dispatch(
            clicked,
            crate::gui_macro_file::ContextId {
                session: 7,
                mutation: 12,
                macro_revision: 5,
            },
            live
        ));
        for blocked in [
            MacroLoadConditions {
                stopping: true,
                ..live
            },
            MacroLoadConditions {
                ready: false,
                ..live
            },
            MacroLoadConditions { busy: true, ..live },
            MacroLoadConditions {
                recording: true,
                ..live
            },
            MacroLoadConditions {
                playing: true,
                ..live
            },
            MacroLoadConditions {
                studio_active: true,
                ..live
            },
            MacroLoadConditions {
                has_session: false,
                ..live
            },
        ] {
            assert!(!macro_load_can_dispatch(clicked, clicked, blocked));
        }
    }

    #[test]
    fn nested_agent_mutation_uses_same_filter_without_cancelling_periodic_state() {
        let mut automation = crate::automation_runtime::State::default();
        assert!(!macro_file_conflicts_with_automation(
            &automation,
            0,
            false,
            0,
            None
        ));
        automation.acknowledged = true;
        automation.clip = Some(
            mux_core::automation::Clip::new(vec![mux_core::automation::TimedAction {
                at_micros: 0,
                action: mux_core::automation::Action::MoveTo {
                    user: 0,
                    x: 2,
                    y: 3,
                },
            }])
            .unwrap(),
        );
        automation.error = Some("rejected".into());
        assert!(!macro_file_conflicts_with_automation(
            &automation,
            0,
            false,
            0,
            None
        ));
        automation.error = None;
        let nested = SessionEvent::Automation(automation);
        let SessionEvent::Automation(state) = &nested else {
            unreachable!()
        };
        assert!(macro_file_conflicts_with_automation(
            state, 0, false, 0, None
        ));
        let periodic_after_mutation = crate::automation_runtime::State {
            macro_revision: 1,
            ..Default::default()
        };
        assert!(macro_file_conflicts_with_automation(
            &periodic_after_mutation,
            0,
            false,
            0,
            None,
        ));
        let periodic_studio = crate::macro_studio::State {
            acknowledged: false,
            users: Vec::new(),
            error: None,
        };
        assert!(!macro_file_conflicts_with_studio(&periodic_studio));
        let acknowledged_studio = crate::macro_studio::State {
            acknowledged: true,
            ..periodic_studio
        };
        let nested = SessionEvent::MacroStudio(acknowledged_studio);
        let SessionEvent::MacroStudio(state) = &nested else {
            unreachable!()
        };
        assert!(macro_file_conflicts_with_studio(state));
    }
}

const USER_IDENTITY_WIDTH: f32 = 40.0;
const SYSTEM_FALLBACK_FONT_NAME: &str = "windows-malgun-cjk";
const MAX_SYSTEM_FALLBACK_FONT_BYTES: u64 = 32 * 1024 * 1024;

#[cfg(test)]
fn append_system_font_fallback(fonts: &mut egui::FontDefinitions, path: &Path) -> bool {
    let Some(bytes) = load_system_font_fallback(path) else {
        return false;
    };
    append_system_font_fallback_bytes(fonts, bytes);
    true
}

fn load_system_font_fallback(path: &Path) -> Option<Vec<u8>> {
    let Ok(metadata) = std::fs::metadata(path) else {
        return None;
    };
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_SYSTEM_FALLBACK_FONT_BYTES
    {
        return None;
    }

    let Ok(file) = File::open(path) else {
        return None;
    };
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    if file
        .take(MAX_SYSTEM_FALLBACK_FONT_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.is_empty()
        || bytes.len() as u64 > MAX_SYSTEM_FALLBACK_FONT_BYTES
    {
        return None;
    }
    Some(bytes)
}

fn append_system_font_fallback_bytes(fonts: &mut egui::FontDefinitions, bytes: Vec<u8>) {
    let mut data = egui::FontData::from_owned(bytes);
    data.index = 0;
    fonts
        .font_data
        .insert(SYSTEM_FALLBACK_FONT_NAME.to_owned(), Arc::new(data));
    for family in fonts.families.values_mut() {
        if !family.iter().any(|name| name == SYSTEM_FALLBACK_FONT_NAME) {
            family.push(SYSTEM_FALLBACK_FONT_NAME.to_owned());
        }
    }
}

fn load_windows_cjk_fallback() -> Option<Vec<u8>> {
    let path = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .map(|directory| directory.join("Fonts").join("malgun.ttf"))?;
    load_system_font_fallback(&path)
}

fn roster_number(
    ui: &mut egui::Ui,
    index: usize,
    identity: &str,
    selected: bool,
) -> egui::Response {
    Button::new(format!("{:02}", index + 1))
        .sm()
        .id_source(("seat", identity))
        .variant(if selected {
            ButtonVariant::Secondary
        } else {
            ButtonVariant::Ghost
        })
        .show(ui)
}

fn home_window_size(user_count: usize) -> egui::Vec2 {
    // A two-user Home starts at 360 px tall. Add room for up to six extra rows;
    // larger rosters scroll inside the reserved roster area instead of moving actions.
    let extra_rows = user_count.saturating_sub(2).min(6) as f32;
    egui::vec2(360.0, (360.0 + extra_rows * 30.0).min(540.0))
}
fn apply_muted_text(ctx: &egui::Context) {
    let muted = Theme::get_from_ctx(ctx).muted_foreground;
    ctx.global_style_mut(|style| style.visuals.weak_text_color = Some(muted));
}

fn theme_mode(mode: crate::app_preferences::AppearanceMode) -> Mode {
    match mode {
        crate::app_preferences::AppearanceMode::Dark => Mode::Dark,
        crate::app_preferences::AppearanceMode::Light => Mode::Light,
    }
}

#[derive(Clone, Copy)]
enum RosterGlyph {
    Role,
    Pointer,
    Keyboard,
}

fn roster_glyph(ui: &mut egui::Ui, kind: RosterGlyph, active: bool, tooltip: &str) {
    // Original vector marks avoid depending on icon-font or platform glyph coverage.
    let color = if active {
        Theme::get(ui).primary
    } else {
        ui.visuals().weak_text_color()
    };
    let stroke = egui::Stroke::new(1.25_f32, color);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(18.0, 20.0), egui::Sense::hover());
    let center = rect.center();
    match kind {
        RosterGlyph::Role => {
            ui.painter()
                .circle_stroke(center + egui::vec2(0.0, 1.0), 6.0, stroke);
            ui.painter()
                .circle_filled(center + egui::vec2(0.0, -2.0), 1.5, color);
            ui.painter().line_segment(
                [
                    center + egui::vec2(-3.0, 5.0),
                    center + egui::vec2(3.0, 5.0),
                ],
                stroke,
            );
        }
        RosterGlyph::Pointer => {
            let mouse = egui::Rect::from_center_size(center, egui::vec2(10.0, 15.0));
            ui.painter()
                .rect_stroke(mouse, 4.0, stroke, egui::StrokeKind::Inside);
            ui.painter().line_segment(
                [
                    egui::pos2(center.x, mouse.top()),
                    egui::pos2(center.x, center.y + 1.0),
                ],
                stroke,
            );
        }
        RosterGlyph::Keyboard => {
            let keyboard = egui::Rect::from_center_size(center, egui::vec2(16.0, 11.0));
            ui.painter()
                .rect_stroke(keyboard, 2.0, stroke, egui::StrokeKind::Inside);
            for row in 0..2 {
                for column in 0..4 {
                    let point = keyboard.min
                        + egui::vec2(3.0 + column as f32 * 3.2, 3.0 + row as f32 * 4.0);
                    ui.painter().circle_filled(point, 0.75, color);
                }
            }
        }
    }
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Label, true, tooltip.to_owned())
    });
    response.on_hover_text(tooltip);
}

fn lock_summary_caption(ui: &mut egui::Ui, text: &str) -> egui::Response {
    // Match Ouroboros Caption typography, but truncate within the actual layout.
    // Measuring egui Small misses Caption's font and letter spacing.
    let style = typography::caption();
    let mut content: String = text.chars().take(256).collect();
    if content.len() < text.len() {
        content.push('…');
    }
    let width = ui
        .available_width()
        .min((ui.clip_rect().right() - ui.next_widget_position().x).max(0.));
    ui.allocate_ui_with_layout(
        egui::vec2(width, 0.),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(content)
                        .font(style.font_id())
                        .extra_letter_spacing(style.tracking)
                        .color(Theme::get(ui).foreground),
                )
                .selectable(false)
                .truncate(),
            )
        },
    )
    .inner
}

fn bounded_display_text(ui: &egui::Ui, text: &str, font: egui::FontId, max_width: f32) -> String {
    const MAX_NAME_SCALARS_TO_MEASURE: usize = 256;

    // Keep profile data intact while bounding the text shaped on every UI frame.
    let mut starts: Vec<usize> = text
        .char_indices()
        .take(MAX_NAME_SCALARS_TO_MEASURE + 1)
        .map(|(index, _)| index)
        .collect();
    let source_truncated = starts.len() > MAX_NAME_SCALARS_TO_MEASURE;
    let prefix_end = if source_truncated {
        starts[MAX_NAME_SCALARS_TO_MEASURE]
    } else {
        text.len()
    };
    starts.truncate(MAX_NAME_SCALARS_TO_MEASURE);
    starts.push(prefix_end);

    let fits = |candidate: &str| {
        ui.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(candidate.to_owned(), font.clone(), Color32::WHITE)
                .size()
                .x
                <= max_width.max(0.)
        })
    };
    if !source_truncated && fits(text) {
        return text.to_owned();
    }

    // Search measured scalar boundaries. Each candidate is at most 256 scalars.
    let (mut low, mut high) = (0, starts.len() - 1);
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        let candidate = format!("{}…", &text[..starts[middle]]);
        if fits(&candidate) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    let candidate = format!("{}…", &text[..starts[low]]);
    if fits(&candidate) {
        candidate
    } else {
        String::new()
    }
}
fn edge_lists_match(
    left: &[crate::automation_runtime::Edge],
    right: &[crate::automation_runtime::Edge],
) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(a, b)| {
            a.source == b.source
                && a.target == b.target
                && a.mouse == b.mouse
                && a.keyboard == b.keyboard
                && a.text == b.text
        })
}

#[derive(Clone, Copy)]
pub struct WorkerDelays {
    pub discovery: Duration,
    pub start_preflight: Duration,
    pub profile_file: Duration,
    pub app_preferences: Duration,
    pub bootstrap: Duration,
}

#[derive(Clone)]
struct BootstrapInputs {
    data_dir: Option<PathBuf>,
    preferences_path: Option<PathBuf>,
    startup_test_key: Option<String>,
    script_path: Option<PathBuf>,
    script_screenshots: bool,
    preferences_delay: Duration,
    bootstrap_delay: Duration,
}

struct BootstrapReady {
    guards: Arc<RuntimeGuards>,
    preferences_path: PathBuf,
    preferences: crate::app_preferences::Preferences,
    runtime: String,
    agent_executable_path: Option<String>,
    font_bytes: Option<Vec<u8>>,
    lifecycle_handle: Option<crate::app_lifecycle::Handle>,
    lifecycle_actions: Option<Receiver<crate::app_lifecycle::LifecycleAction>>,
    preferences_worker: Option<crate::gui_app_preferences::Service>,
}

enum BootstrapMessage {
    FrontendReady {
        script: Box<crate::ui_script::Script>,
        guards: Arc<RuntimeGuards>,
    },
    Finished(std::result::Result<Box<BootstrapReady>, BootstrapFailure>),
}

struct BootstrapFailure {
    message: String,
    guards: Option<Arc<RuntimeGuards>>,
}

struct PendingBootstrap {
    receiver: Receiver<BootstrapMessage>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}
impl Drop for PendingBootstrap {
    fn drop(&mut self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

fn bootstrap_cancelled(cancelled: &std::sync::atomic::AtomicBool) -> Result<()> {
    ensure!(
        !cancelled.load(std::sync::atomic::Ordering::Acquire),
        "GUI startup was cancelled"
    );
    Ok(())
}

fn prepare_bootstrap(
    inputs: BootstrapInputs,
    retained_guards: Option<Arc<RuntimeGuards>>,
    already_has_script: bool,
    guard_slot: &mut Option<Arc<RuntimeGuards>>,
    cancelled: &std::sync::atomic::AtomicBool,
    sender: &std::sync::mpsc::Sender<BootstrapMessage>,
) -> Result<BootstrapReady> {
    let preferences_path =
        crate::app_paths::preferences_path(inputs.data_dir, inputs.preferences_path)?;
    let guards = if let Some(guards) = retained_guards {
        guards
    } else {
        let installation = crate::installation::runtime_lease()?;
        bootstrap_cancelled(cancelled)?;
        let root = preferences_path
            .parent()
            .context("preferences path has no parent")?;
        let data = crate::config_maintenance::runtime_lease(root)
            .with_context(|| format!("lease data directory {}", root.display()))?;
        Arc::new(RuntimeGuards {
            _installation: installation,
            _data: data,
        })
    };
    *guard_slot = Some(Arc::clone(&guards));
    bootstrap_cancelled(cancelled)?;
    if !already_has_script && let Some(path) = inputs.script_path {
        let script = crate::ui_script::Script::load_guarded(
            path.clone(),
            Arc::clone(&guards),
            inputs.script_screenshots,
        )
        .with_context(|| format!("load GUI script {}", path.display()))?;
        sender
            .send(BootstrapMessage::FrontendReady {
                script: Box::new(script),
                guards: Arc::clone(&guards),
            })
            .map_err(|_| anyhow::anyhow!("GUI closed during script startup"))?;
    }
    if !inputs.bootstrap_delay.is_zero() {
        let deadline = Instant::now() + inputs.bootstrap_delay;
        while Instant::now() < deadline {
            bootstrap_cancelled(cancelled)?;
            std::thread::sleep(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(25)),
            );
        }
    }
    bootstrap_cancelled(cancelled)?;
    let preferences =
        crate::app_preferences::Store::load(&preferences_path).with_context(|| {
            format!(
                "load application preferences {}",
                preferences_path.display()
            )
        })?;
    let font_bytes = load_windows_cjk_fallback();
    let runtime = crate::app_paths::runtime_dir().display().to_string();
    let agent_executable_path = std::env::current_exe()
        .ok()
        .and_then(|path| crate::agent_ui::exact_path_string(&path));
    bootstrap_cancelled(cancelled)?;
    let startup_registration: Box<dyn crate::app_preferences::StartupRegistration> =
        if let Some(key) = inputs.startup_test_key.as_deref() {
            Box::new(crate::app_preferences::CurrentUserRunKey::injected_test_key(key)?)
        } else {
            Box::new(crate::app_preferences::CurrentUserRunKey::current_user())
        };
    let mut lifecycle = crate::app_lifecycle::Service::start(&preferences)?;
    let lifecycle_actions = lifecycle.take_actions();
    let lifecycle_handle = lifecycle.handle();
    let preferences_worker = crate::gui_app_preferences::Service::start(
        preferences_path.clone(),
        startup_registration,
        lifecycle,
        preferences.clone(),
        inputs.preferences_delay,
        Arc::clone(&guards),
    )?;
    Ok(BootstrapReady {
        guards,
        preferences_path,
        preferences,
        runtime,
        agent_executable_path,
        font_bytes,
        lifecycle_handle: Some(lifecycle_handle),
        lifecycle_actions: Some(lifecycle_actions),
        preferences_worker: Some(preferences_worker),
    })
}

fn spawn_bootstrap(
    inputs: BootstrapInputs,
    retained_guards: Option<Arc<RuntimeGuards>>,
    already_has_script: bool,
) -> Result<PendingBootstrap> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    let worker = std::thread::Builder::new()
        .name("gui-bootstrap".into())
        .spawn(move || {
            let mut guard_slot = retained_guards.clone();
            let result = prepare_bootstrap(
                inputs,
                retained_guards,
                already_has_script,
                &mut guard_slot,
                &worker_cancelled,
                &sender,
            )
            .map(Box::new)
            .map_err(|error| BootstrapFailure {
                message: format!("{error:#}"),
                guards: guard_slot,
            });
            let _ = sender.send(BootstrapMessage::Finished(result));
        })?;
    drop(worker);
    Ok(PendingBootstrap {
        receiver,
        cancelled,
    })
}

fn install_gui_fonts(ctx: &egui::Context, fallback: Option<Vec<u8>>) {
    let mut fonts = egui::FontDefinitions::default();
    let mut ui_stack = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    typography::register(&mut fonts);
    ui_stack.push("phosphor".to_owned());
    fonts
        .families
        .insert(egui::FontFamily::Proportional, ui_stack.clone());
    for family in [
        "iosevka",
        "iosevka-light",
        "iosevka-medium",
        "iosevka-semibold",
        "iosevka-bold",
    ] {
        fonts
            .families
            .insert(egui::FontFamily::Name(family.into()), ui_stack.clone());
    }
    if let Some(bytes) = fallback {
        append_system_font_fallback_bytes(&mut fonts, bytes);
    }
    ctx.set_fonts(fonts);
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(core::SPACE_3, core::SPACE_3);
    });
}

struct BootstrapApp {
    activity: crate::gui_activity::Probe,
    inputs: BootstrapInputs,
    profile: Option<PathBuf>,
    delays: WorkerDelays,
    pending: Option<PendingBootstrap>,
    ready: Option<BootstrapReady>,
    script: Option<crate::ui_script::Script>,
    frontend_guards: Option<Arc<RuntimeGuards>>,
    dashboard: Option<Dashboard>,
    failure: Option<String>,
    assembly_retry: bool,
    closing: bool,
}

impl BootstrapApp {
    fn new(inputs: BootstrapInputs, profile: Option<PathBuf>, delays: WorkerDelays) -> Self {
        let (pending, failure) = match spawn_bootstrap(inputs.clone(), None, false) {
            Ok(pending) => (Some(pending), None),
            Err(error) => (None, Some(format!("Start GUI initialization: {error:#}"))),
        };
        Self {
            activity: crate::gui_activity::Probe::new(),
            inputs,
            profile,
            delays,
            pending,
            ready: None,
            script: None,
            frontend_guards: None,
            dashboard: None,
            failure,
            assembly_retry: false,
            closing: false,
        }
    }

    fn close(&mut self, ctx: &egui::Context) {
        self.closing = true;
        self.pending = None;
        self.ready = None;
        self.script = None;
        self.frontend_guards = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn retry(&mut self) {
        if self.closing {
            return;
        }
        self.failure = None;
        if self.ready.is_some() {
            self.assembly_retry = true;
        } else {
            match spawn_bootstrap(
                self.inputs.clone(),
                self.frontend_guards.clone(),
                self.script.is_some(),
            ) {
                Ok(pending) => self.pending = Some(pending),
                Err(error) => self.failure = Some(format!("Start GUI initialization: {error:#}")),
            }
        }
    }

    fn poll_bootstrap(&mut self) {
        loop {
            let Some(pending) = &self.pending else {
                return;
            };
            match pending.receiver.try_recv() {
                Ok(BootstrapMessage::FrontendReady { mut script, guards }) => {
                    if self.script.is_none() {
                        script.bind_capture_activity(&self.activity);
                        self.script = Some(*script);
                        self.frontend_guards = Some(guards);
                    }
                }
                Ok(BootstrapMessage::Finished(result)) => {
                    self.pending = None;
                    match result {
                        Ok(ready) => self.ready = Some(*ready),
                        Err(error) => {
                            self.frontend_guards = error.guards;
                            self.failure = Some(error.message);
                        }
                    }
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.failure =
                        Some("GUI initialization worker stopped without a result.".into());
                    return;
                }
            }
        }
    }
}

impl eframe::App for BootstrapApp {
    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        let _activity = self.activity.enter(crate::gui_activity::Callback::RawInput);
        if let Some(dashboard) = &mut self.dashboard {
            eframe::App::raw_input_hook(dashboard, ctx, input);
        } else if let Some(script) = &mut self.script {
            script.capture(ctx, input);
            script.input(input);
        }
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let _activity = self.activity.enter(crate::gui_activity::Callback::Logic);
        if let Some(dashboard) = &mut self.dashboard {
            eframe::App::logic(dashboard, ctx, frame);
            return;
        }
        if self.closing {
            return;
        }
        if ctx.input(|input| input.viewport().close_requested()) {
            self.close(ctx);
            return;
        }
        self.poll_bootstrap();
        if let Some(ready) = &self.ready
            && let Some(actions) = &ready.lifecycle_actions
        {
            let actions: Vec<_> = actions.try_iter().take(32).collect();
            for action in actions {
                match action {
                    crate::app_lifecycle::LifecycleAction::Quit => {
                        self.close(ctx);
                        return;
                    }
                    crate::app_lifecycle::LifecycleAction::Restore => {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    }
                    _ => {}
                }
            }
        }
        if self.ready.is_some() && (self.failure.is_none() || self.assembly_retry) {
            self.assembly_retry = false;
            let ready = self.ready.as_mut().expect("ready checked");
            match Dashboard::new(ready, self.delays, self.activity.observer()) {
                Ok(mut dashboard) => {
                    dashboard.preferences_worker.attach_repaint_context(ctx);
                    dashboard.profile_file.attach_repaint_context(ctx);
                    Theme::install(ctx, theme_mode(dashboard.app_preferences.appearance_mode));
                    apply_muted_text(ctx);
                    ctx.set_zoom_factor(dashboard.app_preferences.zoom_percent as f32 / 100.0);
                    install_gui_fonts(ctx, ready.font_bytes.take());
                    if dashboard.app_preferences.launch_minimized
                        && dashboard.lifecycle.shortcut_status().active
                    {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                    }
                    let startup_path = self
                        .profile
                        .clone()
                        .unwrap_or_else(|| PathBuf::from(&dashboard.profile_path));
                    dashboard.profile_path = startup_path.display().to_string();
                    if dashboard.app_preferences.load_profile_at_startup {
                        dashboard.profile_file_ui.initial_unresolved = true;
                        dashboard.initial_profile_size = Some(egui::vec2(360.0, 360.0));
                        dashboard.submit_profile_file_at(
                            if self.profile.is_some() {
                                crate::gui_profile_file::Kind::InitialLoad
                            } else {
                                crate::gui_profile_file::Kind::InitialLoadDefault
                            },
                            startup_path,
                        );
                    } else {
                        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(home_window_size(
                            dashboard.profile.seats.len(),
                        )));
                    }
                    dashboard.script = self.script.take();
                    dashboard.lifecycle_trace =
                        dashboard.script.as_ref().map(|_| LifecycleTrace::default());
                    self.ready = None;
                    self.frontend_guards = None;
                    self.dashboard = Some(dashboard);
                    if let Some(dashboard) = &mut self.dashboard {
                        eframe::App::logic(dashboard, ctx, frame);
                    }
                    return;
                }
                Err(error) => self.failure = Some(format!("Initialize GUI controls: {error:#}")),
            }
        }
        if let Some(script) = &mut self.script {
            script.ready(false);
            script.bootstrap_state(
                if self.failure.is_some() {
                    "failed"
                } else {
                    "initializing"
                },
                self.failure.as_deref(),
            );
        }
        ctx.request_repaint_after(Duration::from_millis(16));
    }

    fn ui(&mut self, root: &mut egui::Ui, frame: &mut eframe::Frame) {
        let _activity = self.activity.enter(crate::gui_activity::Callback::Ui);
        if let Some(dashboard) = &mut self.dashboard {
            eframe::App::ui(dashboard, root, frame);
            return;
        }
        if self.closing {
            return;
        }
        egui::CentralPanel::default().show_inside(root, |ui| {
            ui.add_space(core::SPACE_6);
            Heading::new(crate::APP_NAME).heading().show(ui);
            ui.add_space(core::SPACE_3);
            if let Some(error) = &self.failure {
                Text::new("Initialization needs attention").show(ui);
                ui.add_space(core::SPACE_2);
                egui::ScrollArea::vertical()
                    .max_height(120.0)
                    .show(ui, |ui| {
                        Text::new(error).wrap().show(ui);
                    });
            } else {
                Text::new("Initializing ProtoInputMultiPlex…").show(ui);
            }
            ui.add_space(core::SPACE_5);
            let response = Button::new("Retry")
                .variant(ButtonVariant::Default)
                .enabled(self.failure.is_some() && self.pending.is_none() && !self.closing)
                .show(ui);
            let accepted = self
                .script
                .as_mut()
                .is_none_or(|script| script.observe("Retry", &response));
            if response.clicked() && accepted {
                self.retry();
            }
            let response = Button::new("Close")
                .variant(ButtonVariant::Secondary)
                .show(ui);
            let accepted = self
                .script
                .as_mut()
                .is_none_or(|script| script.observe("Close", &response));
            if response.clicked() && accepted {
                self.close(ui.ctx());
            }
        });
    }
}

#[cfg(test)]
mod activity_callback_tests {
    use super::*;
    fn fixture_app() -> BootstrapApp {
        BootstrapApp {
            activity: crate::gui_activity::Probe::new(),
            inputs: BootstrapInputs {
                data_dir: None,
                preferences_path: None,
                startup_test_key: None,
                script_path: None,
                script_screenshots: true,
                preferences_delay: Duration::ZERO,
                bootstrap_delay: Duration::ZERO,
            },
            profile: None,
            delays: WorkerDelays {
                discovery: Duration::ZERO,
                start_preflight: Duration::ZERO,
                profile_file: Duration::ZERO,
                app_preferences: Duration::ZERO,
                bootstrap: Duration::ZERO,
            },
            pending: None,
            ready: None,
            script: None,
            frontend_guards: None,
            dashboard: None,
            failure: None,
            assembly_retry: false,
            closing: true,
        }
    }

    #[test]
    fn frontend_ready_binds_capture_metadata_on_gui_and_script_transfer_keeps_its_lifetime() {
        let mut app = fixture_app();
        let observer = app.activity.observer();
        let directory = std::env::temp_dir().join(format!(
            "protoinputmultiplex-render-bind-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("ui.json");
        std::fs::write(&path, b"[]").unwrap();
        let guards = RuntimeGuards::for_test(&directory).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let loader = std::thread::spawn(move || {
            let script = crate::ui_script::Script::load(path).unwrap();
            sender
                .send(BootstrapMessage::FrontendReady {
                    script: Box::new(script),
                    guards,
                })
                .unwrap_or_else(|_| panic!("frontend receiver closed"));
        });
        app.pending = Some(PendingBootstrap {
            receiver,
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        loader.join().unwrap();
        assert!(
            observer.snapshot().render_capture.is_none(),
            "loader must not bind GUI writer"
        );
        app.poll_bootstrap();
        let initial = observer.snapshot().render_capture.unwrap();
        assert!(initial.enabled && !initial.retired);
        assert_eq!(initial.requested_generation, 0);
        // The production Dashboard handoff moves this same Option<Script>.
        // Exercise that ownership move without constructing native Dashboard services.
        let transferred = app.script.take().expect("GUI-owned frontend Script");
        transferred.wait_for_io();
        assert_eq!(
            observer.snapshot().render_capture.unwrap().script_id,
            initial.script_id
        );
        assert!(!observer.snapshot().render_capture.unwrap().retired);
        drop(transferred);
        assert!(observer.snapshot().render_capture.unwrap().retired);
        drop(app);
        assert!(observer.snapshot().observation.unwrap().closed);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn real_outer_callbacks_mark_early_returns_once_and_close_with_the_owner() {
        let mut app = fixture_app();
        let (mut handle, requests) = crate::gui_bridge::app_channel();
        handle.attach_activity(app.activity.observer());
        let ctx = egui::Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        eframe::App::raw_input_hook(&mut app, &ctx, &mut egui::RawInput::default());
        eframe::App::logic(&mut app, &ctx, &mut frame);
        let mut ui_calls = 0u64;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui_calls += 1;
            eframe::App::ui(&mut app, ui, &mut frame)
        });
        let snapshot = handle.activity_snapshot().unwrap();
        assert!(snapshot.coherent);
        let observation = snapshot.observation.unwrap();
        for counter in [observation.raw_input, observation.logic] {
            assert_eq!(
                (counter.entered, counter.returned, counter.unwound),
                (1, 1, 0)
            );
        }
        assert_eq!(
            (
                observation.ui.entered,
                observation.ui.returned,
                observation.ui.unwound
            ),
            (ui_calls, ui_calls, 0)
        );
        assert!(!observation.in_callback && !observation.closed);
        assert!(requests.try_recv().is_err());
        drop(app);
        assert!(
            handle
                .activity_snapshot()
                .unwrap()
                .observation
                .unwrap()
                .closed
        );
    }
}

pub fn run(
    data_dir: Option<PathBuf>,
    profile: Option<PathBuf>,
    script: Option<PathBuf>,
    options: GuiRunOptions,
    preferences_path: Option<PathBuf>,
    startup_test_key: Option<String>,
    delays: WorkerDelays,
) -> Result<()> {
    let renderer = options.renderer;
    let native_options = crate::renderer_config::renderer_options(renderer == crate::GuiRenderer::SoftwareDx12)?;
    ensure!(
        script.is_some() || delays.discovery.is_zero(),
        "Discovery delay requires the opt-in GUI script"
    );
    ensure!(
        script.is_some() || delays.start_preflight.is_zero(),
        "Start preflight delay requires the opt-in GUI script"
    );
    ensure!(
        script.is_some() || delays.profile_file.is_zero(),
        "Profile file delay requires the opt-in GUI script"
    );
    ensure!(
        script.is_some() || delays.app_preferences.is_zero(),
        "Application preference delay requires the opt-in GUI script"
    );
    ensure!(
        script.is_some() || delays.bootstrap.is_zero(),
        "Bootstrap delay requires the opt-in GUI script"
    );
    ensure!(
        delays.bootstrap <= Duration::from_secs(15),
        "Bootstrap delay exceeds 15 seconds"
    );
    let inputs = BootstrapInputs {
        data_dir,
        preferences_path,
        startup_test_key,
        script_path: script,
        script_screenshots: options.script_screenshots,
        preferences_delay: delays.app_preferences,
        bootstrap_delay: delays.bootstrap,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([360.0, 360.0])
            .with_min_inner_size([320.0, 360.0]),
        ..native_options
    };
    eframe::run_native(
        crate::APP_NAME,
        options,
        Box::new(move |cc| {
            crate::renderer_config::verify_renderer(renderer == crate::GuiRenderer::SoftwareDx12, cc)?;
            Theme::install(
                &cc.egui_ctx,
                theme_mode(crate::app_preferences::Preferences::default().appearance_mode),
            );
            apply_muted_text(&cc.egui_ctx);
            install_gui_fonts(&cc.egui_ctx, None);
            Ok(Box::new(BootstrapApp::new(inputs, profile, delays)))
        }),
    )
    .map_err(|error| anyhow::anyhow!("GUI: {error}"))
}

#[cfg(test)]
mod bootstrap_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    fn fixture() -> (PathBuf, PathBuf) {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let parent = std::env::temp_dir().join(format!(
            "protoinputmultiplex-bootstrap-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&parent).unwrap();
        let root = parent.join("data");
        std::fs::create_dir(&root).unwrap();
        (parent, root.join("preferences.json"))
    }

    fn inputs(path: PathBuf, delay: Duration) -> BootstrapInputs {
        BootstrapInputs {
            data_dir: None,
            preferences_path: Some(path),
            startup_test_key: None,
            script_path: None,
            script_screenshots: true,
            preferences_delay: Duration::ZERO,
            bootstrap_delay: delay,
        }
    }

    #[test]
    fn malformed_preferences_fail_without_replacing_the_source() {
        let (parent, path) = fixture();
        let damaged = b"{ damaged preferences";
        std::fs::write(&path, damaged).unwrap();
        let (sender, _receiver) = std::sync::mpsc::channel();
        let mut guards = None;
        let error = prepare_bootstrap(
            inputs(path.clone(), Duration::ZERO),
            None,
            false,
            &mut guards,
            &AtomicBool::new(false),
            &sender,
        )
        .err()
        .expect("invalid preferences must block GUI readiness");
        assert!(format!("{error:#}").contains(&path.display().to_string()));
        assert_eq!(std::fs::read(&path).unwrap(), damaged);
        drop(guards);
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn cancellation_interrupts_the_worker_delay_before_preferences_load() {
        let (parent, path) = fixture();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let (sender, _receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut guards = None;
            prepare_bootstrap(
                inputs(path, Duration::from_secs(5)),
                None,
                false,
                &mut guards,
                &worker_cancelled,
                &sender,
            )
        });
        std::thread::sleep(Duration::from_millis(80));
        cancelled.store(true, Ordering::Release);
        let start = Instant::now();
        let error = worker.join().unwrap().err().expect("cancelled startup");
        assert!(format!("{error:#}").contains("cancelled"));
        assert!(start.elapsed() < Duration::from_secs(1));
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn close_disallows_a_same_frame_retry() {
        let app_inputs = inputs(PathBuf::from("unused-preferences.json"), Duration::ZERO);
        let mut app = BootstrapApp {
            activity: crate::gui_activity::Probe::new(),
            inputs: app_inputs,
            profile: None,
            delays: WorkerDelays {
                discovery: Duration::ZERO,
                start_preflight: Duration::ZERO,
                profile_file: Duration::ZERO,
                app_preferences: Duration::ZERO,
                bootstrap: Duration::ZERO,
            },
            pending: None,
            ready: None,
            script: None,
            frontend_guards: None,
            dashboard: None,
            failure: Some("failed".into()),
            assembly_retry: false,
            closing: true,
        };
        app.retry();
        assert!(app.pending.is_none());
        assert_eq!(app.failure.as_deref(), Some("failed"));
    }

    #[test]
    fn script_load_failure_retries_under_the_same_lease() {
        let (parent, preferences_path) = fixture();
        std::fs::write(&preferences_path, b"{ damaged preferences").unwrap();
        let script_path = preferences_path.with_file_name("ui.json");
        let mut input = inputs(preferences_path, Duration::ZERO);
        input.script_path = Some(script_path.clone());
        let first = spawn_bootstrap(input.clone(), None, false).unwrap();
        let BootstrapMessage::Finished(Err(failure)) =
            first.receiver.recv_timeout(Duration::from_secs(2)).unwrap()
        else {
            panic!("missing script must fail before frontend handoff");
        };
        assert!(failure.message.contains("ui.json"));
        let guards = failure.guards.expect("failed owner retains its leases");
        drop(first);

        std::fs::write(&script_path, b"[]").unwrap();
        let second = spawn_bootstrap(input, Some(Arc::clone(&guards)), false).unwrap();
        let BootstrapMessage::FrontendReady {
            script,
            guards: frontend_guards,
        } = second
            .receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
        else {
            panic!("retry must load the formerly missing script");
        };
        assert!(Arc::ptr_eq(&guards, &frontend_guards));
        drop(frontend_guards);
        let BootstrapMessage::Finished(Err(failure)) = second
            .receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
        else {
            panic!("damaged preferences must still block readiness");
        };
        assert!(failure.message.contains("preferences.json"));
        drop(failure);
        drop(second);
        drop(script);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Arc::strong_count(&guards) > 1 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(Arc::strong_count(&guards), 1);
        drop(guards);
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn close_while_pending_cancels_without_resurrection() {
        let (parent, path) = fixture();
        let input = inputs(path, Duration::from_secs(5));
        let mut app = BootstrapApp::new(
            input,
            None,
            WorkerDelays {
                discovery: Duration::ZERO,
                start_preflight: Duration::ZERO,
                profile_file: Duration::ZERO,
                app_preferences: Duration::ZERO,
                bootstrap: Duration::from_secs(5),
            },
        );
        let cancelled = Arc::clone(&app.pending.as_ref().unwrap().cancelled);
        app.close(&egui::Context::default());
        app.retry();
        assert!(app.closing && app.pending.is_none() && app.ready.is_none());
        assert!(cancelled.load(Ordering::Acquire));
        let deadline = Instant::now() + Duration::from_secs(2);
        while Arc::strong_count(&cancelled) > 1 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(Arc::strong_count(&cancelled), 1);
        std::fs::remove_dir_all(parent).unwrap();
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Page {
    Home,
    Apps,
    Settings,
    AppPreferences,
    Users,
    User,
    Preview,
    Profiles,
    Lab,
    Automation,
    Annotation,
    Magnifier,
    MacroStudio,
    Replication,
    Forwarding,
    InputMapper,
    Screen,
    VirtualUsers,
    Agent,
    Output,
}

struct BuiltInApp {
    label: &'static str,
    page: Page,
    description: &'static str,
}

const BUILT_IN_APPS: &[BuiltInApp] = &[
    BuiltInApp {
        label: "Input Mapper",
        page: Page::InputMapper,
        description: "Build typed input graphs or start from a template. Apply the graph and arm it separately when you want live input.",
    },
    BuiltInApp {
        label: "Macro Studio",
        page: Page::MacroStudio,
        description: "Create and edit per-user input clips. Record and play them through an active routing session.",
    },
    BuiltInApp {
        label: "Annotation",
        page: Page::Annotation,
        description: "Configure each user's drawing tools and annotate an eligible surface during routing.",
    },
    BuiltInApp {
        label: "Magnifier",
        page: Page::Magnifier,
        description: "Set up each user's lens and magnify the selected source during routing.",
    },
    BuiltInApp {
        label: "Replication Graph",
        page: Page::Replication,
        description: "Build and inspect a graph that copies selected pointer events between users.",
    },
    BuiltInApp {
        label: "Input Forwarding",
        page: Page::Forwarding,
        description: "Route pointer movement and eligible keyboard input between virtual users in automatic routing.",
    },
    BuiltInApp {
        label: "Screen tools",
        page: Page::Screen,
        description: "Inspect a selected target snapshot, sample a pixel, or find a captured template. No input is sent.",
    },
];

fn launcher_app_row(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let width = ui.available_width().min(
        (ui.clip_rect().right().min(ui.max_rect().right()) - ui.next_widget_position().x).max(0.0),
    );
    let border = if selected {
        core::BORDER_FOCUS
    } else {
        core::BORDER_THIN
    };
    let content_width = (width - 2.0 * (core::SPACE_1 + border)).max(0.0);
    Surface::new()
        .interactive()
        .selected(selected)
        .pad(core::SPACE_1)
        .id_source(("launcher-app", label))
        .show(ui, |ui| {
            ui.set_width(content_width);
            Text::new(label).wrap().show(ui);
        })
        .response
}

#[cfg(test)]
mod launcher_row_tests {
    use super::*;
    #[test]
    fn actual_launcher_rows_fit_the_clip_at_normal_and_compact_widths() {
        for width in [320.0, 540.0] {
            let ctx = egui::Context::default();
            Theme::install(&ctx, Mode::Dark);
            for _ in 0..2 {
                let _ = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 480.0),
                        )),
                        ..Default::default()
                    },
                    |root| {
                        egui::CentralPanel::default().show_inside(root, |ui| {
                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    for (index, app) in BUILT_IN_APPS.iter().enumerate() {
                                        let row = launcher_app_row(ui, app.label, index == 0);
                                        assert!(
                                            row.interact_rect.contains_rect(row.rect),
                                            "{width}: {} {:?} {:?}",
                                            app.label,
                                            row.rect,
                                            row.interact_rect
                                        );
                                        assert!(ui.clip_rect().contains_rect(row.rect));
                                    }
                                });
                        });
                    },
                );
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HomeAccess {
    Configure,
    Settings,
    Hide,
    Apps,
}

impl HomeAccess {
    fn label(self) -> &'static str {
        match self {
            Self::Configure => "Configure user",
            Self::Settings => "Settings",
            Self::Hide => "Hide",
            Self::Apps => "Apps",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Configure => "Alt+C",
            Self::Settings => "Alt+S",
            Self::Hide => "Alt+H",
            Self::Apps => "Alt+A",
        }
    }

    fn caption(self, show_hint: bool) -> String {
        if show_hint {
            format!(
                "{} · {}",
                self.label(),
                self.hint().trim_start_matches("Alt+")
            )
        } else {
            self.label().to_owned()
        }
    }
}

fn home_access_key(ctx: &egui::Context) -> Option<HomeAccess> {
    if egui::Popup::is_any_open(ctx) || ctx.any_popup_open() || ctx.text_edit_focused() {
        return None;
    }
    ctx.input(|input| {
        if !input.focused {
            return None;
        }
        input.events.iter().find_map(|event| match event {
            egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } if *modifiers == egui::Modifiers::ALT => match key {
                egui::Key::C => Some(HomeAccess::Configure),
                egui::Key::S => Some(HomeAccess::Settings),
                egui::Key::H => Some(HomeAccess::Hide),
                egui::Key::A => Some(HomeAccess::Apps),
                _ => None,
            },
            _ => None,
        })
    })
}

#[derive(Clone, Copy)]
enum MapperPublisher {
    Direct,
    Agent,
}

fn update_mapper_status(
    status: &mut String,
    previous_worker_error: Option<&str>,
    error: Option<&str>,
) {
    if let Some(error) = error {
        *status = format!("Input Mapper: {error}");
    } else if previous_worker_error.is_some_and(|old| *status == format!("Input Mapper: {old}")) {
        *status = "Input Mapper state updated.".into();
    }
}

fn mapper_reply_id(
    state: &crate::mapper_runtime::State,
    publisher: MapperPublisher,
) -> Option<u64> {
    match publisher {
        MapperPublisher::Direct => state.reply_to,
        MapperPublisher::Agent => None,
    }
}

fn receive_mapper_page(
    page: &mut crate::mapper_ui::MapperPage,
    state: &crate::mapper_runtime::State,
    publisher: MapperPublisher,
) -> Option<mux_core::mapper::Definition> {
    if let Some(command_id) = mapper_reply_id(state, publisher) {
        page.receive_reply(command_id, state)
    } else {
        page.receive_external(state);
        None
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SettingsCategory {
    #[default]
    Application,
    UsersDevices,
    Automation,
    Tools,
    AgentAccess,
}

impl SettingsCategory {
    const ALL: [Self; 5] = [
        Self::Application,
        Self::UsersDevices,
        Self::Automation,
        Self::Tools,
        Self::AgentAccess,
    ];

    fn for_page(page: Page) -> Option<Self> {
        Self::ALL.into_iter().find(|category| {
            category
                .destinations()
                .iter()
                .any(|(_, destination)| *destination == page)
        })
    }

    fn label(self) -> &'static str {
        match self {
            Self::Application => "Application",
            Self::UsersDevices => "Users & devices",
            Self::Automation => "Automation",
            Self::Tools => "Tools",
            Self::AgentAccess => "Agent access",
        }
    }

    fn destinations(self) -> &'static [(&'static str, Page)] {
        match self {
            Self::Application => &[("Application preferences", Page::AppPreferences)],
            Self::UsersDevices => &[
                ("Users & devices", Page::Users),
                ("Profiles & backend", Page::Profiles),
            ],
            Self::Automation => &[
                ("Macros & replication", Page::Automation),
                ("Macro Studio", Page::MacroStudio),
                ("Replication Graph", Page::Replication),
                ("Input Forwarding", Page::Forwarding),
                ("Input Mapper", Page::InputMapper),
                ("Virtual Replicas", Page::VirtualUsers),
            ],
            Self::Tools => &[
                ("Screen tools", Page::Screen),
                ("Live preview", Page::Preview),
                ("Input lab", Page::Lab),
                ("Annotation", Page::Annotation),
                ("Magnifier", Page::Magnifier),
                ("Output Rules", Page::Output),
            ],
            Self::AgentAccess => &[("Agent Access", Page::Agent)],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum UserTab {
    Pointer,
    Keyboard,
    Target,
    Motion,
    Buttons,
    Wheel,
    Theme,
    Overlay,
    Lock,
    Draw,
    Magnifier,
    Macros,
}

// Each page retains its position without transferring it to a different page.
fn page_scroll_area(page: Page) -> egui::ScrollArea {
    egui::ScrollArea::vertical().id_salt(("dashboard-page-scroll", page))
}

fn user_tab_scroll_area(seat_id: &str, tab: UserTab) -> egui::ScrollArea {
    egui::ScrollArea::vertical().id_salt(("user-tab-content", seat_id, tab))
}

// These controllers own their scrolling; scope only their seat/tab-owned body.
fn with_user_tab_content<R>(
    ui: &mut egui::Ui,
    seat_id: &str,
    tab: UserTab,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.push_id(("user-tool-content", seat_id, tab), body).inner
}

fn selection_window_title(title: &str) -> String {
    title
        .chars()
        .filter(|character| !character.is_control())
        .take(48)
        .collect()
}

fn window_chooser_display_label(ui: &egui::Ui, label: &str, max_width: f32) -> String {
    let Some((title, identity)) = label.rsplit_once(" (PID ") else {
        return label.to_owned();
    };
    let suffix = format!(" (PID {identity}");
    // MenuItem renders Text's body role, including its tracking. Measure that
    // exact Ouroboros style rather than egui's unrelated default font.
    let style = typography::body();
    let fits = |text: String| {
        let mut job = egui::text::LayoutJob::simple_singleline(
            text,
            style.font_id(),
            Theme::get(ui).foreground,
        );
        for section in &mut job.sections {
            section.format.extra_letter_spacing = style.tracking;
        }
        ui.fonts_mut(|fonts| fonts.layout_job(job).size().x <= max_width.max(0.))
    };
    if fits(label.to_owned()) {
        return label.to_owned();
    }
    let mut boundaries: Vec<usize> = title.char_indices().map(|(offset, _)| offset).collect();
    boundaries.push(title.len());
    let (mut low, mut high) = (0, boundaries.len() - 1);
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if fits(format!("{}...{suffix}", &title[..boundaries[middle]])) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    let display = format!("{}...{suffix}", &title[..boundaries[low]]);
    if fits(display.clone()) {
        display
    } else {
        suffix.trim_start().to_owned()
    }
}

fn selection_snapshot_index(
    candidate: &crate::window_selection::Selection,
    windows: &[(Target, targets::TargetEligibility)],
) -> Option<usize> {
    if targets::parse_creation_token(&candidate.creation_token).is_err() {
        return None;
    }
    windows.iter().position(|(window, readiness)| {
        candidate.pid == window.pid
            && candidate.hwnd == window.hwnd
            && readiness.creation_token.as_deref() == Some(candidate.creation_token.as_str())
    })
}

fn selection_ack_matches(
    pending: Option<&crate::window_selection::Selection>,
    acknowledgement: &crate::window_selection::Acknowledgement,
) -> bool {
    pending.is_some_and(|pending| {
        !pending.request_id.is_empty() && pending == &acknowledgement.selection
    })
}

#[derive(Clone, Copy)]
struct MacroLoadConditions {
    has_session: bool,
    ready: bool,
    stopping: bool,
    busy: bool,
    recording: bool,
    playing: bool,
    studio_active: bool,
}

fn macro_load_can_dispatch(
    requested: crate::gui_macro_file::ContextId,
    current: crate::gui_macro_file::ContextId,
    conditions: MacroLoadConditions,
) -> bool {
    requested == current
        && conditions.has_session
        && conditions.ready
        && !conditions.stopping
        && !conditions.busy
        && !conditions.recording
        && !conditions.playing
        && !conditions.studio_active
}

fn macro_file_conflicts_with_automation(
    state: &crate::automation_runtime::State,
    observed_revision: u64,
    recording: bool,
    loaded_events: usize,
    playback_state: Option<&str>,
) -> bool {
    state.macro_revision != observed_revision
        || state.recording != recording
        || state.loaded_events != loaded_events
        || state.playback.as_ref().map(|play| play.state.as_str()) != playback_state
        || (state.acknowledged
            && state.error.is_none()
            && (state.clip.is_some() || state.recording || state.playback.is_some()))
}

fn macro_file_conflicts_with_studio(state: &crate::macro_studio::State) -> bool {
    state.acknowledged && state.error.is_none()
}

#[derive(Default)]
struct AttachmentNotice {
    opening: Option<u32>,
    failure: Option<String>,
}
impl AttachmentNotice {
    fn receive(&mut self, event: &SessionEvent) {
        match event {
            SessionEvent::Attaching(pid) => {
                self.opening = Some(*pid);
                self.failure = None;
            }
            SessionEvent::AttachmentFinished { pid, error } if self.opening == Some(*pid) => {
                self.opening = None;
                self.failure = error
                    .as_ref()
                    .map(|error| format!("Application {pid} could not be prepared: {error}"));
            }
            SessionEvent::Stopped | SessionEvent::Failed(_) => *self = Self::default(),
            _ => {}
        }
    }
    fn message(&self) -> Option<String> {
        self.opening
            .map(|pid| format!("Preparing application {pid}…"))
            .or_else(|| self.failure.clone())
    }
}

struct PreferencesPending {
    ticket: u64,
    editor: Option<crate::app_preferences_ui::PreferencesEditorSubmission>,
    request: Option<crate::gui_bridge::AppRequest>,
    setup_request: Option<crate::gui_setup::Request>,
}
#[derive(Clone, serde::Serialize)]
struct SetupStartOrder {
    retired_broker_generation: u64,
    broker_generation_before_controller: u64,
    broker_phase_before_controller: String,
    controller_epoch: u64,
}

fn broker_phase_label(phase: &crate::gui_agent_broker::Phase) -> &'static str {
    match phase {
        crate::gui_agent_broker::Phase::Stopped => "stopped",
        crate::gui_agent_broker::Phase::Starting => "starting",
        crate::gui_agent_broker::Phase::Active => "active",
        crate::gui_agent_broker::Phase::Stopping => "stopping",
        crate::gui_agent_broker::Phase::StartFailed(_) => "start_failed",
        crate::gui_agent_broker::Phase::CleanupFailed(_) => "cleanup_failed",
        crate::gui_agent_broker::Phase::WorkerFailed(_) => "worker_failed",
    }
}

#[derive(Clone, Copy)]
struct RecoveryBridgeCursor {
    key: (u64, u64, u64, u64),
    ticket: (u64, u64, u64, Instant),
    last_sample_sequence: u64,
}
impl RecoveryBridgeCursor {
    fn new(generation: u64, ticket: (u64, u64, u64, Instant)) -> Self {
        Self {
            key: (generation, ticket.0, ticket.1, ticket.2),
            ticket,
            last_sample_sequence: 0,
        }
    }

    fn next_fresh(
        &mut self,
        published: &crate::session_recovery::PublishedState,
        ticket: (u64, u64, u64, Instant),
    ) -> Option<(
        (u64, u64, u64, Instant),
        crate::session_recovery::NativeSample,
    )> {
        let key = (published.generation, ticket.0, ticket.1, ticket.2);
        if self.key != key {
            *self = Self::new(published.generation, ticket);
        } else if self.ticket.3.elapsed() > Duration::from_secs(2) {
            self.ticket = ticket;
        }
        let sample = published.native_sample.as_ref()?;
        if sample.generation != published.generation
            || sample.source_revision != published.revision
            || sample.sequence <= self.last_sample_sequence
            || sample.poll_started < self.ticket.3
        {
            return None;
        }
        let accepted_ticket = self.ticket;
        self.last_sample_sequence = sample.sequence;
        self.ticket = ticket;
        Some((accepted_ticket, sample.clone()))
    }
}

#[cfg(test)]
mod recovery_bridge_cursor_tests {
    use super::*;

    #[test]
    fn new_recovery_and_bridge_revision_admit_their_first_fresh_sample() {
        let first = crate::session_recovery::Recovery::starting();
        let first_ticket = (1, 1, 1, Instant::now());
        first.poll_native();
        let first_view = first.published();
        let mut cursor = RecoveryBridgeCursor::new(first_view.generation, first_ticket);
        assert!(cursor.next_fresh(&first_view, first_ticket).is_some());

        let second = crate::session_recovery::Recovery::starting();
        let second_ticket = (2, 2, 1, Instant::now());
        second.poll_native();
        let second_view = second.published();
        assert_eq!(second_view.native_sample.as_ref().unwrap().sequence, 1);
        assert!(cursor.next_fresh(&second_view, second_ticket).is_some());

        let changed_policy_ticket = (2, 2, 2, Instant::now());
        assert!(
            cursor
                .next_fresh(&second_view, changed_policy_ticket)
                .is_none()
        );
        second.poll_native();
        assert!(
            cursor
                .next_fresh(&second.published(), changed_policy_ticket)
                .is_some()
        );
    }
}

#[derive(Default)]
struct UserPauseUi {
    states: Vec<crate::routing_engine::UserPauseState>,
    pending: Option<(crate::session::SettingsTicket, usize, bool)>,
    next_request: u64,
}
impl UserPauseUi {
    fn reset(&mut self) {
        self.states.clear();
        self.pending = None;
    }
    fn publish(&mut self, state: crate::routing_engine::UserPauseState) {
        self.states.retain(|old| old.user != state.user);
        self.states.push(state);
    }
    fn issue(&mut self, user: usize, identity: String, paused: bool) -> Option<Request> {
        if self.pending.is_some() {
            return None;
        }
        self.next_request = self.next_request.checked_add(1)?;
        let ticket = crate::session::SettingsTicket {
            request: self.next_request,
            identity,
        };
        self.pending = Some((ticket.clone(), user, paused));
        Some(Request::UserPause {
            user,
            paused,
            gui: Some(ticket),
        })
    }
    fn acknowledge(
        &mut self,
        ticket: &crate::session::SettingsTicket,
        state: &crate::routing_engine::UserPauseState,
        seats: &[Seat],
    ) -> bool {
        let Some((pending, user, paused)) = &self.pending else {
            return false;
        };
        if pending != ticket
            || *user != state.user
            || seats
                .get(*user)
                .is_none_or(|seat| seat.identity() != ticket.identity)
            || state.error.is_none() && (state.paused != *paused || state.pending_releases != 0)
        {
            return false;
        }
        self.pending = None;
        self.publish(state.clone());
        true
    }
    fn snapshot(&self, selected: Option<usize>) -> serde_json::Value {
        serde_json::json!({"selected_user":selected,"users":self.states,
            "pending":self.pending.as_ref().map(|(ticket,user,paused)|serde_json::json!({
                "request":ticket.request,"identity":ticket.identity,"user":user,"paused":paused}))})
    }
}

#[cfg(test)]
mod user_pause_ui_tests {
    use super::*;
    #[test]
    fn publication_cannot_consume_ticket_and_stop_or_replacement_rejects_old_receipts() {
        let seats: Vec<Seat> = serde_json::from_value(serde_json::json!([
            {"id":"a","name":"A","mouse":"virtual://mouse/1","keyboard":"virtual://keyboard/1"},
            {"id":"b","name":"B","mouse":"virtual://mouse/2","keyboard":"virtual://keyboard/2"}
        ]))
        .unwrap();
        let mut page = UserPauseUi::default();
        let Request::UserPause {
            gui: Some(ticket), ..
        } = page.issue(0, seats[0].identity(), true).unwrap()
        else {
            panic!("missing private request");
        };
        let state = crate::routing_engine::UserPauseState {
            user: 0,
            supported: true,
            paused: true,
            ..Default::default()
        };
        page.publish(state.clone());
        assert!(page.pending.is_some());
        let mut wrong_result = state.clone();
        wrong_result.paused = false;
        assert!(!page.acknowledge(&ticket, &wrong_result, &seats));
        assert!(page.issue(1, seats[1].identity(), true).is_none());
        let mut wrong_user = state.clone();
        wrong_user.user = 1;
        assert!(!page.acknowledge(&ticket, &wrong_user, &seats));
        let mut replacement = seats.clone();
        replacement[0].id = "replacement".into();
        assert!(!page.acknowledge(&ticket, &state, &replacement));
        assert!(page.pending.is_some());
        assert!(page.acknowledge(&ticket, &state, &seats));
        assert!(page.pending.is_none());
        assert!(!page.acknowledge(&ticket, &state, &seats));
        page.reset();
        assert!(!page.acknowledge(&ticket, &state, &seats));
        let Request::UserPause {
            gui: Some(next), ..
        } = page.issue(0, seats[0].identity(), false).unwrap()
        else {
            panic!("missing new private request");
        };
        assert!(next.request > ticket.request);
        assert!(!page.acknowledge(&ticket, &state, &seats));
        let failed = crate::routing_engine::UserPauseState {
            pending_releases: 1,
            error: Some("Saved UP failed".into()),
            ..state
        };
        assert!(page.acknowledge(&next, &failed, &seats));
        assert_eq!(page.states[0].pending_releases, 1);
        assert!(page.states[0].paused);
    }
}

struct Dashboard {
    _runtime_guards: Arc<RuntimeGuards>,
    action_frame: u64,
    frame_actions: BTreeSet<String>,
    script: Option<crate::ui_script::Script>,
    lifecycle_trace: Option<LifecycleTrace>,
    theme_cursor_textures: BTreeMap<[u8; 3], egui::TextureHandle>,
    states: Vec<crate::router::State>,
    lab_positions: Vec<[i32; 2]>,
    capture: Capture,
    devices: Vec<Device>,
    pointers: BTreeMap<usize, Pointer>,
    keyboard_packets: BTreeMap<usize, u64>,
    cursor_settings: crate::cursor_settings_ui::CursorSettingsEditor,
    profile: Profile,
    profile_path: String,
    app_preferences_path: PathBuf,
    app_preferences: crate::app_preferences::Preferences,
    appearance_applied: (crate::app_preferences::AppearanceMode, u16),
    minimum_size_pending: bool,
    app_preferences_page: crate::app_preferences_ui::PreferencesPage,
    lifecycle: crate::app_lifecycle::Handle,
    lifecycle_actions: Receiver<crate::app_lifecycle::LifecycleAction>,
    preferences_worker: crate::gui_app_preferences::Service,
    preferences_pending: Option<PreferencesPending>,
    preferences_quit_waiting: bool,
    app_request_handle: crate::gui_bridge::AppHandle,
    app_requests: Receiver<crate::gui_bridge::AppRequest>,
    quit_after_stop: bool,
    close_ready: bool,
    allow_close: bool,
    restore_started_frame: Option<u64>,
    session_stopping: bool,
    recovery: crate::session_recovery::Recovery,
    recovery_bridge_cursor: Option<RecoveryBridgeCursor>,
    recovery_phase: crate::session_recovery::Phase,
    recovery_dialog_open: bool,
    input_release_dialog_open: bool,
    profile_transfer_path: String,
    profile_feedback: Option<(String, bool)>,
    profile_file: crate::gui_profile_file::Service,
    profile_file_ui: crate::gui_profile_file::UiState,
    profile_exit: crate::gui_profile_policy::ExitCoordinator,
    profile_install_epoch: u64,
    profile_interaction_epoch: u64,
    initial_profile_size: Option<egui::Vec2>,
    profile_undo: Option<Profile>,
    status: String,
    attachment: AttachmentNotice,
    last_refresh: Instant,
    discovery: crate::gui_discovery::Service,
    discovery_applied_generation: u64,
    discovery_devices_ok: bool,
    discovery_picker_revision: u64,
    discovery_picker: Option<(crate::gui_discovery::PickerScope, String)>,
    selected: Option<usize>,
    page: Page,
    viewport_page: Page,
    selected_app: usize,
    app_info_open: bool,
    settings_category: SettingsCategory,
    user_tab: UserTab,
    user_tabs_width: f32,
    show_devices: bool,
    preview_paused: bool,
    targets: Vec<Target>,
    start_readiness: Option<(Vec<u32>, bool, Option<String>)>,
    target_readiness: BTreeMap<u32, targets::TargetEligibility>,
    selection_windows: Vec<(Target, targets::TargetEligibility)>,
    selection_candidate: Option<crate::window_selection::Selection>,
    selection_highlight: Option<(usize, usize)>,
    selection_pending: Option<crate::window_selection::Selection>,
    staged_window_locks: crate::gui_window_lock::StagedWindows,
    startup_window_selections: VecDeque<crate::window_selection::Selection>,
    startup_window_selection_active: bool,
    startup_window_selection_error: Option<String>,
    selection_error: Option<(usize, String)>,
    monitors: Vec<mux_windows::monitors::Monitor>,
    monitor_error: Option<String>,
    desktop_bounds: Option<mux_core::desktop::Bounds>,
    region_draft: mux_core::desktop::Bounds,
    region_draft_user: Option<usize>,
    runtime: String,
    session: Option<Controller>,
    macro_file_session_epoch: u64,
    macro_file_mutation_epoch: u64,
    macro_file_observed_revision: u64,
    macro_file: crate::gui_macro_file::Service,
    macro_file_result: Option<&'static str>,
    start_preflight_delay: Duration,
    session_ready: bool,
    session_paused: bool,
    user_pause: UserPauseUi,
    session_busy: bool,
    mapper_owns_session_busy: bool,
    mapper_commands: BTreeSet<u64>,
    key_name: String,
    text_input: String,
    macro_path: String,
    macro_loops: u32,
    macro_repeat: bool,
    macro_speed: f64,
    macro_clip: Option<Arc<mux_core::automation::Clip>>,
    macro_recording: bool,
    macro_recorded_events: usize,
    macro_loaded_events: usize,
    macro_playback: Option<crate::automation_runtime::PlayState>,
    macro_error: Option<String>,
    replication_edges: Vec<crate::automation_runtime::Edge>,
    edge_source: usize,
    edge_target: usize,
    edge_mouse: bool,
    edge_keyboard: bool,
    edge_text: bool,
    replication_dirty: bool,
    replication_revision: u64,
    replication_page: crate::replication_ui::ReplicationPage,
    forwarding_page: crate::forwarding_ui::ForwardingPage,
    mapper_page: crate::mapper_ui::MapperPage,
    screen_page: crate::screen_ui::ScreenPage,
    screen_targets: Vec<crate::session::CaptureTarget>,
    annotation_page: crate::annotation_ui::AnnotationPage,
    magnifier_page: crate::magnifier_ui::MagnifierPage,
    macro_studio_page: crate::macro_studio_ui::MacroStudioPage,
    virtual_users_page: crate::virtual_users_ui::VirtualUsersPage,
    agent_page: crate::agent_ui::AgentPage,
    setup_page: crate::agent_ui::SetupPage,
    setup_handle: crate::gui_setup::Handle,
    setup_requests: Receiver<crate::gui_setup::Request>,
    setup_pending: Option<(u64, u64)>,
    setup_retiring: bool,
    setup_retired_generation: Option<u64>,
    setup_start_order: Option<SetupStartOrder>,
    setup_file_pending: Option<(u64, crate::gui_setup::Request)>,
    setup_revision: u64,
    output_page: crate::output_ui::OutputPage,
    agent_broker: crate::gui_agent_broker::Service,
    sdk_service: Option<crate::gui_sdk::Service>,
    application_events: crate::gui_application_events::Controller,
    application_event_tab: bool,
    sdk_origins: String,
    agent_sdk_tab: bool,
    agent_session_epoch: u64,
    agent_pending_enable: Option<(u64, u64, crate::agent_runtime::Command)>,
    restart_requested: bool,
    restart_discovery_generation: u64,
    agent_descriptor_path: String,
    agent_executable_path: Option<String>,
    virtual_users_base_seat_count: Option<usize>,
}
impl Dashboard {
    fn macro_file_context(&self) -> crate::gui_macro_file::ContextId {
        crate::gui_macro_file::ContextId {
            session: self.macro_file_session_epoch,
            mutation: self.macro_file_mutation_epoch,
            macro_revision: self.macro_file_observed_revision,
        }
    }

    fn invalidate_macro_file(&mut self) {
        self.macro_file_mutation_epoch = self.macro_file_mutation_epoch.saturating_add(1);
        if self.macro_file.pending() {
            self.macro_file.invalidate();
            self.macro_file_result = Some("invalidated");
        }
    }

    fn advance_macro_file_session(&mut self) {
        self.macro_file_session_epoch = self.macro_file_session_epoch.saturating_add(1);
        self.macro_file_observed_revision = 0;
        self.invalidate_macro_file();
    }

    fn poll_macro_file(&mut self) {
        use crate::gui_macro_file::{Kind, Value};
        let Some(completion) = self.macro_file.poll() else {
            return;
        };
        match (completion.kind, completion.result) {
            (Kind::Save, Ok(Value::Saved)) => {
                self.macro_error = None;
                self.macro_file_result = Some("saved");
            }
            (Kind::Load, Ok(Value::Loaded(clip))) => {
                let conditions = MacroLoadConditions {
                    has_session: self.session.is_some(),
                    ready: self.session_ready,
                    stopping: self.session_stopping,
                    busy: self.session_busy,
                    recording: self.macro_recording,
                    playing: self
                        .macro_playback
                        .as_ref()
                        .is_some_and(|p| p.state == "playing"),
                    studio_active: self.macro_studio_page.has_active_program(),
                };
                if macro_load_can_dispatch(
                    completion.context,
                    self.macro_file_context(),
                    conditions,
                ) {
                    if let Some(session) = &self.session {
                        session.request(Request::MacroFileLoad {
                            clip,
                            expected_macro_revision: completion.context.macro_revision,
                        });
                        self.macro_error = None;
                        self.macro_file_result = Some("load_requested");
                        self.session_busy = true;
                    }
                } else {
                    self.macro_file_result = Some("invalidated");
                }
            }
            (_, Err(error)) => {
                self.macro_error = Some(error);
                self.macro_file_result = Some("failed");
            }
            _ => {
                self.macro_error = Some("macro file worker returned the wrong result".into());
                self.macro_file_result = Some("failed");
            }
        }
    }
    fn sync_appearance(&mut self, ctx: &egui::Context) -> bool {
        let candidate = (
            self.app_preferences.appearance_mode,
            self.app_preferences.zoom_percent,
        );
        if candidate == self.appearance_applied {
            return false;
        }
        let zoom_changed = candidate.1 != self.appearance_applied.1;
        if candidate.0 != self.appearance_applied.0 {
            Theme::apply(ctx, theme_mode(candidate.0));
            apply_muted_text(ctx);
        }
        if zoom_changed {
            ctx.set_zoom_factor(candidate.1 as f32 / 100.0);
            self.minimum_size_pending = true;
        }
        self.appearance_applied = candidate;
        zoom_changed
    }

    fn new(
        ready: &mut BootstrapReady,
        delays: WorkerDelays,
        activity: crate::gui_activity::Observer,
    ) -> Result<Self> {
        let app_preferences_path = ready.preferences_path.clone();
        let app_preferences = ready.preferences.clone();
        let data_root = app_preferences_path
            .parent()
            .context("preferences path must have a parent directory")?
            .to_path_buf();
        let agent_executable_path = ready.agent_executable_path.clone();
        let mut discovery = if delays.discovery.is_zero() {
            crate::gui_discovery::Service::start()?
        } else {
            crate::gui_discovery::Service::start_delayed(delays.discovery)?
        };
        discovery.ensure(Default::default());
        let (mut app_request_handle, app_requests) = crate::gui_bridge::app_channel();
        let (mut setup_handle, setup_requests) = crate::gui_setup::channel();
        app_request_handle.attach_activity(activity.clone());
        setup_handle.attach_activity(activity);
        let mut app_preferences_page = crate::app_preferences_ui::PreferencesPage::default();
        app_preferences_page.reset(&app_preferences);
        // Every fallible UI assembly step runs before the bootstrap owner is consumed.
        let capture = Capture::start()?;
        let profile_file = crate::gui_profile_file::Service::start(
            delays.profile_file,
            Arc::clone(&ready.guards),
        )?;
        let macro_file = crate::gui_macro_file::Service::start(Arc::clone(&ready.guards))?;
        let agent_broker = crate::gui_agent_broker::Service::start(Arc::clone(&ready.guards))?;
        let lifecycle_actions = ready
            .lifecycle_actions
            .take()
            .expect("bootstrap lifecycle actions");
        let lifecycle_handle = ready
            .lifecycle_handle
            .take()
            .expect("bootstrap lifecycle handle");
        let preferences_worker = ready
            .preferences_worker
            .take()
            .expect("bootstrap preferences worker");
        Ok(Self {
            _runtime_guards: Arc::clone(&ready.guards),
            action_frame: 0,
            frame_actions: BTreeSet::new(),
            script: None,
            lifecycle_trace: None,
            theme_cursor_textures: BTreeMap::new(),
            states: Vec::new(),
            lab_positions: Vec::new(),
            capture,
            devices: Vec::new(),
            pointers: BTreeMap::new(),
            keyboard_packets: BTreeMap::new(),
            cursor_settings: crate::cursor_settings_ui::CursorSettingsEditor::default(),
            profile: Profile {
                concurrent: true,
                ..Default::default()
            },
            profile_path: data_root.join("profiles/local.json").display().to_string(),
            app_preferences_path,
            app_preferences: app_preferences.clone(),
            appearance_applied: (
                app_preferences.appearance_mode,
                app_preferences.zoom_percent,
            ),
            minimum_size_pending: true,
            app_preferences_page,
            lifecycle: lifecycle_handle,
            lifecycle_actions,
            preferences_worker,
            preferences_pending: None,
            preferences_quit_waiting: false,
            app_request_handle,
            app_requests,
            quit_after_stop: false,
            close_ready: false,
            allow_close: false,
            restore_started_frame: None,
            session_stopping: false,
            recovery: Default::default(),
            recovery_bridge_cursor: None,
            recovery_phase: Default::default(),
            recovery_dialog_open: false,
            input_release_dialog_open: false,
            profile_transfer_path: data_root
                .join("profiles/transfer.json")
                .display()
                .to_string(),
            profile_feedback: None,
            profile_file,
            profile_file_ui: Default::default(),
            profile_exit: Default::default(),
            profile_install_epoch: 0,
            profile_interaction_epoch: 0,
            initial_profile_size: None,
            profile_undo: None,
            status: "Ready to assign devices.".into(),
            attachment: AttachmentNotice::default(),
            last_refresh: Instant::now(),
            discovery,
            discovery_applied_generation: 0,
            discovery_devices_ok: false,
            discovery_picker_revision: 0,
            discovery_picker: None,
            selected: None,
            page: Page::Home,
            viewport_page: Page::Home,
            selected_app: 0,
            app_info_open: false,
            settings_category: SettingsCategory::Application,
            user_tab: UserTab::Pointer,
            user_tabs_width: 0.0,
            show_devices: false,
            preview_paused: false,
            targets: Vec::new(),
            start_readiness: None,
            target_readiness: BTreeMap::new(),
            selection_windows: Vec::new(),
            selection_candidate: None,
            selection_highlight: None,
            selection_pending: None,
            staged_window_locks: Default::default(),
            startup_window_selections: VecDeque::new(),
            startup_window_selection_active: false,
            startup_window_selection_error: None,
            selection_error: None,
            monitors: Vec::new(),
            monitor_error: None,
            desktop_bounds: None,
            region_draft: mux_core::desktop::Bounds::default(),
            region_draft_user: None,
            runtime: ready.runtime.clone(),
            session: None,
            macro_file_session_epoch: 0,
            macro_file_mutation_epoch: 0,
            macro_file_observed_revision: 0,
            macro_file,
            macro_file_result: None,
            start_preflight_delay: delays.start_preflight,
            session_ready: false,
            session_paused: false,
            user_pause: UserPauseUi::default(),
            session_busy: false,
            mapper_owns_session_busy: false,
            mapper_commands: BTreeSet::new(),
            key_name: "A".into(),
            text_input: String::new(),
            macro_path: data_root
                .join("macros/automation.json")
                .display()
                .to_string(),
            macro_loops: 1,
            macro_repeat: false,
            macro_speed: 1.0,
            macro_clip: None,
            macro_recording: false,
            macro_recorded_events: 0,
            macro_loaded_events: 0,
            macro_playback: None,
            macro_error: None,
            replication_edges: Vec::new(),
            edge_source: 0,
            edge_target: 1,
            edge_mouse: true,
            edge_keyboard: true,
            edge_text: true,
            replication_dirty: false,
            replication_revision: 0,
            replication_page: crate::replication_ui::ReplicationPage::default(),
            forwarding_page: crate::forwarding_ui::ForwardingPage::default(),
            screen_page: crate::screen_ui::ScreenPage::default(),
            screen_targets: Vec::new(),
            mapper_page: crate::mapper_ui::MapperPage::with_flow_root(
                &data_root,
                Arc::clone(&ready.guards),
            ),
            annotation_page: crate::annotation_ui::AnnotationPage::default(),
            magnifier_page: crate::magnifier_ui::MagnifierPage::default(),
            macro_studio_page: crate::macro_studio_ui::MacroStudioPage::default(),
            virtual_users_page: crate::virtual_users_ui::VirtualUsersPage::default(),
            agent_page: crate::agent_ui::AgentPage::default(),
            setup_page: crate::agent_ui::SetupPage::default(),
            setup_handle,
            setup_requests,
            setup_pending: None,
            setup_retiring: false,
            setup_retired_generation: None,
            setup_start_order: None,
            setup_file_pending: None,
            setup_revision: 1,
            output_page: crate::output_ui::OutputPage::default(),
            agent_broker,
            sdk_service: None,
            application_events: crate::gui_application_events::Controller::default(),
            application_event_tab: false,
            sdk_origins: String::new(),
            agent_sdk_tab: false,
            agent_session_epoch: 0,
            agent_pending_enable: None,
            restart_requested: false,
            restart_discovery_generation: 0,
            agent_descriptor_path: String::new(),
            agent_executable_path,
            virtual_users_base_seat_count: None,
        })
    }
    fn button(&mut self, ui: &mut egui::Ui, label: &str, enabled: bool) -> bool {
        let response = Button::new(label).enabled(enabled).show(ui);
        let accepted = self
            .script
            .as_mut()
            .is_none_or(|script| script.observe(label, &response));
        response.clicked() && accepted && self.consume_action(ui.ctx(), label)
    }
    fn prominent_button(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        enabled: bool,
        variant: Option<ButtonVariant>,
    ) -> bool {
        let button = Button::new(label).lg().enabled(enabled);
        let response = if let Some(variant) = variant {
            button.variant(variant).show(ui)
        } else {
            button.show(ui)
        };
        let accepted = self
            .script
            .as_mut()
            .is_none_or(|script| script.observe(label, &response));
        response.clicked() && accepted && self.consume_action(ui.ctx(), label)
    }
    fn home_button(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        enabled: bool,
        variant: ButtonVariant,
    ) -> bool {
        self.home_button_caption(ui, label, label, enabled, variant)
    }

    fn home_button_caption(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        caption: &str,
        enabled: bool,
        variant: ButtonVariant,
    ) -> bool {
        let response = Button::new(caption)
            .sm()
            .variant(variant)
            .enabled(enabled)
            .show(ui);
        let accepted = self
            .script
            .as_mut()
            .is_none_or(|script| script.observe(label, &response));
        response.clicked() && accepted && self.consume_action(ui.ctx(), label)
    }

    fn home_configure_button(&mut self, ui: &mut egui::Ui, caption: &str, enabled: bool) -> bool {
        let label = HomeAccess::Configure.label();
        let response = ui
            .add_enabled_ui(enabled, |ui| {
                Surface::new()
                    .interactive()
                    .border_strong()
                    .pad(core::SPACE_2)
                    .id_source("home-configure")
                    .show(ui, |ui| {
                        let width = ui.available_width();
                        ui.set_width(width);
                        ui.set_height(20.0);
                        ui.with_layout(
                            egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                            |ui| {
                                Text::new(caption).label_strong().show(ui);
                            },
                        );
                    })
                    .response
            })
            .inner;
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label.to_owned())
        });
        let accepted = self
            .script
            .as_mut()
            .is_none_or(|script| script.observe(label, &response));
        response.clicked() && accepted && self.consume_action(ui.ctx(), label)
    }
    fn settings_category_control(
        &mut self,
        ui: &mut egui::Ui,
        category: SettingsCategory,
        compact: bool,
    ) {
        let label = category.label();
        let selected = self.settings_category == category;
        let response = if compact {
            Button::new(label)
                .sm()
                .variant(if selected {
                    ButtonVariant::Secondary
                } else {
                    ButtonVariant::Ghost
                })
                .show(ui)
        } else {
            MenuItem::new(label)
                .checked(selected)
                .id_source(("settings-category", label))
                .show(ui)
        };
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.to_owned())
        });
        let observer = format!("Settings category {label}");
        let accepted = self
            .script
            .as_mut()
            .is_none_or(|script| script.observe(&observer, &response));
        if response.clicked() && accepted && self.consume_action(ui.ctx(), &observer) {
            self.settings_category = category;
        }
    }
    fn settings_destinations(&mut self, ui: &mut egui::Ui) {
        Heading::new(self.settings_category.label())
            .heading()
            .show(ui);
        Divider::horizontal().show(ui);
        for &(label, page) in self.settings_category.destinations() {
            let row_width = ui.available_width();
            let response = Surface::new()
                .fill_none()
                .border_none()
                .interactive()
                .pad(core::SPACE_1)
                .radius(core::RADIUS_SM)
                .id_source(("settings-destination", label))
                .show(ui, |ui| {
                    ui.set_min_width((row_width - 2.0 * core::SPACE_1).max(0.0));
                    Text::new(label).show(ui);
                })
                .response;
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.to_owned())
            });
            let accepted = self
                .script
                .as_mut()
                .is_none_or(|script| script.observe(label, &response));
            if response.clicked() && accepted && self.consume_action(ui.ctx(), label) {
                self.go_to_page(ui.ctx(), page);
            }
        }
    }
    fn go_to_page(&mut self, ctx: &egui::Context, page: Page) {
        if self.page != page {
            if self.page == Page::InputMapper {
                self.mapper_page.leave_page();
            }
            // Navigation may leave hidden, unsubmitted editors; conservatively
            // keep them instead of installing an older asynchronous file read.
            self.profile_interaction_epoch = self.profile_interaction_epoch.saturating_add(1);
            self.initial_profile_size = None;
        }
        if self.page != page && page == Page::Output {
            self.output_page.sync_profile(&self.profile.seats);
            if let Some(user) = self.selected {
                self.output_page.select_user(user);
            }
        }
        if self.page != page && page == Page::Apps {
            self.app_info_open = false;
        }
        if self.page != page && page == Page::Agent {
            self.agent_sdk_tab = false;
        }
        self.page = page;
        if self.viewport_page != page {
            // Choose a useful starting size only on navigation; users can resize
            // freely while a page is open. Forms stay compact and canvases get room.
            let size = match page {
                Page::Home => home_window_size(self.profile.seats.len()),
                Page::User => egui::vec2(540.0, 520.0),
                Page::Settings | Page::Apps => egui::vec2(540.0, 480.0),
                Page::AppPreferences
                | Page::Users
                | Page::Profiles
                | Page::Agent
                | Page::Output => egui::vec2(680.0, 620.0),
                Page::Preview | Page::Lab | Page::Annotation | Page::Magnifier => {
                    egui::vec2(800.0, 640.0)
                }
                Page::Automation => egui::vec2(680.0, 620.0),
                Page::MacroStudio => egui::vec2(760.0, 620.0),
                Page::Replication => egui::vec2(860.0, 680.0),
                Page::Forwarding | Page::InputMapper | Page::Screen => egui::vec2(680.0, 620.0),
                Page::VirtualUsers => egui::vec2(760.0, 620.0),
            };
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
            self.viewport_page = page;
        }
    }
    fn sync_page_viewport(&mut self, ctx: &egui::Context) {
        if self.viewport_page != self.page {
            self.go_to_page(ctx, self.page);
        }
    }
    fn user_active(&self, user: usize) -> bool {
        self.virtual_users_page
            .snapshot()
            .state
            .and_then(|state| state.slots.get(user).map(|slot| slot.active))
            .unwrap_or(true)
    }

    fn reset_window_selection(&mut self) {
        self.selection_windows.clear();
        self.selection_candidate = None;
        self.selection_highlight = None;
        self.selection_pending = None;
        self.selection_error = None;
    }

    fn live_window_selection_controls(
        &mut self,
        ui: &mut egui::Ui,
        user: usize,
        window_lock: bool,
    ) {
        let virtual_user = self.profile.seats.get(user).is_some_and(|seat| {
            emulated::parse(&seat.mouse)
                .ok()
                .flatten()
                .is_some_and(|device| device.kind == DeviceKind::Mouse)
                && seat.keyboard.as_deref().is_none_or(|path| {
                    emulated::parse(path)
                        .ok()
                        .flatten()
                        .is_some_and(|device| device.kind == DeviceKind::Keyboard)
                })
        });
        if self.profile.desktop.is_none() || !virtual_user || !self.user_active(user) {
            Text::new(
                "Window selection requires an active virtual user in automatic desktop routing.",
            )
            .muted()
            .wrap()
            .show(ui);
            self.button(ui, "Select window", false);
            return;
        }
        let stopped_lock = window_lock && self.session.is_none();
        if !window_lock {
            Text::new("Choose a window for this user").show(ui);
            Text::new("Choosing a window also changes this user's mouse and keyboard target. Selection is passive until the first routed input.")
                .caption().wrap().show(ui);
            if let Some(state) = self.states.get(user) {
                Text::new(if state.status.is_empty() {
                    "No window selected"
                } else {
                    &state.status
                })
                .muted()
                .wrap()
                .show(ui);
                if state.pid != 0 {
                    Text::new(format!(
                        "Current application: {} (PID {})",
                        selection_window_title(&state.title),
                        state.pid
                    ))
                    .wrap()
                    .show(ui)
                    .on_hover_text(format!("{} (PID {})", state.title, state.pid));
                }
            }
        } else {
            let current = self
                .states
                .get(user)
                .filter(|state| self.session.is_some() && state.pid != 0);
            let stage = self.staged_window_locks.get(&self.profile, user);
            let target_summary = ui
                .vertical(|ui| {
                    let current_line = current.map_or_else(
                        || "Current application: none".to_owned(),
                        |state| format!("Current application: {} · PID {}", state.title, state.pid),
                    );
                    lock_summary_caption(ui, &current_line).on_hover_text(current.map_or_else(
                        || "No current application is selected.".to_owned(),
                        |state| format!("{} (PID {})", state.title, state.pid),
                    ));
                    if let Some(state) = self
                        .states
                        .get(user)
                        .filter(|state| self.session.is_some() && !state.status.is_empty())
                    {
                        lock_summary_caption(ui, &state.status).on_hover_text(&state.status);
                    }
                    let next_start_line = stage.map_or_else(
                        || "Next Start: no window staged".to_owned(),
                        |stage| {
                            format!("Next Start: {} · PID {}", stage.title, stage.selection.pid)
                        },
                    );
                    lock_summary_caption(ui, &next_start_line).on_hover_text(stage.map_or_else(
                        || "Window choices are transient and are not saved to profiles.".to_owned(),
                        |stage| {
                            format!(
                                "{} (PID {}) · staged for the next Start",
                                stage.title, stage.selection.pid
                            )
                        },
                    ));
                })
                .response;
            if let Some(script) = self.script.as_mut() {
                script.observe("Lock target summary", &target_summary);
            }
        }
        let discovery_current = self.discovery_current();
        let selected = self
            .selection_candidate
            .as_ref()
            .filter(|candidate| candidate.user == user)
            .and_then(|candidate| selection_snapshot_index(candidate, &self.selection_windows))
            .or_else(|| {
                self.selection_highlight
                    .filter(|(highlight_user, index)| {
                        *highlight_user == user && *index < self.selection_windows.len()
                    })
                    .map(|(_, index)| index)
            });
        let labels: Vec<String> = self
            .selection_windows
            .iter()
            .enumerate()
            .map(|(index, (window, _))| {
                format!(
                    "{} (PID {}) - window {}",
                    selection_window_title(&window.title),
                    window.pid,
                    index + 1
                )
            })
            .collect();
        let mut refresh_clicked = false;
        let mut chosen_window = None;
        if window_lock {
            ui.horizontal(|ui| {
                let refresh = Button::new("Refresh windows")
                    .icon_left(ouroboros_ui::egui_phosphor::light::ARROW_CLOCKWISE)
                    .icon_only()
                    .sm()
                    .ghost()
                    .enabled(self.selection_pending.is_none() && discovery_current)
                    .show(ui)
                    .on_hover_text("Refresh windows");
                let accepted = self
                    .script
                    .as_mut()
                    .is_none_or(|script| script.observe("Refresh windows", &refresh));
                refresh_clicked = refresh.clicked()
                    && accepted
                    && self.consume_action(ui.ctx(), "Refresh windows");
                chosen_window = self.window_chooser_menu(
                    ui,
                    selected,
                    &labels,
                    self.selection_pending.is_none() && discovery_current && !labels.is_empty(),
                );
            });
        } else {
            refresh_clicked = self.button(
                ui,
                "Refresh windows",
                self.selection_pending.is_none() && discovery_current,
            );
            chosen_window = self.window_chooser_menu(
                ui,
                selected,
                &labels,
                self.selection_pending.is_none() && discovery_current && !labels.is_empty(),
            );
        }
        if refresh_clicked {
            self.discovery_picker_revision = self.discovery_picker_revision.saturating_add(1);
            let mut allowed_pids = self
                .profile
                .desktop
                .as_ref()
                .expect("desktop checked")
                .allowed_pids
                .clone();
            allowed_pids.sort_unstable();
            allowed_pids.dedup();
            self.discovery_picker = Some((
                crate::gui_discovery::PickerScope {
                    user,
                    revision: self.discovery_picker_revision,
                    allowed_pids,
                },
                self.profile.seats[user].identity(),
            ));
            self.selection_highlight = None;
            self.refresh();
        }
        if !self.discovery_current() {
            Text::new("Refreshing applications… Selection will be available when the current check finishes.")
                .caption().wrap().show(ui);
        }
        // Preserve full logical labels for identity and scripted observations;
        // only the dedicated popup's visual title prefix is pixel-truncated.
        if let Some(index) = chosen_window
            && let Some((window, readiness)) = self.selection_windows.get(index)
        {
            self.selection_highlight = Some((user, index));
            self.selection_candidate = readiness
                .creation_token
                .as_ref()
                .filter(|token| targets::parse_creation_token(token).is_ok())
                .map(|creation_token| crate::window_selection::Selection {
                    request_id: String::new(),
                    user,
                    pid: window.pid,
                    hwnd: window.hwnd,
                    creation_token: creation_token.clone(),
                });
            self.selection_error = None;
        }
        let selected = self
            .selection_highlight
            .filter(|(highlight_user, index)| {
                *highlight_user == user && *index < self.selection_windows.len()
            })
            .map(|(_, index)| index)
            .or(selected);
        if let Some(label) = selected.and_then(|index| labels.get(index)) {
            if window_lock {
                let response =
                    lock_summary_caption(ui, &format!("Candidate: {label}")).on_hover_text(label);
                if let Some(script) = self.script.as_mut() {
                    script.observe("Lock candidate summary", &response);
                }
            } else {
                Text::new(format!("Highlighted: {label}"))
                    .wrap()
                    .show(ui)
                    .on_hover_text(label);
            }
        } else if window_lock {
            let response = lock_summary_caption(ui, "Candidate: no window highlighted");
            if let Some(script) = self.script.as_mut() {
                script.observe("Lock candidate summary", &response);
            }
        }
        if let Some((_, readiness)) = selected.and_then(|index| self.selection_windows.get(index))
            && (!window_lock || !readiness.can_start(true))
        {
            Text::new(&readiness.reason).caption().wrap().show(ui);
        }
        if window_lock {
            let selected_window = selected.and_then(|index| self.selection_windows.get(index));
            Collapsible::new("Selection details")
                .id_source(("lock-selection-details", self.profile.seats[user].identity()))
                .default_open(false)
                .show(ui, |ui| {
                    ui.weak("Selection is passive until routed input. Staged choices are checked again before Start; they are not saved to profiles.");
                    if let Some(stage) = self.staged_window_locks.get(&self.profile, user) {
                        ui.label(format!("Next Start identity: PID {} · HWND 0x{:X}", stage.selection.pid, stage.selection.hwnd));
                    }
                    if let Some((window, readiness)) = selected_window {
                        ui.label(format!("Candidate: {} · PID {} · HWND 0x{:X}", window.title, window.pid, window.hwnd));
                        ui.label(format!("Client bounds: {:?}", targets::client_bounds(window.hwnd, window.pid).ok()));
                        ui.label(format!("Eligibility: {:?} · {}", readiness.state, readiness.reason));
                    }
                    if let Some(state) = self.states.get(user) {
                        ui.label(format!(
                            "Target status: {}",
                            if state.status.is_empty() { "No window selected" } else { state.status.as_str() }
                        ));
                        if state.pid != 0 {
                            ui.label(format!("Current client bounds: {:?}", state.bounds));
                            ui.label(format!("Effective movement bounds: {:?}", state.movement_bounds));
                        }
                    }
                });
        }
        let independent = self
            .profile
            .desktop
            .as_ref()
            .is_some_and(|desktop| desktop.multi_keyboard);
        let ready = (stopped_lock
            || self.session_ready
                && !self.session_busy
                && !self.session_paused
                && !self.session_stopping)
            && independent
            && self.selection_pending.is_none()
            && self.discovery_current();
        let candidate = self
            .selection_candidate
            .as_ref()
            .filter(|candidate| {
                candidate.user == user
                    && selection_snapshot_index(candidate, &self.selection_windows) == selected
                    && selected.is_some()
            })
            .cloned();
        // Busy may be this session's own attachment; the worker checks ownership.
        let candidate_ready = selected
            .and_then(|index| self.selection_windows.get(index))
            .is_some_and(|(_, readiness)| {
                readiness.can_start(true) || readiness.state == targets::EligibilityState::Busy
            });
        let select_enabled = ready && candidate.is_some() && candidate_ready;
        let select_label = if stopped_lock {
            "Stage window for Start"
        } else {
            "Select window"
        };
        let mut select_clicked = false;
        let mut clear_staged_clicked = false;
        if window_lock {
            ui.horizontal_wrapped(|ui| {
                select_clicked = self.button(ui, select_label, select_enabled);
                if self.staged_window_locks.get(&self.profile, user).is_some() {
                    clear_staged_clicked = self.button(ui, "Clear staged window", stopped_lock);
                }
            });
        } else {
            select_clicked = self.button(ui, select_label, select_enabled);
        }
        if clear_staged_clicked {
            self.staged_window_locks.clear_user(&self.profile, user);
        }
        if select_clicked && let Some(mut candidate) = candidate {
            if stopped_lock {
                let title = selected
                    .and_then(|index| self.selection_windows.get(index))
                    .map(|(window, _)| window.title.clone())
                    .unwrap_or_default();
                match self
                    .staged_window_locks
                    .stage(&self.profile, user, candidate, title)
                {
                    Ok(()) => {
                        self.selection_error = None;
                        self.status = "Window staged for this user's next Start.".into();
                    }
                    Err(error) => self.selection_error = Some((user, format!("{error:#}"))),
                }
            } else if let Some(session) = &self.session {
                candidate.issue();
                session.request(Request::SelectWindow(candidate.clone()));
                self.selection_pending = Some(candidate);
                self.selection_error = None;
                self.session_busy = true;
            }
        }
        let disabled_reason = if !independent {
            Some("Enable independent keyboards before selecting a window.")
        } else if self.selection_pending.is_some() {
            Some("Waiting for window selection acknowledgement.")
        } else if !stopped_lock && (!self.session_ready || self.session_stopping) {
            Some("Wait for routing to be ready before selecting a window.")
        } else if self.session_paused {
            Some("Resume routing before selecting a window.")
        } else if self.session_busy {
            Some("Wait for the current routing action to finish.")
        } else if !select_enabled {
            Some("Refresh and choose an available application before selecting a window.")
        } else {
            None
        };
        if let Some(reason) = disabled_reason {
            Text::new(reason).caption().wrap().show(ui);
        }
        if let Some((error_user, error)) = &self.selection_error
            && *error_user == user
        {
            Text::new(error).caption().wrap().show(ui);
        }
        if let Some(script) = &self.script {
            script.window_selection_state(&serde_json::json!({
                "user": user,
                "candidate": self.selection_candidate,
                "pending": self.selection_pending,
                "error": self.selection_error,
                "status": self.states.get(user).map(|state| &state.status),
                "current_target": self.states.get(user).map(|state| serde_json::json!({
                    "pid": state.pid, "title": state.title, "status": state.status,
                })),
                "select_enabled": select_enabled && self.selection_pending.is_none(),
                "disabled_reason": disabled_reason,
                "independent_keyboards": independent,
                "windows": self.selection_windows.iter().zip(&labels).map(|((window, readiness), label)| serde_json::json!({
                    "pid": window.pid, "hwnd": window.hwnd, "label": label,
                    "title": selection_window_title(&window.title), "eligibility": readiness,
                })).collect::<Vec<_>>(),
            }));
        }
    }
    fn window_chooser_menu(
        &mut self,
        ui: &mut egui::Ui,
        selected: Option<usize>,
        labels: &[String],
        enabled: bool,
    ) -> Option<usize> {
        let mut chosen = None;
        // The trigger never incorporates an application's unbounded identity.
        // Button uses Size's own typography and padding tokens.
        let popup_width = ui
            .available_width()
            .min(ui.ctx().content_rect().width() - 2. * core::SPACE_4)
            .max(0.);
        ui.add_enabled_ui(enabled, |ui| {
            let trigger = Button::new("Window chooser")
                .size(core::Size::Md)
                .icon_right(ouroboros_ui::egui_phosphor::light::CARET_DOWN)
                .variant(ButtonVariant::Outline)
                .show(ui);
            if let Some(script) = self.script.as_mut() {
                script.observe("Window chooser", &trigger);
            }
            let ctx = ui.ctx().clone();
            egui::Popup::menu(&trigger).width(popup_width).show(|menu| {
                menu.set_max_width(popup_width);
                // MenuItem's surface padding and checked-icon slot are fixed
                // Ouroboros tokens; retain room for horizontal layout spacing.
                let row_insets = 2. * core::SPACE_1
                    + core::ICON_MD
                    + core::SPACE_2
                    + 3. * menu.spacing().item_spacing.x;
                let text_width = menu.available_width().min(popup_width) - row_insets;
                for (index, label) in labels.iter().enumerate() {
                    let display = window_chooser_display_label(menu, label, text_width);
                    let option = MenuItem::new(display)
                        .checked(selected == Some(index))
                        .id_source(("Window chooser", index))
                        .show(menu)
                        .on_hover_text(label);
                    if let Some(script) = self.script.as_mut() {
                        script.observe(label, &option);
                    }
                    if option.clicked() && self.consume_action(&ctx, label) {
                        chosen = Some(index);
                        menu.close();
                    }
                }
            });
        });
        chosen
    }
    fn extended_overlay_available(&self) -> bool {
        self.profile.desktop.is_some() || self.profile.concurrent
    }
    fn consume_action(&mut self, ctx: &egui::Context, label: &str) -> bool {
        let frame = ctx.cumulative_frame_nr();
        if self.action_frame != frame {
            self.action_frame = frame;
            self.frame_actions.clear();
        }
        // egui can repeat the layout pass with the same click. A product action
        // must be dispatched once, even if an acknowledgement arrives mid-frame.
        self.frame_actions.insert(label.to_owned())
    }
    fn choice_menu(
        &mut self,
        ui: &mut egui::Ui,
        control: &str,
        selected: &str,
        options: &[&str],
        enabled: bool,
    ) -> Option<usize> {
        let mut chosen = None;
        ui.add_enabled_ui(enabled, |ui| {
            let trigger = Button::new(format!("{control}: {selected}"))
                .icon_right(ouroboros_ui::egui_phosphor::light::CARET_DOWN)
                .variant(ButtonVariant::Outline)
                .show(ui);
            if let Some(script) = self.script.as_mut() {
                script.observe(control, &trigger);
            }
            let ctx = ui.ctx().clone();
            egui::Popup::menu(&trigger).show(|menu| {
                for (index, label) in options.iter().enumerate() {
                    let option = MenuItem::new(*label)
                        .checked(*label == selected)
                        .id_source((control, index))
                        .show(menu);
                    if let Some(script) = self.script.as_mut() {
                        script.observe(label, &option);
                    }
                    if option.clicked() && self.consume_action(&ctx, label) {
                        chosen = Some(index);
                        menu.close();
                    }
                }
            });
        });
        chosen
    }
    fn merge_virtual_users_roster(&mut self, state: &crate::virtual_users::State) {
        if self
            .virtual_users_page
            .snapshot()
            .state
            .is_some_and(|current| current.revision > state.revision)
        {
            return;
        }
        let base_count = *self
            .virtual_users_base_seat_count
            .get_or_insert(self.profile.seats.len());
        if let Err(error) = merge_runtime_roster(&mut self.profile, state, base_count) {
            self.status = format!("Virtual user state could not be applied: {error:#}");
            return;
        }
        self.replication_edges.retain(|edge| {
            state.slots.get(edge.source).is_some_and(|slot| slot.active)
                && state.slots.get(edge.target).is_some_and(|slot| slot.active)
        });
        self.replication_page.refresh_seats(&self.profile.seats);
        self.replication_page
            .sync_applied(&self.profile.replication);
    }
    fn reset_virtual_users_session(&mut self) {
        if let Some(base_count) = self.virtual_users_base_seat_count.take() {
            let original_ids: BTreeSet<String> = self
                .profile
                .seats
                .iter()
                .take(base_count)
                .map(Seat::identity)
                .collect();
            self.profile.seats.truncate(base_count);
            self.profile.replication.edges.retain(|edge| {
                original_ids.contains(&edge.source) && original_ids.contains(&edge.target)
            });
            self.profile.forwarding.edges.retain(|edge| {
                original_ids.contains(&edge.source) && original_ids.contains(&edge.target)
            });
            self.replication_edges
                .retain(|edge| edge.source < base_count && edge.target < base_count);
            self.replication_page.reset_profile(&self.profile);
            self.forwarding_page.reset_profile(&self.profile);
        }
        self.virtual_users_page.reset_session();
    }
    fn advance_setup_revision(&mut self) {
        self.setup_handle.cancel_pending();
        self.setup_revision = self
            .setup_revision
            .checked_add(1)
            .expect("setup revision exhausted");
    }

    fn reset_profile_editors(&mut self) {
        self.advance_setup_revision();
        self.profile_install_epoch = self.profile_install_epoch.saturating_add(1);
        self.profile_file.invalidate();
        self.invalidate_macro_file();
        self.staged_window_locks.clear();
        self.startup_window_selections.clear();
        self.startup_window_selection_active = false;
        self.startup_window_selection_error = None;
        self.reset_window_selection();
        self.states.clear();
        self.user_pause.reset();
        self.pointers.clear();
        self.keyboard_packets.clear();
        self.cursor_settings.reset(&[]);
        self.lab_positions.clear();
        self.macro_clip = None;
        self.macro_recording = false;
        self.macro_recorded_events = 0;
        self.macro_loaded_events = 0;
        self.macro_playback = None;
        self.macro_error = None;
        self.edge_source = 0;
        self.edge_target = 1;
        self.replication_page.reset_profile(&self.profile);
        self.forwarding_page.reset_profile(&self.profile);
        self.mapper_commands.clear();
        self.mapper_owns_session_busy = false;
        self.mapper_page.reset_profile(&self.profile.mapper);
        self.screen_page.reset_session();
        self.screen_targets.clear();
        self.replication_edges = self
            .profile
            .replication
            .resolve(&self.profile.seats)
            .unwrap_or_default();
        self.replication_dirty = false;
        self.replication_revision = 0;
        self.annotation_page = crate::annotation_ui::AnnotationPage::default();
        self.output_page = crate::output_ui::OutputPage::default();
        self.magnifier_page.reset_for_profile();
        self.macro_studio_page = crate::macro_studio_ui::MacroStudioPage::default();
        self.virtual_users_page = crate::virtual_users_ui::VirtualUsersPage::default();
        self.agent_page = crate::agent_ui::AgentPage::default();
        self.virtual_users_base_seat_count = None;
        self.selected = None;
        self.region_draft = mux_core::desktop::Bounds::default();
        self.region_draft_user = None;
    }
    fn sync_keyboard_settings(&mut self, states: &[crate::router::State]) {
        for (index, state) in states.iter().enumerate() {
            let Some(live) = state.keyboard.as_ref() else {
                continue;
            };
            let Some(seat) = self.profile.seats.get_mut(index) else {
                continue;
            };
            self.cursor_settings.observe_keyboard(seat, live.settings);
        }
    }
    fn install_profile(&mut self, profile: Profile) {
        let prior = std::mem::replace(&mut self.profile, profile);
        self.profile_undo = Some(prior);
        self.reset_profile_editors();
    }
    fn poll_agent_broker(&mut self) {
        use crate::gui_agent_broker::{GuiSession, GuiTransition, Phase, apply_gui_completion};
        let Some(completion) = self.agent_broker.take_completion() else {
            let snapshot = self.agent_broker.status();
            match snapshot.phase {
                Phase::CleanupFailed(error) => {
                    self.restart_requested = false;
                    self.status = if self.setup_retiring {
                        self.setup_page.error =
                            Some(format!("Setup broker cleanup is incomplete: {error}"));
                        format!("Stopped setup cleanup is incomplete: {error}")
                    } else {
                        format!("Agent Access cleanup is incomplete: {error}")
                    };
                }
                Phase::WorkerFailed(error) => {
                    self.restart_requested = false;
                    self.status = format!("Agent Access lifecycle worker failed: {error}");
                }
                Phase::Stopped if !snapshot.worker_alive => {
                    self.status = "Agent Access lifecycle worker has stopped.".into();
                }
                _ => {}
            }
            return;
        };
        if let Some((generation, _)) = self.setup_pending
            && completion.generation == generation
        {
            match &completion.kind {
                crate::gui_agent_broker::CompletionKind::Started(path) => {
                    self.setup_pending = None;
                    self.setup_page.pending = false;
                    if self.setup_handle.enabled()
                        && self.session.is_none()
                        && self
                            .agent_broker
                            .is_current(generation, completion.session_epoch)
                    {
                        self.setup_page.descriptor = crate::agent_ui::exact_path_string(path);
                        self.status = "Stopped setup connection ready.".into();
                    } else {
                        self.setup_handle.revoke();
                        self.agent_broker.request_stop(self.agent_session_epoch);
                    }
                    return;
                }
                crate::gui_agent_broker::CompletionKind::StartFailed(error)
                | crate::gui_agent_broker::CompletionKind::WorkerFailed(error) => {
                    self.setup_pending = None;
                    self.setup_page.pending = false;
                    self.setup_page.error = Some(error.clone());
                    self.setup_handle.revoke();
                    return;
                }
                crate::gui_agent_broker::CompletionKind::CleanupFailed(error) => {
                    self.setup_page.error =
                        Some(format!("Setup broker cleanup is incomplete: {error}"));
                    return;
                }
                crate::gui_agent_broker::CompletionKind::Stopped => {
                    self.setup_pending = None;
                    self.setup_page.pending = false;
                    self.setup_page.descriptor = None;
                    return;
                }
            }
        }
        if self.setup_retiring {
            match &completion.kind {
                crate::gui_agent_broker::CompletionKind::Started(_) => {
                    self.agent_broker.request_stop(self.agent_session_epoch);
                }
                crate::gui_agent_broker::CompletionKind::CleanupFailed(error) => {
                    self.setup_page.error =
                        Some(format!("Setup broker cleanup is incomplete: {error}"));
                    self.status = format!("Stopped setup cleanup is incomplete: {error}");
                    self.restart_requested = false;
                }
                crate::gui_agent_broker::CompletionKind::WorkerFailed(error) => {
                    self.setup_page.error = Some(format!("Setup broker worker failed: {error}"));
                    self.status = format!("Stopped setup worker failed: {error}");
                    self.restart_requested = false;
                }
                crate::gui_agent_broker::CompletionKind::Stopped
                | crate::gui_agent_broker::CompletionKind::StartFailed(_) => {
                    if matches!(
                        &completion.kind,
                        crate::gui_agent_broker::CompletionKind::Stopped
                    ) {
                        self.setup_retired_generation = Some(completion.generation);
                    }
                    self.setup_retiring = false;
                    self.setup_page.descriptor = None;
                    self.status = "Stopped setup broker cleanup is complete.".into();
                }
            }
            return;
        }
        match apply_gui_completion(
            &mut self.agent_page,
            &mut self.agent_pending_enable,
            completion,
            &self.agent_broker,
            GuiSession {
                epoch: self.agent_session_epoch,
                live: self.session.is_some(),
                stopping: self.session_stopping,
            },
        ) {
            GuiTransition::Configure(path, command) => {
                self.agent_descriptor_path =
                    crate::agent_ui::exact_path_string(&path).unwrap_or_default();
                if let Some(session) = &self.session {
                    self.app_request_handle.prepare_policy();
                    session.request(Request::Agent(command));
                    self.session_busy = true;
                    self.status = "Agent Access is starting.".into();
                }
            }
            GuiTransition::Retire => {
                self.agent_broker.request_stop(self.agent_session_epoch);
            }
            GuiTransition::Status(status) => {
                if matches!(
                    self.agent_broker.status().phase,
                    Phase::CleanupFailed(_) | Phase::WorkerFailed(_)
                ) {
                    self.restart_requested = false;
                }
                self.status = status;
            }
            GuiTransition::Stopped => {
                self.agent_descriptor_path.clear();
                self.setup_page.descriptor = None;
                if self.quit_after_stop
                    && crate::gui_agent_broker::quit_ready(
                        self.session.is_none(),
                        &self.agent_broker.status().phase,
                    )
                {
                    self.close_ready = self.all_sdks_retired();
                } else if self.session.is_none() {
                    self.status = "Routing stopped; Agent Access cleanup is complete.".into();
                } else if !self.agent_page.snapshot().policy_enabled {
                    self.status = "Agent Access is disabled; broker cleanup is complete.".into();
                }
            }
            GuiTransition::Unchanged => {}
        }
    }
    fn seed_region_draft(&mut self, user: usize) {
        if self.region_draft_user == Some(user) || user >= self.profile.seats.len() {
            return;
        }
        let existing = self.profile.seats[user].cursor.region;
        let current_window = self
            .states
            .get(user)
            .filter(|state| state.pid != 0 && state.bounds.valid())
            .map(|state| state.bounds)
            .or_else(|| {
                let pid = self.profile.seats[user].target_pid;
                self.targets
                    .iter()
                    .find(|target| target.pid == pid)
                    .and_then(|target| targets::client_bounds(target.hwnd, target.pid).ok())
            });
        self.region_draft = existing
            .or(current_window)
            .unwrap_or_else(mux_windows::targets::desktop_bounds);
        self.region_draft_user = Some(user);
    }
    fn cursor_settings_error(
        &self,
        user: usize,
        settings: &mux_core::desktop::CursorSettings,
    ) -> Option<String> {
        if settings.requires_extended_overlay()
            && self.profile.desktop.is_none()
            && !self.profile.concurrent
        {
            return Some(
                "Extended cursor overlay settings require automatic desktop routing or concurrent assigned routing. The saved settings are retained.".into(),
            );
        }
        if settings.horizontal_wheel_emulation != mux_core::desktop::HorizontalWheelEmulation::Never
        {
            let virtual_mouse = self.profile.seats.get(user).is_some_and(|seat| {
                emulated::parse(&seat.mouse)
                    .ok()
                    .flatten()
                    .is_some_and(|device| device.kind == DeviceKind::Mouse)
            });
            if !virtual_mouse {
                return Some(
                    "Vertical-to-horizontal emulation requires an assigned virtual mouse. The saved setting is retained.".into(),
                );
            }
            if self.profile.desktop.is_none() && !self.profile.concurrent {
                return Some(
                    "Vertical-to-horizontal emulation requires automatic desktop routing or concurrent assigned routing.".into(),
                );
            }
        }
        if settings.horizontal_wheel == mux_core::desktop::HorizontalWheelPolicy::Preserve
            && self.profile.desktop.is_none()
            && !self.profile.concurrent
        {
            return Some("Preserving horizontal scroll requires automatic targeting or concurrent assigned routing with desktop cursors.".into());
        }
        if let Err(error) = settings.validate() {
            return Some(error.to_string());
        }
        let monitor = if let Some(monitor_id) = settings.monitor_id.as_deref().filter(|_| {
            matches!(
                settings.bounds_mode,
                mux_core::desktop::BoundsMode::Monitor | mux_core::desktop::BoundsMode::Region
            )
        }) {
            let Some(monitor) = self
                .monitors
                .iter()
                .find(|monitor| monitor.id == monitor_id)
            else {
                return Some(
                    "The selected monitor is disconnected. Choose a connected display.".into(),
                );
            };
            if !monitor.stable_id {
                return Some("The selected display has no stable monitor path. Choose a display with a verified interface path.".into());
            }
            Some(if settings.monitor_work_area {
                monitor.work_area
            } else {
                monitor.bounds
            })
        } else {
            None
        };
        if matches!(settings.bounds_mode, mux_core::desktop::BoundsMode::Monitor)
            && monitor.is_none()
        {
            if let Some(error) = &self.monitor_error {
                return Some(format!("Monitor inventory unavailable: {error}"));
            }
            return Some("Choose a connected monitor with a stable path.".into());
        }
        if matches!(settings.bounds_mode, mux_core::desktop::BoundsMode::Region)
            && settings.monitor_id.is_some()
            && monitor.is_none()
        {
            return Some("The selected region monitor is unavailable.".into());
        }
        let window = self
            .states
            .get(user)
            .filter(|state| state.pid != 0 && state.bounds.valid())
            .map(|state| state.bounds);
        cursor_draft_bounds_error(settings, self.desktop_bounds, window, monitor)
    }
    fn apply_script_fields(&mut self) {
        if self.quit_after_stop || self.preferences_quit_waiting || self.allow_close {
            return;
        }
        let Some(fields) = self
            .script
            .as_ref()
            .and_then(|script| script.field_edits().cloned())
        else {
            return;
        };
        if !fields.is_empty() {
            self.advance_setup_revision();
        }
        let replication_edit = fields.keys().any(|key| key.starts_with("replication_"));
        self.annotation_page.apply_script_fields(&fields);
        self.output_page.apply_script_fields(&fields);
        self.magnifier_page.apply_script_fields(&fields);
        self.screen_page.apply_script_fields(&fields);
        self.macro_studio_page.apply_script_fields(&fields);
        self.virtual_users_page.apply_script_fields(&fields);
        self.agent_page
            .apply_script_fields(&fields, &self.profile.seats);
        self.app_preferences_page.apply_script_fields(&fields);
        let event_origin_editable = self.application_events.origin_editable();
        self.application_events
            .draft
            .apply_script_fields(&fields, event_origin_editable);
        if self.sdk_retired()
            && let Some(origins) = fields.get("sdk_origins")
        {
            self.sdk_origins.clone_from(origins);
        }
        self.replication_page
            .apply_script_fields(&fields, &self.profile.seats);
        for (name, value) in fields {
            match name.as_str() {
                "key_name" => self.key_name = value,
                "text_input" => self.text_input = value,
                "macro_path" => self.macro_path = value,
                "profile_path" => self.profile_path = value,
                "profile_transfer_path" => self.profile_transfer_path = value,
                "seat_name" if self.session.is_none() => {
                    if let Some(seat) = self
                        .selected
                        .and_then(|index| self.profile.seats.get_mut(index))
                    {
                        seat.name = value;
                    }
                }
                "seat_mouse" if self.session.is_none() => {
                    if let Some(seat) = self
                        .selected
                        .and_then(|index| self.profile.seats.get_mut(index))
                    {
                        seat.mouse = value;
                    }
                }
                "seat_keyboard" if self.session.is_none() => {
                    if let Some(seat) = self
                        .selected
                        .and_then(|index| self.profile.seats.get_mut(index))
                    {
                        seat.keyboard = (!value.is_empty()).then_some(value);
                    }
                }
                "macro_loops" => {
                    if let Ok(loops) = value.parse::<u32>() {
                        self.macro_loops = loops.clamp(1, 1_000_000);
                    }
                }
                "macro_speed" => {
                    if let Ok(speed) = value.parse::<f64>()
                        && speed.is_finite()
                    {
                        self.macro_speed = speed.clamp(0.1, 4.0);
                    }
                }
                "motion_curve" => {
                    if let Ok(curve) =
                        serde_json::from_str::<mux_core::desktop::MotionCurve>(&value)
                    {
                        if let Some(index) = self.selected.filter(|i| *i < self.profile.seats.len())
                        {
                            self.profile.seats[index].cursor.motion_curve = curve;
                        }
                    } else if let Some(index) =
                        self.selected.filter(|i| *i < self.profile.seats.len())
                    {
                        let curve = match value.to_ascii_lowercase().as_str() {
                            "linear" => Some(mux_core::desktop::MotionCurve::Linear),
                            "gentle" => Some(mux_core::desktop::MotionCurve::Gentle),
                            "strong" => Some(mux_core::desktop::MotionCurve::Strong),
                            _ => None,
                        };
                        if let Some(curve) = curve {
                            self.profile.seats[index].cursor.motion_curve = curve;
                        }
                    }
                }
                "shape" => {
                    let shape = serde_json::from_str::<mux_core::desktop::CursorShape>(&value)
                        .ok()
                        .or_else(|| match value.to_ascii_lowercase().as_str() {
                            "arrow" => Some(mux_core::desktop::CursorShape::Arrow),
                            "crosshair" => Some(mux_core::desktop::CursorShape::Crosshair),
                            "i_beam" | "ibeam" => Some(mux_core::desktop::CursorShape::IBeam),
                            "ring" => Some(mux_core::desktop::CursorShape::Ring),
                            _ => None,
                        });
                    if let (Some(shape), Some(index)) = (
                        shape,
                        self.selected.filter(|i| *i < self.profile.seats.len()),
                    ) {
                        self.profile.seats[index].cursor.shape = shape;
                    }
                }
                "label_position" => {
                    let position = match value.to_ascii_lowercase().as_str() {
                        "right" => Some(mux_core::desktop::LabelPosition::Right),
                        "left" => Some(mux_core::desktop::LabelPosition::Left),
                        "above" => Some(mux_core::desktop::LabelPosition::Above),
                        "below" => Some(mux_core::desktop::LabelPosition::Below),
                        _ => None,
                    };
                    if let (Some(position), Some(index)) = (
                        position,
                        self.selected.filter(|i| *i < self.profile.seats.len()),
                    ) && self.extended_overlay_available()
                    {
                        self.profile.seats[index].cursor.label_position = position;
                    }
                }
                "label_font_size" => {
                    if let Ok(size) = value.parse::<f32>()
                        && size.is_finite()
                        && let Some(index) = self.selected.filter(|i| *i < self.profile.seats.len())
                        && self.extended_overlay_available()
                    {
                        self.profile.seats[index].cursor.label_font_size = size.clamp(8., 48.);
                    }
                }
                "badge_shape" => {
                    let shape = match value.to_ascii_lowercase().as_str() {
                        "rounded" => Some(mux_core::desktop::BadgeShape::Rounded),
                        "box" => Some(mux_core::desktop::BadgeShape::Box),
                        "circle" => Some(mux_core::desktop::BadgeShape::Circle),
                        _ => None,
                    };
                    if let (Some(shape), Some(index)) = (
                        shape,
                        self.selected.filter(|i| *i < self.profile.seats.len()),
                    ) && self.extended_overlay_available()
                    {
                        self.profile.seats[index].cursor.badge_shape = shape;
                    }
                }
                "badge_position" => {
                    let position = match value.to_ascii_lowercase().as_str() {
                        "inline" => Some(mux_core::desktop::BadgePosition::Inline),
                        "top_left" => Some(mux_core::desktop::BadgePosition::TopLeft),
                        "top_right" => Some(mux_core::desktop::BadgePosition::TopRight),
                        "bottom_left" => Some(mux_core::desktop::BadgePosition::BottomLeft),
                        "bottom_right" => Some(mux_core::desktop::BadgePosition::BottomRight),
                        _ => None,
                    };
                    if let (Some(position), Some(index)) = (
                        position,
                        self.selected.filter(|i| *i < self.profile.seats.len()),
                    ) && self.extended_overlay_available()
                    {
                        self.profile.seats[index].cursor.badge_position = position;
                    }
                }
                "idle_hide_ms" => {
                    let timeout = if matches!(value.to_ascii_lowercase().as_str(), "none" | "off") {
                        Some(None)
                    } else {
                        value
                            .parse::<u64>()
                            .ok()
                            .filter(|ms| (250..=600_000).contains(ms))
                            .map(Some)
                    };
                    if let (Some(timeout), Some(index)) = (
                        timeout,
                        self.selected.filter(|i| *i < self.profile.seats.len()),
                    ) && self.extended_overlay_available()
                    {
                        self.profile.seats[index].cursor.idle_hide_ms = timeout;
                    }
                }
                "badge" => {
                    if let Some(index) = self.selected.filter(|i| *i < self.profile.seats.len()) {
                        self.profile.seats[index].cursor.badge =
                            (!value.is_empty()).then_some(value);
                    }
                }
                "visible" | "label" => {
                    if let Ok(enabled) = value.parse::<bool>()
                        && let Some(index) = self.selected.filter(|i| *i < self.profile.seats.len())
                    {
                        if name == "visible" {
                            self.profile.seats[index].cursor.visible = enabled;
                        } else {
                            self.profile.seats[index].cursor.label = enabled;
                        }
                    }
                }
                "size" => {
                    if let Ok(size) = value.parse::<f32>()
                        && size.is_finite()
                        && (size <= 64. || self.extended_overlay_available())
                        && let Some(index) = self.selected.filter(|i| *i < self.profile.seats.len())
                    {
                        self.profile.seats[index].cursor.size = size.clamp(12., 256.);
                    }
                }
                "bounds_mode" => {
                    let mode = match value.to_ascii_lowercase().as_str() {
                        "legacy" => Some(mux_core::desktop::BoundsMode::Legacy),
                        "desktop" => Some(mux_core::desktop::BoundsMode::Desktop),
                        "window" => Some(mux_core::desktop::BoundsMode::Window),
                        "monitor" => Some(mux_core::desktop::BoundsMode::Monitor),
                        "region" => Some(mux_core::desktop::BoundsMode::Region),
                        _ => None,
                    };
                    if let (Some(mode), Some(index)) = (
                        mode,
                        self.selected.filter(|i| *i < self.profile.seats.len()),
                    ) {
                        if mode == mux_core::desktop::BoundsMode::Monitor
                            && self.profile.seats[index].cursor.monitor_id.is_none()
                        {
                            self.profile.seats[index].cursor.monitor_id = self
                                .monitors
                                .iter()
                                .find(|monitor| monitor.primary && monitor.stable_id)
                                .map(|monitor| monitor.id.clone());
                        }
                        self.profile.seats[index].cursor.bounds_mode = mode;
                        if mode == mux_core::desktop::BoundsMode::Region {
                            self.seed_region_draft(index);
                            if self.profile.seats[index].cursor.region.is_none() {
                                self.profile.seats[index].cursor.region = Some(self.region_draft);
                            }
                        }
                    }
                }
                "monitor_id" => {
                    if let Some(index) = self.selected.filter(|i| *i < self.profile.seats.len()) {
                        self.profile.seats[index].cursor.monitor_id =
                            (!value.is_empty() && value != "none").then_some(value);
                    }
                }
                "monitor_work_area" => {
                    if let Ok(enabled) = value.parse::<bool>()
                        && let Some(index) = self.selected.filter(|i| *i < self.profile.seats.len())
                    {
                        self.profile.seats[index].cursor.monitor_work_area = enabled;
                    }
                }
                "region" => {
                    if let Ok(region) = serde_json::from_str::<mux_core::desktop::Bounds>(&value)
                        && let Some(index) = self.selected.filter(|i| *i < self.profile.seats.len())
                    {
                        self.profile.seats[index].cursor.region = Some(region);
                        self.region_draft = region;
                        self.region_draft_user = Some(index);
                    }
                }
                "region_left" | "region_top" | "region_right" | "region_bottom" => {
                    if let Ok(coordinate) = value.parse::<i32>()
                        && let Some(index) = self.selected.filter(|i| *i < self.profile.seats.len())
                    {
                        self.seed_region_draft(index);
                        match name.as_str() {
                            "region_left" => {
                                self.region_draft.left = coordinate.clamp(-1_000_000, 1_000_000)
                            }
                            "region_top" => {
                                self.region_draft.top = coordinate.clamp(-1_000_000, 1_000_000)
                            }
                            "region_right" => {
                                self.region_draft.right = coordinate.clamp(-1_000_000, 1_000_000)
                            }
                            "region_bottom" => {
                                self.region_draft.bottom = coordinate.clamp(-1_000_000, 1_000_000)
                            }
                            _ => unreachable!(),
                        }
                        self.profile.seats[index].cursor.region = Some(self.region_draft);
                    }
                }
                "button_mapping" => {
                    if let Ok(mapping) =
                        serde_json::from_str::<[Option<mux_core::desktop::MouseButton>; 5]>(&value)
                        && let Some(index) = self.selected.filter(|i| *i < self.profile.seats.len())
                    {
                        self.profile.seats[index].cursor.button_mapping = mapping;
                    }
                }
                "horizontal_wheel" => {
                    if let Some(index) = self.selected.filter(|i| *i < self.profile.seats.len()) {
                        self.profile.seats[index].cursor.horizontal_wheel =
                            match value.to_ascii_lowercase().as_str() {
                                "preserve" => mux_core::desktop::HorizontalWheelPolicy::Preserve,
                                "convert_to_vertical" | "convert" => {
                                    mux_core::desktop::HorizontalWheelPolicy::ConvertToVertical
                                }
                                _ => mux_core::desktop::HorizontalWheelPolicy::Ignore,
                            };
                    }
                }
                "horizontal_wheel_emulation" => {
                    let emulation = match value.to_ascii_lowercase().as_str() {
                        "never" => Some(mux_core::desktop::HorizontalWheelEmulation::Never),
                        "always" => Some(mux_core::desktop::HorizontalWheelEmulation::Always),
                        "while_primary" | "while_primary_down" => {
                            Some(mux_core::desktop::HorizontalWheelEmulation::WhilePrimary)
                        }
                        "while_secondary" | "while_secondary_down" => {
                            Some(mux_core::desktop::HorizontalWheelEmulation::WhileSecondary)
                        }
                        "while_middle" | "while_middle_down" => {
                            Some(mux_core::desktop::HorizontalWheelEmulation::WhileMiddle)
                        }
                        "while_back" | "while_back_down" => {
                            Some(mux_core::desktop::HorizontalWheelEmulation::WhileBack)
                        }
                        "while_forward" | "while_forward_down" => {
                            Some(mux_core::desktop::HorizontalWheelEmulation::WhileForward)
                        }
                        _ => None,
                    };
                    if let (Some(emulation), Some(index)) = (
                        emulation,
                        self.selected.filter(|i| *i < self.profile.seats.len()),
                    ) {
                        let seat = &self.profile.seats[index];
                        let supported = emulated::parse(&seat.mouse)
                            .ok()
                            .flatten()
                            .is_some_and(|device| device.kind == DeviceKind::Mouse)
                            && (self.profile.desktop.is_some() || self.profile.concurrent);
                        if supported {
                            self.profile.seats[index].cursor.horizontal_wheel_emulation = emulation;
                        }
                    }
                }
                "horizontal_wheel_scale" => {
                    if let Ok(scale) = value.parse::<f32>()
                        && scale.is_finite()
                        && let Some(index) = self.selected.filter(|i| *i < self.profile.seats.len())
                    {
                        self.profile.seats[index].cursor.horizontal_wheel_scale =
                            scale.clamp(0.1, 8.0);
                    }
                }
                "invert_horizontal_wheel" => {
                    if let Ok(invert) = value.parse::<bool>()
                        && let Some(index) = self.selected.filter(|i| *i < self.profile.seats.len())
                    {
                        self.profile.seats[index].cursor.invert_horizontal_wheel = invert;
                    }
                }
                name if name.starts_with("button_mapping_") => {
                    let Some(index) = self.selected.filter(|i| *i < self.profile.seats.len())
                    else {
                        continue;
                    };
                    let Some(source) = name
                        .strip_prefix("button_mapping_")
                        .and_then(|index| index.parse::<usize>().ok())
                        .filter(|index| *index < 5)
                    else {
                        continue;
                    };
                    let button = match value.to_ascii_lowercase().as_str() {
                        "" | "disabled" | "none" | "null" => None,
                        "primary" | "left" => Some(mux_core::desktop::MouseButton::Primary),
                        "secondary" | "right" => Some(mux_core::desktop::MouseButton::Secondary),
                        "middle" => Some(mux_core::desktop::MouseButton::Middle),
                        "x1" | "back" => Some(mux_core::desktop::MouseButton::X1),
                        "x2" | "forward" => Some(mux_core::desktop::MouseButton::X2),
                        _ => continue,
                    };
                    self.profile.seats[index].cursor.button_mapping[source] = button;
                }
                "selected_user" => {
                    if let Ok(user) = value.parse::<usize>()
                        && user < self.profile.seats.len()
                    {
                        self.selected = Some(user);
                    }
                }
                "replication_source" => {
                    if let Ok(source) = value.parse::<usize>()
                        && source < self.profile.seats.len()
                    {
                        self.edge_source = source;
                    }
                }
                "replication_target" => {
                    if let Ok(target) = value.parse::<usize>()
                        && target < self.profile.seats.len()
                    {
                        self.edge_target = target;
                    }
                }
                "replication_mouse" => {
                    if let Ok(enabled) = value.parse::<bool>() {
                        self.edge_mouse = enabled;
                    }
                }
                "replication_keyboard" => {
                    if let Ok(enabled) = value.parse::<bool>() {
                        self.edge_keyboard = enabled;
                    }
                }
                "replication_text" => {
                    if let Ok(enabled) = value.parse::<bool>() {
                        self.edge_text = enabled;
                    }
                }
                _ => {}
            }
        }
        self.replication_dirty |= replication_edit;
    }
    fn emulated_controls(&mut self, ui: &mut egui::Ui) {
        Heading::new("Emulated input").heading().show(ui);
        Text::new("Application-local devices. Your desktop pointer stays independent.")
            .caption()
            .show(ui);
        while self.lab_positions.len() < self.profile.seats.len() {
            let i = self.lab_positions.len();
            self.lab_positions
                .push(self.profile.seats[i].desktop_start.unwrap_or([0, 0]));
        }
        for index in 0..self.profile.seats.len() {
            let seat = self.profile.seats[index].clone();
            if !self.user_active(index) {
                let full_label = format!("{} - Virtual replica removed", seat.name);
                let display = bounded_display_text(
                    ui,
                    &full_label,
                    ui.style().text_styles[&egui::TextStyle::Body].clone(),
                    ui.available_width(),
                );
                ui.colored_label(Color32::LIGHT_RED, display)
                    .on_hover_text(full_label);
                continue;
            }
            let Some(mouse) = emulated::parse(&seat.mouse).ok().flatten() else {
                continue;
            };
            let enabled = self.session_ready && self.session.is_some() && !self.session_busy;
            let display_name = bounded_display_text(
                ui,
                &seat.name,
                ui.style().text_styles[&egui::TextStyle::Body].clone(),
                ui.available_width(),
            );
            ui.label(display_name).on_hover_text(&seat.name);
            ui.horizontal(|ui| {
                for (label, x, y) in [
                    ("Left", -40, 0),
                    ("Right", 40, 0),
                    ("Up", 0, -40),
                    ("Down", 0, 40),
                ] {
                    if self.button(ui, &format!("{} {label}", index + 1), enabled) {
                        self.session.as_ref().unwrap().request(Request::Input(
                            emulated::Packet::motion(mouse.handle as u32, x, y),
                        ));
                        self.session_busy = true;
                    }
                }
            });
            ui.horizontal(|ui| {
                for (label, packet) in [
                    (
                        "Hold click",
                        emulated::Packet::button(mouse.handle as u32, 0, false),
                    ),
                    (
                        "Release click",
                        emulated::Packet::button(mouse.handle as u32, 0, true),
                    ),
                    ("Wheel", emulated::Packet::wheel(mouse.handle as u32, 120)),
                ] {
                    if self.button(ui, &format!("{} {label}", index + 1), enabled) {
                        self.session
                            .as_ref()
                            .unwrap()
                            .request(Request::Input(packet));
                        self.session_busy = true;
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                for (label, button, released) in [
                    ("Hold Back", 3, false),
                    ("Release Back", 3, true),
                    ("Hold Forward", 4, false),
                    ("Release Forward", 4, true),
                ] {
                    if self.button(ui, &format!("{} {label}", index + 1), enabled) {
                        self.session.as_ref().unwrap().request(Request::Input(
                            emulated::Packet::button(mouse.handle as u32, button, released),
                        ));
                        self.session_busy = true;
                    }
                }
                if self.button(ui, &format!("{} Horizontal wheel", index + 1), enabled) {
                    self.session.as_ref().unwrap().request(Request::Input(
                        emulated::Packet::horizontal_wheel(mouse.handle as u32, 120),
                    ));
                    self.session_busy = true;
                }
            });
            if self.profile.desktop.is_some() {
                ui.horizontal(|ui| {
                    ui.label("Desktop X");
                    ui.add(
                        egui::DragValue::new(&mut self.lab_positions[index][0])
                            .range(-32767..=32767),
                    );
                    ui.label("Y");
                    ui.add(
                        egui::DragValue::new(&mut self.lab_positions[index][1])
                            .range(-32767..=32767),
                    );
                    if self.button(ui, &format!("{} Move to point", index + 1), enabled) {
                        let [x, y] = self.lab_positions[index];
                        self.session.as_ref().unwrap().request(Request::MoveTo {
                            user: index,
                            x,
                            y,
                        });
                        self.session_busy = true;
                    }
                });
            }
            if let Some(keyboard) = seat
                .keyboard
                .as_deref()
                .and_then(|p| emulated::parse(p).ok().flatten())
            {
                ui.horizontal_wrapped(|ui| {
                    for (label, key) in [
                        ("W", 0x57),
                        ("A", 0x41),
                        ("S", 0x53),
                        ("D", 0x44),
                        ("Space", 0x20),
                    ] {
                        if self.button(ui, &format!("{} Tap {label}", index + 1), enabled) {
                            self.session.as_ref().unwrap().request(Request::Tap {
                                device: keyboard.handle as u32,
                                key,
                            });
                            self.session_busy = true;
                        }
                    }
                });
                ui.horizontal(|ui| {
                    for (label, release) in [("Hold W", false), ("Release W", true)] {
                        if self.button(ui, &format!("{} {label}", index + 1), enabled) {
                            self.session.as_ref().unwrap().request(Request::Input(
                                emulated::Packet::key(keyboard.handle as u32, 0x57, release),
                            ));
                            self.session_busy = true;
                        }
                    }
                });
            }
        }
        let enabled = self.session_ready && self.session.is_some() && !self.session_busy;
        ui.horizontal(|ui| {
            if self.button(ui, "Hold all users", enabled) {
                let mut packets = Vec::new();
                for (index, seat) in self.profile.seats.iter().enumerate() {
                    if !self.user_active(index) {
                        continue;
                    }
                    if let Some(d) = emulated::parse(&seat.mouse).ok().flatten() {
                        packets.push(emulated::Packet::button(d.handle as u32, 0, false));
                    }
                    if let Some(d) = seat
                        .keyboard
                        .as_deref()
                        .and_then(|p| emulated::parse(p).ok().flatten())
                    {
                        packets.push(emulated::Packet::key(d.handle as u32, 0x57, false));
                    }
                }
                self.session
                    .as_ref()
                    .unwrap()
                    .request(Request::Batch(packets));
                self.session_busy = true;
            }
            if self.button(ui, "Release all users", enabled) {
                self.session.as_ref().unwrap().request(Request::ReleaseAll);
                self.session_busy = true;
            }
        });
        Divider::horizontal().show(ui);
    }

    fn keyboard_text_controls(&mut self, ui: &mut egui::Ui) {
        Heading::new("Keyboard and text").heading().show(ui);
        Text::new(
            "Send a full named key or a Unicode text string to the selected virtual keyboard.",
        )
        .caption()
        .wrap()
        .show(ui);
        let selected = self
            .selected
            .filter(|index| *index < self.profile.seats.len());
        let keyboard = selected.and_then(|index| {
            if !self.user_active(index) {
                return None;
            }
            self.profile.seats[index]
                .keyboard
                .as_deref()
                .and_then(|path| emulated::parse(path).ok().flatten())
                .map(|device| (index, device))
        });
        if let Some(index) = selected {
            if !self.user_active(index) {
                ui.colored_label(
                    Color32::LIGHT_RED,
                    "Virtual replica removed; choose an active user.",
                );
            }
            ui.label(format!(
                "User {} · {}",
                index + 1,
                self.profile.seats[index].name
            ));
        } else {
            ui.weak("Select a user from the roster first.");
        }
        let enabled = keyboard.is_some()
            && self.session_ready
            && self.session.is_some()
            && !self.session_busy;
        ui.horizontal(|ui| {
            ui.label("Key name");
            Input::new(&mut self.key_name)
                .id_source("input_lab_key_name")
                .show(ui);
        });
        ui.horizontal_wrapped(|ui| {
            for label in ["Hold key", "Release key", "Tap key"] {
                if self.button(ui, label, enabled && !self.key_name.trim().is_empty()) {
                    let (_, device) = keyboard.as_ref().expect("enabled requires a keyboard");
                    let result = (|| -> Result<()> {
                        let down = mux_windows::keyboard::packet(
                            device.handle as u32,
                            self.key_name.trim(),
                            false,
                        )?;
                        match label {
                            "Hold key" => {
                                self.session.as_ref().unwrap().request(Request::Input(down))
                            }
                            "Release key" => {
                                let up = mux_windows::keyboard::packet(
                                    device.handle as u32,
                                    self.key_name.trim(),
                                    true,
                                )?;
                                self.session.as_ref().unwrap().request(Request::Input(up));
                            }
                            _ => {
                                let up = mux_windows::keyboard::packet(
                                    device.handle as u32,
                                    self.key_name.trim(),
                                    true,
                                )?;
                                self.session
                                    .as_ref()
                                    .unwrap()
                                    .request(Request::Batch(vec![down, up]));
                            }
                        }
                        self.session_busy = true;
                        Ok(())
                    })();
                    if let Err(error) = result {
                        self.status = format!("{error:#}");
                    }
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label("Text");
            Input::new(&mut self.text_input)
                .id_source("input_lab_text")
                .show(ui);
        });
        ui.horizontal_wrapped(|ui| {
            if self.button(ui, "Send text", enabled && !self.text_input.is_empty())
                && let Some((user, _)) = keyboard.as_ref()
            {
                let (_, device) = keyboard.as_ref().expect("enabled requires keyboard");
                match emulated::text_packets(device.handle as u32, &self.text_input) {
                    Ok(_) => {
                        self.session.as_ref().unwrap().request(Request::Text {
                            user: *user,
                            text: self.text_input.clone(),
                        });
                        self.session_busy = true;
                    }
                    Err(error) => self.status = format!("{error:#}"),
                }
            }
        });
        if selected.is_some() && keyboard.is_none() {
            ui.colored_label(
                COLORS[4],
                "This user needs an emulated keyboard for these controls.",
            );
        }
        Divider::horizontal().show(ui);
    }

    fn automation_controls(&mut self, ui: &mut egui::Ui) {
        use crate::automation_runtime::{Command, Edge};

        Heading::new("Macro automation").heading().show(ui);
        let mapper_armed = self.mapper_page.snapshot().armed;
        if mapper_armed {
            ui.weak("Disarm Input Mapper before recording, playing or changing legacy routes.");
        }
        let studio_active = self.macro_studio_page.has_active_program();
        let playing = self
            .macro_playback
            .as_ref()
            .is_some_and(|playback| playback.state == "playing");

        ui.vertical(|ui| {
            if self.button(
                ui,
                "Refresh automation status",
                self.session_ready && !self.session_busy,
            ) {
                self.session
                    .as_ref()
                    .unwrap()
                    .request(Request::Automation(Command::Inspect));
                self.session_busy = true;
            }
            if self.button(
                ui,
                "Start recording",
                self.session_ready
                    && !self.session_busy
                    && !self.macro_recording
                    && !playing
                    && !studio_active
                    && !mapper_armed,
            ) {
                self.invalidate_macro_file();
                self.session
                    .as_ref()
                    .unwrap()
                    .request(Request::Automation(Command::RecordStart));
                self.session_busy = true;
            }
            if self.button(
                ui,
                "Stop recording",
                self.session.is_some() && self.macro_recording,
            ) {
                self.invalidate_macro_file();
                self.session
                    .as_ref()
                    .unwrap()
                    .request(Request::Automation(Command::RecordStop));
                self.session_busy = true;
            }
            if self.button(
                ui,
                "Cancel playback",
                self.session.is_some() && (playing || self.macro_recording || studio_active),
            ) {
                self.invalidate_macro_file();
                self.session
                    .as_ref()
                    .unwrap()
                    .request(Request::Automation(Command::Cancel));
                self.session_busy = true;
            }
        });

        ui.label(format!(
            "{} · {} recorded · {} loaded",
            if self.macro_recording {
                "Recording"
            } else if playing {
                "Playing"
            } else {
                "Idle"
            },
            self.macro_recorded_events,
            self.macro_loaded_events,
        ));
        if let Some(playback) = &self.macro_playback {
            ui.small(format!(
                "Playback {} · {} events · {} loops · {:.2}×",
                playback.state, playback.events_emitted, playback.loops_completed, playback.speed
            ));
        }
        Text::new("Cancel playback stops every user's programs and releases all session input.")
            .caption()
            .wrap()
            .show(ui);
        if self.macro_clip.is_some() {
            ui.weak("The acknowledged clip stays in memory until replaced or the session ends.");
        }
        if self.prominent_button(
            ui,
            "Play macro",
            self.session_ready
                && self.macro_loaded_events > 0
                && !self.session_busy
                && !self.macro_recording
                && !playing
                && !studio_active
                && !mapper_armed,
            None,
        ) {
            self.invalidate_macro_file();
            self.session
                .as_ref()
                .unwrap()
                .request(Request::Automation(Command::Play {
                    loops: (!self.macro_repeat).then_some(self.macro_loops),
                    speed: self.macro_speed,
                }));
            self.session_busy = true;
        }

        let macro_files = Collapsible::new("Macro file")
            .id_source("automation-macro-file")
            .default_open(false)
            .show(ui, |ui| {
                ui.label("Macro file");
                Input::new(&mut self.macro_path)
                    .id_source("automation_macro_path")
                    .show(ui);
                ui.horizontal_wrapped(|ui| {
                    if self.button(
                        ui,
                        "Save macro",
                        self.macro_clip.is_some()
                            && !self.macro_recording
                            && !playing
                            && !self.session_busy
                            && !self.session_stopping
                            && !self.macro_file.pending(),
                    ) {
                        let context = self.macro_file_context();
                        let path = PathBuf::from(self.macro_path.clone());
                        let result = self.macro_clip.as_ref().cloned().map_or_else(
                            || Err(anyhow::anyhow!("no macro is available to save")),
                            |clip| self.macro_file.save(path, clip, context),
                        );
                        self.macro_error = result.err().map(|error| format!("{error:#}"));
                        self.macro_file_result = Some(if self.macro_error.is_some() {
                            "failed"
                        } else {
                            "saving"
                        });
                    }
                    if self.button(
                        ui,
                        "Load macro",
                        self.session_ready
                            && !self.session_busy
                            && !self.macro_recording
                            && !playing
                            && !studio_active
                            && !self.session_stopping
                            && !self.macro_file.pending(),
                    ) {
                        let context = self.macro_file_context();
                        let result = self
                            .macro_file
                            .load(PathBuf::from(self.macro_path.clone()), context);
                        self.macro_error = result.err().map(|error| format!("{error:#}"));
                        self.macro_file_result = Some(if self.macro_error.is_some() {
                            "failed"
                        } else {
                            "loading"
                        });
                    }
                });
                if self.macro_file.pending() {
                    ui.weak("Macro file operation in progress…");
                } else if let Some(result) = self.macro_file_result
                    && matches!(result, "saved" | "load_requested" | "invalidated")
                {
                    ui.weak(match result {
                        "saved" => "Macro file saved.",
                        "load_requested" => "Macro file load requested; check automation status for acknowledgement.",
                        _ => "Macro file operation cancelled because the session changed.",
                    });
                }
            });
        if let Some(script) = self.script.as_mut() {
            script.observe("Macro file", &macro_files);
        }

        let playback_options = Collapsible::new("Playback options")
            .id_source("automation-playback-options")
            .default_open(false)
            .show(ui, |ui| {
                Checkbox::new(&mut self.macro_repeat)
                    .label("Repeat until cancelled")
                    .enabled(!playing && !self.macro_recording && !self.session_busy)
                    .show(ui);
                if !self.macro_repeat {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Loops");
                        ui.add_enabled(
                            !playing && !self.macro_recording && !self.session_busy,
                            egui::DragValue::new(&mut self.macro_loops).range(1..=1_000_000),
                        );
                    });
                }
                ui.horizontal_wrapped(|ui| {
                    ui.label("Speed");
                    ui.add_enabled(
                        !playing && !self.macro_recording && !self.session_busy,
                        egui::Slider::new(&mut self.macro_speed, 0.1..=4.0).show_value(true),
                    );
                });
            });
        if let Some(script) = self.script.as_mut() {
            script.observe("Playback options", &playback_options);
        }
        if let Some(error) = &self.macro_error {
            ui.colored_label(Theme::get(ui).error, error);
        }

        Divider::horizontal().show(ui);
        ui.add_enabled_ui(!mapper_armed, |ui| {
        Heading::new("Replication graph").heading().show(ui);
        let replication_help = Collapsible::new("Replication help")
            .id_source("automation-replication-help")
            .default_open(false)
            .show(ui, |ui| {
                Text::new("An edge copies selected event categories from one user to another. Changes remain a draft until Apply replication succeeds.")
                    .caption()
                    .wrap()
                    .show(ui);
                Text::new("Macro automation is the legacy session-wide recorder and player. Macro Studio keeps a separate program for each user.")
                    .caption()
                    .wrap()
                    .show(ui);
            });
        if let Some(script) = self.script.as_mut() {
            script.observe("Replication help", &replication_help);
        }

        let seat_count = self.profile.seats.len();
        if seat_count == 0 {
            ui.weak("Add users before configuring replication.");
        } else {
            let max = seat_count.saturating_sub(1);
            self.edge_source = self.edge_source.min(max);
            self.edge_target = self.edge_target.min(max);
            let user_label_width = (ui.available_width() - 130.0).max(50.0);
            let user_font = ui.style().text_styles[&egui::TextStyle::Body].clone();
            let user_options: Vec<String> = self
                .profile
                .seats
                .iter()
                .enumerate()
                .map(|(index, seat)| {
                    bounded_display_text(
                        ui,
                        &format!("User {}: {}", index + 1, seat.name),
                        user_font.clone(),
                        user_label_width,
                    )
                })
                .collect();
            let user_choices: Vec<&str> = user_options.iter().map(String::as_str).collect();
            ui.vertical(|ui| {
                if let Some(source) = self.choice_menu(
                    ui,
                    "From user",
                    user_choices[self.edge_source],
                    &user_choices,
                    true,
                ) {
                    self.edge_source = source;
                }
                if let Some(target) = self.choice_menu(
                    ui,
                    "To user",
                    user_choices[self.edge_target],
                    &user_choices,
                    true,
                ) {
                    self.edge_target = target;
                }
            });
            Text::new(format!(
                "From: {} · To: {}",
                self.profile.seats[self.edge_source].name,
                self.profile.seats[self.edge_target].name,
            ))
            .caption()
            .wrap()
            .show(ui);
            ui.horizontal_wrapped(|ui| {
                Checkbox::new(&mut self.edge_mouse).label("Mouse").show(ui);
                Checkbox::new(&mut self.edge_keyboard)
                    .label("Keyboard")
                    .show(ui);
                Checkbox::new(&mut self.edge_text).label("Text").show(ui);
            });
            if self.button(
                ui,
                "Add replication edge",
                seat_count > 1
                    && !playing
                    && !self.macro_recording
                    && !self.session_busy
                    && self.edge_source != self.edge_target
                    && (self.edge_mouse || self.edge_keyboard || self.edge_text)
                    && self.replication_edges.len() < mux_core::automation::MAX_REPLICATION_EDGES,
            ) {
                self.replication_edges.push(Edge {
                    source: self.edge_source,
                    target: self.edge_target,
                    mouse: self.edge_mouse,
                    keyboard: self.edge_keyboard,
                    text: self.edge_text,
                    options: Default::default(),
                });
                self.replication_dirty = true;
            }
        }

        let mut remove = None;
        for index in 0..self.replication_edges.len() {
            let mut edge = self.replication_edges[index];
            let mut remove_this = false;
            let mut changed = false;
            let source_name = self
                .profile
                .seats
                .get(edge.source)
                .map(|seat| seat.name.clone())
                .unwrap_or_else(|| "Unknown user".into());
            let target_name = self
                .profile
                .seats
                .get(edge.target)
                .map(|seat| seat.name.clone())
                .unwrap_or_else(|| "Unknown user".into());
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{source_name} → {target_name}"));
                changed |= Checkbox::new(&mut edge.mouse)
                    .label("Mouse")
                    .show(ui)
                    .changed();
                changed |= Checkbox::new(&mut edge.keyboard)
                    .label("Keyboard")
                    .show(ui)
                    .changed();
                changed |= Checkbox::new(&mut edge.text)
                    .label("Text")
                    .show(ui)
                    .changed();
                remove_this = self.button(ui, "Remove edge", true);
            });
            self.replication_edges[index] = edge;
            self.replication_dirty |= changed;
            if remove_this {
                remove = Some(index);
            }
        }
        if let Some(index) = remove {
            self.replication_edges.remove(index);
            self.replication_dirty = true;
        }
        if self.replication_dirty {
            ui.weak("Replication draft has unapplied changes.");
        }
        if self.button(
            ui,
            "Apply replication",
            self.session_ready
                && !self.session_busy
                && !playing
                && !self.macro_recording
                && !studio_active
                && self.session.is_some(),
        ) {
            self.session
                .as_ref()
                .unwrap()
                .request(Request::Automation(Command::Replication {
                    edges: self.replication_edges.clone(),
                }));
            self.replication_dirty = true;
            self.session_busy = true;
        }
        });
    }

    fn replication_controls(&mut self, ui: &mut egui::Ui) {
        let mapper_armed = self.mapper_page.snapshot().armed;
        if mapper_armed {
            ui.weak("Disarm Input Mapper before changing replication.");
        }
        let active_users: Vec<bool> = (0..self.profile.seats.len())
            .map(|user| self.user_active(user))
            .collect();
        let active = self.macro_recording
            || self
                .macro_playback
                .as_ref()
                .is_some_and(|p| p.state == "playing")
            || self.macro_studio_page.has_active_program();
        let action = ui
            .add_enabled_ui(!mapper_armed, |ui| {
                self.replication_page.show(
                    ui,
                    &self.profile.seats,
                    &active_users,
                    self.session.is_some(),
                    self.session_ready,
                    self.session_busy,
                    active,
                    self.script.as_mut(),
                    &mut self.action_frame,
                    &mut self.frame_actions,
                )
            })
            .inner;
        if let Some(crate::replication_ui::PageAction::Apply(ref definition)) = action {
            match definition.resolve(&self.profile.seats) {
                Ok(edges) => {
                    if let Some(session) = &self.session {
                        session.request(Request::Automation(
                            crate::automation_runtime::Command::Graph {
                                edges,
                                expected_revision: self.replication_revision,
                            },
                        ));
                        self.session_busy = true;
                    } else {
                        self.profile.replication = definition.clone();
                        self.replication_edges = edges;
                        self.replication_dirty = false;
                        self.replication_page.applied_local(definition);
                        self.status = "Replication graph applied to this profile. Save the profile to keep it.".into();
                    }
                }
                Err(error) => self.replication_page.failed_apply(format!("{error:#}")),
            }
        }
        if let Some(crate::replication_ui::PageAction::CancelReplication { expected_revision }) =
            action
            && let Some(session) = &self.session
        {
            session.request(Request::Automation(
                crate::automation_runtime::Command::CancelReplication { expected_revision },
            ));
            self.session_busy = true;
        }
        if let Some(script) = &self.script {
            script.graph_state(&self.replication_page.snapshot());
        }
    }
    fn forwarding_controls(&mut self, ui: &mut egui::Ui) {
        let mapper_armed = self.mapper_page.snapshot().armed;
        if mapper_armed {
            ui.weak("Disarm Input Mapper before changing forwarding.");
        }
        let active_users: Vec<bool> = (0..self.profile.seats.len())
            .map(|user| self.user_active(user))
            .collect();
        let automation_active = self.macro_recording
            || self
                .macro_playback
                .as_ref()
                .is_some_and(|play| play.state == "playing")
            || self.macro_studio_page.has_active_program();
        let apply = ui
            .add_enabled_ui(!mapper_armed, |ui| {
                self.forwarding_page.show(
                    ui,
                    &self.profile.seats,
                    &active_users,
                    self.profile.desktop.is_some(),
                    self.session.is_some(),
                    self.session_ready,
                    self.session_busy,
                    !self.profile.replication.edges.is_empty(),
                    automation_active,
                    self.script.as_mut(),
                    &mut self.action_frame,
                    &mut self.frame_actions,
                )
            })
            .inner;
        if apply {
            match self
                .forwarding_page
                .begin_apply(&self.profile.seats, self.session.is_some())
            {
                Ok((definition, edges)) => {
                    if let Some(session) = &self.session {
                        session.request(Request::Automation(
                            crate::automation_runtime::Command::Forwarding {
                                edges,
                                expected_revision: self.forwarding_page.snapshot().revision,
                            },
                        ));
                        self.session_busy = true;
                    } else {
                        self.profile.forwarding = definition.clone();
                        self.forwarding_page.applied_local(definition);
                        self.status = "Input forwarding applied to this profile. Save the profile to keep it.".into();
                    }
                }
                Err(error) => self.forwarding_page.failed_apply(format!("{error:#}")),
            }
        }
        if let Some(script) = &self.script {
            script.forwarding_state(&self.forwarding_page.snapshot());
        }
    }
    fn reset_mapper_session(&mut self) {
        self.mapper_commands.clear();
        self.mapper_owns_session_busy = false;
        self.mapper_page.session_stopped(&self.profile.mapper);
    }

    fn install_mapper_definition(&mut self, definition: mux_core::mapper::Definition) {
        if self.profile.mapper != definition {
            self.profile.mapper = definition;
            self.profile_interaction_epoch = self.profile_interaction_epoch.saturating_add(1);
            self.profile_file.invalidate();
            self.advance_setup_revision();
        }
    }

    fn receive_mapper_state(
        &mut self,
        state: &crate::mapper_runtime::State,
        publisher: MapperPublisher,
    ) {
        if self.session.is_none() || self.session_stopping {
            return;
        }
        let previous_worker_error = self.mapper_page.worker_error_text().map(str::to_owned);
        let committed = if let Some(command_id) = mapper_reply_id(state, publisher) {
            // A stale reply from a retired page/session must not clear unrelated busy work.
            if !self.mapper_commands.remove(&command_id) {
                return;
            }
            let committed = receive_mapper_page(&mut self.mapper_page, state, publisher);
            if self.mapper_commands.is_empty() && self.mapper_owns_session_busy {
                self.session_busy = false;
                self.mapper_owns_session_busy = false;
            }
            committed
        } else {
            receive_mapper_page(&mut self.mapper_page, state, publisher)
        };
        let snapshot = self.mapper_page.snapshot();
        if let Some(definition) = committed {
            self.install_mapper_definition(definition);
            self.status = "Input Mapper Apply acknowledged. Save the profile to keep it.".into();
        } else if !matches!(
            &snapshot.pending,
            Some(crate::mapper_ui::Pending::Apply { .. })
        ) {
            // External authority updates the saved definition while the page preserves dirty edits.
            // An exact own Apply publication waits for its correlated receipt before profile commit.
            self.install_mapper_definition(snapshot.applied.clone());
        }
        update_mapper_status(
            &mut self.status,
            previous_worker_error.as_deref(),
            snapshot.error.as_deref(),
        );
        if let Some(script) = &self.script {
            if matches!(publisher, MapperPublisher::Direct) {
                script.mapper_ack(state);
            }
            script.mapper_state(&snapshot);
        }
    }

    fn sync_screen_target(&mut self) {
        let binding = (self.session_ready && !self.session_stopping)
            .then(|| {
                let target = self.screen_targets.get(self.screen_page.user)?;
                Some(crate::screen_ui::Binding {
                    session: self.macro_file_session_epoch,
                    target_revision: target.revision,
                    user_id: target.user_id.clone(),
                    window: target.window?,
                })
            })
            .flatten();
        self.screen_page.sync(binding);
    }
    fn screen_capture(&mut self, query: Result<Option<crate::screen_analysis::Query>>) {
        match query {
            Ok(query) if self.screen_page.request() => {
                if let Some(session) = &self.session {
                    session.request(Request::Capture {
                        user: self.screen_page.user,
                        query,
                    });
                } else {
                    self.screen_page
                        .capture_error("Start routing and select an allowed window first.".into());
                }
            }
            Ok(_) => {
                self.screen_page.error =
                    Some("Wait for the current operation or select a target window.".into())
            }
            Err(error) => self.screen_page.error = Some(format!("{error:#}")),
        }
    }
    fn screen_controls(&mut self, ui: &mut egui::Ui) {
        Heading::new("Screen tools").h2().show(ui);
        Text::new("Inspect pixels, find a template or read text in the selected application window. These tools never send input.").wrap().show(ui);
        self.screen_page.check_ocr_capability(ui.ctx(), false);
        let users: Vec<_> = self
            .profile
            .seats
            .iter()
            .enumerate()
            .filter(|(index, seat)| {
                self.user_active(*index) && emulated::parse(&seat.mouse).ok().flatten().is_some()
            })
            .map(|(index, seat)| (index, seat.name.clone()))
            .collect();
        ui.horizontal_wrapped(|ui| {
            for (index, name) in users {
                let label = format!("Screen user {}", index + 1);
                if self.button(ui, &label, !self.screen_page.busy()) {
                    self.screen_page.user = index;
                }
                if self.screen_page.user == index {
                    Text::new(name).caption().show(ui);
                }
            }
        });
        self.sync_screen_target();
        let available = self.screen_page.available();
        ui.horizontal_wrapped(|ui| {
            if self.button(ui, "Capture selected window", available) {
                self.screen_capture(Ok(None));
            }
            if self.button(ui, "Inspect pixel", available) {
                let query = self.screen_page.pixel_query().map(Some);
                self.screen_capture(query);
            }
            let template_ready = self.screen_page.snapshot()["template_size"].is_array();
            if self.button(ui, "Find template", available && template_ready) {
                let query = self.screen_page.match_query().map(Some);
                self.screen_capture(query);
            }
            if self.button(
                ui,
                "Read text",
                available && self.screen_page.ocr_available(),
            ) {
                let query = self.screen_page.ocr_query().map(Some);
                self.screen_capture(query);
            }
        });
        Text::new(self.screen_page.ocr_status())
            .caption()
            .wrap()
            .show(ui);
        if !self.screen_page.ocr_available()
            && self.button(
                ui,
                "Check OCR models",
                !self.screen_page.ocr_check_pending(),
            )
        {
            self.screen_page.check_ocr_capability(ui.ctx(), true);
        }
        let options = Collapsible::new("Text extraction options")
            .id_source("ocr-region-options").default_open(false).show(ui, |ui| {
                Text::new("Local Latin text model · early preview. Leave the region blank for the whole client. Limit: 1,048,576 pixels, at most 4096 on either edge. Narrow very wide text lines with a region.").caption().wrap().show(ui);
                for (index, label) in ["Text region X", "Text region Y", "Text region width", "Text region height"].iter().enumerate() {
                    Text::new(*label).caption().show(ui);
                    Input::new(&mut self.screen_page.ocr_region[index])
                        .show(ui);
                }
            });
        if let Some(script) = &mut self.script {
            script.observe("Text extraction options", &options);
        }
        if !available && !self.screen_page.busy() {
            Text::new("Start automatic routing with a virtual user and choose an explicitly allowed target window. Capture is unavailable until selection is acknowledged.").caption().wrap().show(ui);
        }
        if self.screen_page.busy() {
            Text::new("Snapshot operation in progress…").show(ui);
        }
        self.screen_page.show_snapshot(ui);
        let preview = Collapsible::new("Snapshot preview")
            .id_source("screen-preview")
            .default_open(false)
            .show(ui, |ui| self.screen_page.show_preview(ui));
        if let Some(script) = self.script.as_mut() {
            script.observe("Snapshot preview", &preview);
        }

        Divider::horizontal().show(ui);
        Text::new("Pixel coordinates · relative to the target client area").show(ui);
        ui.columns(2, |columns| {
            for (index, label) in ["Pixel X", "Pixel Y"].into_iter().enumerate() {
                let ui = &mut columns[index];
                Text::new(label).caption().show(ui);
                Input::new(&mut self.screen_page.pixel[index])
                    .id_source(("screen_pixel", index))
                    .show(ui);
            }
        });
        Text::new("Template region · from the captured snapshot, at most 128 × 128")
            .wrap()
            .show(ui);
        ui.columns(2, |columns| {
            for (index, label) in [
                "Template X",
                "Template Y",
                "Template width",
                "Template height",
            ]
            .into_iter()
            .enumerate()
            {
                let ui = &mut columns[index % 2];
                Text::new(label).caption().show(ui);
                Input::new(&mut self.screen_page.region[index])
                    .id_source(("screen_region", index))
                    .show(ui);
            }
        });
        if self.button(
            ui,
            "Use region as template",
            available && self.screen_page.has_snapshot(),
        ) && let Err(error) = self.screen_page.save_template(ui.ctx())
        {
            self.screen_page.error = Some(format!("{error:#}"));
        }
        let search = Collapsible::new("Search settings")
            .id_source("screen-search")
            .default_open(false)
            .show(ui, |ui| {
                Text::new("Search region · leave all four fields empty for the whole snapshot")
                    .wrap()
                    .show(ui);
                ui.columns(2, |columns| {
                    for (index, label) in ["Search X", "Search Y", "Search width", "Search height"]
                        .into_iter()
                        .enumerate()
                    {
                        let ui = &mut columns[index % 2];
                        Text::new(label).caption().show(ui);
                        Input::new(&mut self.screen_page.search[index])
                            .id_source(("screen_search", index))
                            .show(ui);
                    }
                });
                Text::new("Minimum similarity · 0 to 1").caption().show(ui);
                Input::new(&mut self.screen_page.min_score)
                    .id_source("screen_min_score")
                    .show(ui);
            });
        if let Some(script) = self.script.as_mut() {
            script.observe("Search settings", &search);
        }
        Text::new("Matching compares RGB pixels at every candidate position; alpha is ignored. Large searches return a work-budget error. Narrow the search region and try again. Templates and snapshots are memory only and clear when the user, target or session changes.").caption().wrap().show(ui);
        if let Some(script) = &self.script {
            script.screen_state(&self.screen_page.snapshot());
        }
    }
    fn mapper_controls(&mut self, ui: &mut egui::Ui) {
        use crate::mapper_runtime::Command;
        use crate::mapper_ui::PageAction;
        self.screen_page.check_ocr_capability(ui.ctx(), false);
        self.mapper_page
            .set_ocr_available(self.screen_page.ocr_available());
        let active_users: Vec<bool> = (0..self.profile.seats.len())
            .map(|user| self.user_active(user))
            .collect();
        if self.mapper_page.can_stage_screen_template() {
            Text::new("Copying a Screen tools template into this graph makes the image visible to graph readers and saves it with profiles or flows after Update and Apply.").caption().wrap().show(ui);
            if self.button(
                ui,
                "Use Screen tools template in graph",
                self.screen_page.graph_template().is_some()
                    && !self.session_stopping
                    && !self.quit_after_stop
                    && !self.allow_close
                    && !self.session_busy,
            ) && let Some(template) = self.screen_page.graph_template()
                && let Err(error) = self.mapper_page.stage_screen_template(template)
            {
                self.status = format!("Input Mapper: {error:#}");
            }
        }
        let action = self.mapper_page.show(
            ui,
            &self.profile.seats,
            &active_users,
            self.session.is_some(),
            self.session_ready,
            self.session_busy,
            !self.session_stopping && !self.quit_after_stop && !self.allow_close,
            self.script.as_mut(),
            &mut self.action_frame,
            &mut self.frame_actions,
        );
        if action.is_none()
            && self.session_ready
            && !self.session_stopping
            && !self.session_busy
            && let Some(session) = &self.session
            && let Some(request_id) = self.mapper_page.monitor_request(Instant::now())
        {
            session.request(Request::InspectMapperMonitors { request_id });
        }
        if let Some(action) = action {
            let (command_id, command) = match action {
                PageAction::Apply {
                    command_id,
                    definition,
                    expected_revision,
                } => {
                    if self.session.is_none() {
                        let mut candidate = self.profile.clone();
                        candidate.mapper = definition.clone();
                        match candidate.validate() {
                            Ok(()) => {
                                self.install_mapper_definition(definition.clone());
                                self.mapper_page.applied_local(command_id, &definition);
                                self.status = "Input Mapper applied to this profile, disarmed. Save the profile to keep it.".into();
                            }
                            Err(error) => self
                                .mapper_page
                                .request_failed(command_id, format!("{error:#}")),
                        }
                        if let Some(script) = &self.script {
                            script.mapper_state(&self.mapper_page.snapshot());
                        }
                        return;
                    }
                    (
                        command_id,
                        Command::Apply {
                            definition,
                            expected_revision,
                            gui: Some(command_id),
                        },
                    )
                }
                PageAction::Arm {
                    command_id,
                    expected_revision,
                } => (
                    command_id,
                    Command::Arm {
                        expected_revision,
                        gui: Some(command_id),
                    },
                ),
                PageAction::Disarm {
                    command_id,
                    expected_revision,
                } => (
                    command_id,
                    Command::Disarm {
                        expected_revision,
                        gui: Some(command_id),
                    },
                ),
            };
            if let Some(session) = &self.session {
                self.mapper_commands.insert(command_id);
                if !self.session_busy {
                    self.mapper_owns_session_busy = true;
                }
                self.session_busy = true;
                session.request(Request::Automation(
                    crate::automation_runtime::Command::Mapper(command),
                ));
            } else {
                self.mapper_page
                    .request_failed(command_id, "Routing session is unavailable.");
            }
        }
        if let Some(script) = &self.script {
            script.mapper_state(&self.mapper_page.snapshot());
        }
    }

    fn macro_studio_controls(&mut self, ui: &mut egui::Ui, user: Option<usize>) {
        if let Some(user) = user
            && !self.macro_studio_page.focus_user(&self.profile.seats, user)
        {
            Text::new("Waiting for the previous user's macro command to finish.")
                .wrap()
                .show(ui);
            return;
        }
        let user_active = self.user_active(self.macro_studio_page.selected_user());
        if !user_active {
            ui.colored_label(
                Color32::LIGHT_RED,
                "Virtual replica removed; Macro Studio is unavailable for this user.",
            );
        }
        let mapper_armed = self.mapper_page.snapshot().armed;
        if mapper_armed {
            ui.weak("Disarm Input Mapper before recording or playing Macro Studio programs.");
        }
        let composite_active = mapper_armed
            || self.macro_recording
            || self
                .macro_playback
                .as_ref()
                .is_some_and(|playback| playback.state == "playing");
        let action = self.macro_studio_page.show(
            ui,
            &self.profile.seats,
            user.is_none(),
            self.session.is_some(),
            self.session_ready && user_active,
            self.session_busy,
            composite_active,
            self.script.as_mut(),
            &mut self.action_frame,
            &mut self.frame_actions,
        );
        if let Some(crate::macro_studio_ui::PageAction::Worker(command)) = action {
            self.invalidate_macro_file();
            if let Some(session) = &self.session {
                session.request(Request::MacroStudio(command));
                self.session_busy = true;
            }
        }
    }
    fn sdk_retired(&self) -> bool {
        self.sdk_service
            .as_ref()
            .is_none_or(|service| service.snapshot().retired)
    }

    fn stop_sdk(&self) {
        if let Some(service) = &self.sdk_service {
            service.request_stop();
        }
    }

    fn sdk_authority_ready(&self) -> bool {
        let broker = self.agent_broker.status();
        let policy = self.agent_page.snapshot();
        matches!(broker.phase, crate::gui_agent_broker::Phase::Active)
            && broker.session_epoch == self.agent_session_epoch
            && broker.descriptor_path.is_some()
            && !self.quit_after_stop
            && !self.preferences_quit_waiting
            && !self.session_stopping
            && !self.restart_requested
            && ((self.session.is_none()
                && self.setup_handle.enabled()
                && self.setup_pending.is_none()
                && !self.setup_retiring
                && self.setup_page.descriptor.is_some())
                || (self.session.is_some()
                    && self.session_ready
                    && policy.policy_enabled
                    && !policy.pending
                    && !self.app_request_handle.continuing()))
    }

    fn poll_sdk(&self, ctx: &egui::Context) {
        if let Some(service) = &self.sdk_service {
            let broker = self.agent_broker.status();
            if broker.generation != service.broker_generation
                || broker.descriptor_path.as_ref() != Some(&service.broker_descriptor_path)
                || !self.sdk_authority_ready()
            {
                service.request_stop();
            }
            if !service.snapshot().retired {
                ctx.request_repaint_after(Duration::from_millis(40));
            }
        }
    }

    fn sdk_controls(&mut self, ui: &mut egui::Ui) {
        let snapshot = self
            .sdk_service
            .as_ref()
            .map(|service| service.snapshot())
            .unwrap_or_default();
        let broker = self.agent_broker.status();
        let bridge_ready = self.sdk_authority_ready();
        let action = crate::agent_ui::sdk_controls(
            ui,
            &snapshot,
            &mut self.sdk_origins,
            bridge_ready,
            self.script.as_mut(),
            (&mut self.action_frame, &mut self.frame_actions),
        );
        match action {
            Some(crate::agent_ui::SdkAction::Enable) => {
                let result = (|| -> Result<crate::gui_sdk::Service> {
                    let origins = crate::gui_sdk::origin_draft(&self.sdk_origins)?;
                    let parent = self
                        .app_preferences_path
                        .parent()
                        .context("application data directory is unavailable")?;
                    crate::gui_sdk::Service::start(
                        crate::sdk_listener::Config {
                            endpoint_path: parent.join(format!(
                                "sdk-endpoint-{}-{}.json",
                                std::process::id(),
                                broker.generation
                            )),
                            gui_connection: broker
                                .descriptor_path
                                .context("Agent Access descriptor is unavailable")?,
                            runtime: self.runtime.clone().into(),
                            preferences: self.app_preferences_path.clone(),
                            allowed_origins: origins,
                            stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                        },
                        broker.generation,
                        Arc::clone(&self._runtime_guards),
                    )
                })();
                match result {
                    Ok(service) => {
                        self.sdk_service = Some(service);
                        self.status = "Preparing SDK connections…".into();
                    }
                    Err(error) => self.status = format!("SDK could not start: {error:#}"),
                }
            }
            Some(crate::agent_ui::SdkAction::Disable) => {
                self.stop_sdk();
                self.status = "SDK connections are closing; accepted requests and input cleanup will finish in the background.".into();
            }
            Some(crate::agent_ui::SdkAction::Copy) => {
                if let Some(connection) = self
                    .sdk_service
                    .as_ref()
                    .and_then(crate::gui_sdk::Service::connection_for_copy)
                {
                    ui.ctx().copy_text(connection);
                    self.status = "Private SDK connection copied. Share it only with a trusted app whose exact origin is allowed.".into();
                }
            }
            None => {}
        }
    }

    fn agent_controls(&mut self, ui: &mut egui::Ui) {
        crate::agent_ui::access_tabs(
            ui,
            &mut self.agent_sdk_tab,
            self.script.as_mut(),
            (&mut self.action_frame, &mut self.frame_actions),
        );
        if self.agent_sdk_tab {
            crate::application_event_ui::sdk_kind_tabs(
                ui,
                &mut self.application_event_tab,
                self.script.as_mut(),
                &mut self.action_frame,
                &mut self.frame_actions,
            );
            if self.application_event_tab {
                egui::ScrollArea::vertical()
                    .id_salt("application-event-sdk-body")
                    .show(ui, |ui| self.application_event_controls(ui));
            } else {
                self.sdk_controls(ui);
            }
            if let Some(script) = &self.script {
                script.agent_state(&self.agent_page.snapshot());
            }
        } else {
            self.agent_access_controls(ui);
        }
    }

    fn agent_access_controls(&mut self, ui: &mut egui::Ui) {
        self.agent_page
            .set_activity(self.app_request_handle.activity_snapshot());
        self.setup_page
            .set_activity(self.setup_handle.activity_snapshot());
        if self.session.is_none() && !self.app_request_handle.continuing() {
            self.setup_controls(ui);
            return;
        }
        if self.session.is_some() && self.profile.desktop.is_none() && !self.profile.concurrent {
            Text::new("Agent access is unavailable in fixed routing without desktop cursors. Start automatic desktop routing or concurrent assigned routing to connect agents.").wrap().show(ui);
            return;
        }
        let active_users: Vec<bool> = (0..self.profile.seats.len())
            .map(|user| self.user_active(user))
            .collect();
        let allowed_pids = if let Some(desktop) = &self.profile.desktop {
            desktop.allowed_pids.clone()
        } else {
            self.profile
                .seats
                .iter()
                .map(|seat| seat.target_pid)
                .filter(|pid| *pid != 0 && *pid != std::process::id())
                .collect()
        };
        let broker = self.agent_broker.status();
        let broker_busy = matches!(
            broker.phase,
            crate::gui_agent_broker::Phase::Starting | crate::gui_agent_broker::Phase::Stopping
        );
        let continuing = self.app_request_handle.continuing();
        if continuing {
            Heading::new("Agent access").heading().show(ui);
            Text::new("Routing has stopped accepting agent commands. Existing connections can only inspect recovery or disconnect.")
                .wrap().show(ui);
            Text::new("New connections, capture and preference changes are unavailable.")
                .caption()
                .wrap()
                .show(ui);
            if self.home_button(
                ui,
                "Disable agent access",
                !broker_busy,
                ButtonVariant::Outline,
            ) {
                self.app_request_handle.revoke();
                self.agent_pending_enable = None;
                self.agent_page.reset_session();
                self.agent_broker.request_stop(self.agent_session_epoch);
                self.status = "Agent Access disabled; broker cleanup is pending.".into();
            }
            if let Some(script) = &self.script {
                script.agent_state(&self.agent_page.snapshot());
            }
            return;
        }
        let action = self.agent_page.show(
            ui,
            &self.profile.seats,
            &active_users,
            &allowed_pids,
            &self.targets,
            &self.agent_descriptor_path,
            self.agent_executable_path.as_deref(),
            &broker.phase,
            self.session.is_some() || continuing,
            self.session_ready,
            self.session_busy || broker_busy,
            self.script.as_mut(),
            &mut self.action_frame,
            &mut self.frame_actions,
        );
        if let Some(crate::agent_ui::PageAction::Configure(command)) = action {
            let enabled = matches!(
                &command,
                crate::agent_runtime::Command::Configure { enabled: true, .. }
            );
            if !enabled {
                self.stop_sdk();
            }
            if enabled {
                let Some(session) = self.session.as_ref() else {
                    self.agent_page
                        .request_failed("Start routing before enabling Agent Access");
                    return;
                };
                let broker = self.agent_broker.status();
                if matches!(broker.phase, crate::gui_agent_broker::Phase::Active)
                    && broker.session_epoch == self.agent_session_epoch
                    && broker.descriptor_path.is_some()
                {
                    self.app_request_handle.prepare_policy();
                    session.request(Request::Agent(command));
                    self.session_busy = true;
                } else {
                    if !matches!(broker.phase, crate::gui_agent_broker::Phase::Stopped) {
                        self.agent_page
                            .request_failed("Wait for previous Agent Access cleanup");
                        return;
                    }
                    self.app_request_handle.prepare_broker();
                    match self.agent_broker.request_start(
                        self.agent_session_epoch,
                        session.shared_handle(),
                        self.app_request_handle.clone(),
                    ) {
                        Ok(generation) => {
                            self.agent_pending_enable =
                                Some((generation, self.agent_session_epoch, command));
                            self.status = "Preparing Agent Access…".into();
                        }
                        Err(error) => {
                            self.agent_page.request_failed(format!("{error:#}"));
                            self.status = format!("Agent bridge could not start: {error:#}");
                        }
                    }
                }
            } else if let Some(session) = &self.session {
                self.app_request_handle.prepare_policy();
                let broker = self.agent_broker.status();
                if matches!(broker.phase, crate::gui_agent_broker::Phase::Starting)
                    && broker.session_epoch == self.agent_session_epoch
                {
                    self.agent_pending_enable =
                        Some((broker.generation, self.agent_session_epoch, command));
                    self.status = "Applying Agent Access change when the broker is ready…".into();
                } else {
                    session.request(Request::Agent(command));
                    self.session_busy = true;
                }
            } else {
                // Terminal Disable revokes locally; no worker or synthetic ACK.
                self.app_request_handle.revoke();
                self.agent_pending_enable = None;
                self.agent_page.reset_session();
                self.agent_broker.request_stop(self.agent_session_epoch);
                self.status = "Agent Access disabled; broker cleanup is pending.".into();
            }
        }
        if let Some(script) = &self.script {
            script.agent_state(&self.agent_page.snapshot());
        }
    }

    fn setup_controls(&mut self, ui: &mut egui::Ui) {
        let broker = self.agent_broker.status();
        let action = self.setup_page.show(
            ui,
            self.setup_handle.enabled(),
            &broker.phase,
            self.agent_executable_path.as_deref(),
            self.script.as_mut(),
            (&mut self.action_frame, &mut self.frame_actions),
        );
        match action {
            Some(true) => {
                if self.quit_after_stop || self.preferences_quit_waiting {
                    self.setup_page.error =
                        Some("Finish Quit before enabling setup access.".into());
                    return;
                }
                let epoch = self.setup_handle.enable();
                self.setup_retiring = false;
                self.setup_retired_generation = None;
                self.setup_start_order = None;
                match self
                    .agent_broker
                    .request_setup_start(self.agent_session_epoch, self.setup_handle.clone())
                {
                    Ok(generation) => {
                        self.setup_pending = Some((generation, epoch));
                        self.setup_page.pending = true;
                        self.setup_page.error = None;
                    }
                    Err(error) => {
                        self.setup_handle.revoke();
                        self.setup_page.error = Some(format!("{error:#}"));
                    }
                }
            }
            Some(false) => {
                self.stop_sdk();
                self.setup_handle.revoke();
                self.setup_pending = None;
                self.setup_page.pending = false;
                self.setup_page.descriptor = None;
                self.setup_retiring = true;
                self.agent_broker.request_stop(self.agent_session_epoch);
                self.status = "Stopped setup access disabled; broker cleanup is pending.".into();
            }
            None => {}
        }
    }

    fn app_preferences_controls(&mut self, ui: &mut egui::Ui) {
        let shortcut_status = self.shortcut_status();
        let action = self.app_preferences_page.show(
            ui,
            (&self.app_preferences, &shortcut_status),
            !self.preferences_worker.pending()
                && !self.preferences_worker.retiring()
                && !self.session_stopping
                && !self.quit_after_stop
                && !self.preferences_quit_waiting,
            self.script.as_mut(),
            &mut self.action_frame,
            &mut self.frame_actions,
        );
        if let Some(candidate) = action
            && let Err(error) = self.submit_preferences(candidate, None)
        {
            self.app_preferences_page.set_error(format!("{error:#}"));
            self.status = format!("Application preferences were not applied: {error:#}");
        }
        self.publish_preferences_snapshot();
    }
    fn shortcut_status(&self) -> crate::app_lifecycle::ShortcutStatus {
        self.lifecycle.shortcut_status()
    }
    fn publish_preferences_snapshot(&self) {
        if let Some(script) = &self.script {
            script.app_preferences_state(
                &self.app_preferences,
                self.app_preferences_page
                    .draft()
                    .unwrap_or(&self.app_preferences),
                &self.app_preferences_path,
                &crate::ui_script::PreferencesUiStatus {
                    operation: self.preferences_worker.status(),
                    quit_waiting: self.preferences_quit_waiting,
                    notice: self.app_preferences_page.notice(),
                    error: self.app_preferences_page.error(),
                    editor: self.app_preferences_page.raw_state(),
                },
            );
        }
    }
    fn submit_preferences(
        &mut self,
        candidate: crate::app_preferences::Preferences,
        request: Option<crate::gui_bridge::AppRequest>,
    ) -> Result<()> {
        use crate::gui_app_preferences::{Kind, Origin};
        if self.quit_after_stop || self.preferences_quit_waiting {
            if let Some(request) = request {
                self.reply_preferences(request, Some("Application is quitting.".into()));
            }
            bail!("Application is quitting.");
        }
        let editor = self
            .app_preferences_page
            .capture_for_submit(&self.app_preferences);
        let (origin, decision) = request.as_ref().map_or_else(
            || {
                (
                    Origin::Local,
                    std::sync::Arc::new(crate::app_lifecycle::CommitDecision::default()),
                )
            },
            |request| {
                (
                    Origin::Connected {
                        client: request.client,
                        sequence: request.sequence,
                        authority: request.authority,
                    },
                    request.decision.clone(),
                )
            },
        );
        // A rejected admission still gets its own correlated response.
        match self
            .preferences_worker
            .submit(Kind::Configure, Some(candidate), origin, decision)
        {
            Ok(ticket) => {
                self.preferences_pending = Some(PreferencesPending {
                    ticket,
                    editor: Some(editor),
                    request,
                    setup_request: None,
                });
                self.app_preferences_page.set_pending(true);
                self.status = "Applying application preferences…".into();
                Ok(())
            }
            Err(error) => {
                if let Some(request) = request {
                    self.reply_preferences(request, Some(format!("{error:#}")));
                }
                Err(error)
            }
        }
    }
    fn submit_preferences_action(&mut self, kind: crate::gui_app_preferences::Kind) {
        match self.preferences_worker.submit(
            kind,
            None,
            crate::gui_app_preferences::Origin::Local,
            std::sync::Arc::new(crate::app_lifecycle::CommitDecision::default()),
        ) {
            Ok(ticket) => {
                self.preferences_pending = Some(PreferencesPending {
                    ticket,
                    editor: None,
                    request: None,
                    setup_request: None,
                });
                self.app_preferences_page.set_pending(true);
                self.status = "Updating application services…".into();
            }
            Err(error) => {
                self.status = format!("Application service request rejected: {error:#}");
                self.app_preferences_page.set_error(self.status.clone());
            }
        }
    }
    fn reply_preferences(&self, request: crate::gui_bridge::AppRequest, error: Option<String>) {
        let response = crate::agent_runtime::AppPreferencesState {
            acknowledged: true,
            client: request.client,
            sequence: request.sequence,
            preferences: Some(self.app_preferences.clone()),
            shortcut_status: Some(self.shortcut_status()),
            error: error.or_else(|| {
                self.preferences_worker.degraded().then(|| {
                    "Application preference state is unconfirmed; values are last acknowledged."
                        .into()
                })
            }),
        };
        if let Some(script) = &self.script {
            script.app_preferences_ack(&response);
        }
        request.reply(response);
    }
    fn poll_preferences(&mut self) {
        use crate::gui_app_preferences::Outcome;
        let Some(completion) = self.preferences_worker.poll() else {
            return;
        };
        let pending = self.preferences_pending.take();
        let permits_waiting_quit = completion.permits_waiting_quit();
        self.app_preferences_page.set_pending(false);
        // Committed application state is global. Client death, Stop, profile loads
        // and navigation never discard a positively acknowledged activation.
        if let Some(actual) = completion.committed {
            let policy_changed =
                self.app_preferences.profile_persistence() != actual.profile_persistence();
            self.app_preferences = actual;
            if policy_changed {
                self.advance_setup_revision();
            }
            if let Some(pending) = &pending
                && pending.ticket == completion.ticket
                && let Some(editor) = &pending.editor
            {
                self.app_preferences_page.completed(
                    &self.app_preferences,
                    editor,
                    completion.origin.local(),
                );
            }
        }
        self.status = match completion.outcome {
            Outcome::Committed => "Application preferences applied.".into(),
            Outcome::Retried if self.preferences_worker.degraded() => {
                "Shortcuts checked; application preferences remain unconfirmed. Apply locally."
                    .into()
            }
            Outcome::Retried => "Application shortcut check finished.".into(),
            Outcome::ShutdownComplete => "Application services stopped.".into(),
            _ => completion
                .error
                .clone()
                .unwrap_or_else(|| "Application preference operation did not complete.".into()),
        };
        if completion.error.is_some() {
            if completion.outcome != Outcome::Cancelled {
                self.app_preferences_page.set_error(self.status.clone());
            }
            // Failed/uncertain writes and teardown stay visible. A repeated Quit
            // cannot turn an unconfirmed shutdown into a synthetic clean exit.
            if !permits_waiting_quit {
                self.profile_exit.cancel();
                self.preferences_quit_waiting = false;
                self.profile_file_ui.quit_waiting = false;
                self.quit_after_stop = false;
                self.close_ready = false;
            }
        }
        if let Some(pending) = pending {
            if let Some(request) = pending.request {
                self.reply_preferences(request, completion.error.clone());
            }
            if let Some(request) = pending.setup_request {
                request.reply(
                    serde_json::json!({
                        "ticket": completion.ticket,
                        "outcome": completion.outcome,
                        "preferences_revision": completion.revision,
                        "revision": self.setup_revision,
                        "profile_persistence": self.app_preferences.profile_persistence(),
                    }),
                    completion.error,
                    None,
                );
            }
        }
    }
    fn drain_app_requests(&mut self) {
        let requests: Vec<_> = self.app_requests.try_iter().take(16).collect();
        for request in requests {
            let result = (|| -> Result<Option<crate::app_preferences::Preferences>> {
                use std::sync::atomic::Ordering;
                ensure!(
                    self.app_request_handle
                        .permits_app(request.client, request.authority),
                    "App preferences are unavailable during recovery inspection"
                );
                ensure!(
                    request.connection_alive.load(Ordering::Acquire),
                    "GUI client disconnected"
                );
                ensure!(
                    !request.decision.cancelled(),
                    "App-preference request cancelled"
                );
                ensure!(
                    self.agent_page.client_is_enabled(request.client),
                    "Agent Access client is no longer active"
                );
                match &request.command {
                    crate::agent_runtime::AppPreferencesCommand::Inspect => Ok(None),
                    crate::agent_runtime::AppPreferencesCommand::Configure { settings } => {
                        ensure!(
                            self.agent_page.allows_app_preferences(request.client),
                            "Allow app-preference changes in Agent Access first"
                        );
                        Ok(Some(settings.apply_to(&self.app_preferences)?))
                    }
                }
            })();
            match result {
                Ok(Some(candidate)) => {
                    let _ = self.submit_preferences(candidate, Some(request));
                }
                Ok(None) => self.reply_preferences(request, None),
                Err(error) => {
                    let message = format!("{error:#}");
                    self.status = format!("Connected app-preference request rejected: {message}");
                    self.app_preferences_page.set_error(message.clone());
                    self.reply_preferences(request, Some(message));
                }
            }
        }
    }
    fn fail_profile_exit(&mut self, ctx: &egui::Context, error: String) {
        self.preferences_quit_waiting = false;
        self.quit_after_stop = false;
        self.close_ready = false;
        self.profile_file_ui.quit_waiting = false;
        self.profile_feedback = Some((error.clone(), true));
        self.status = error;
        self.handle_lifecycle_action(crate::app_lifecycle::LifecycleAction::Restore, ctx);
    }

    fn poll_preferences_quit(&mut self, ctx: &egui::Context) {
        if self.close_ready {
            self.close_ready = false;
            self.preferences_quit_waiting = true;
            self.profile_exit.begin();
        }
        if !self.preferences_quit_waiting {
            return;
        }
        if !self.all_sdks_retired() {
            self.stop_all_sdks();
            self.status = "Finishing SDK requests and input cleanup before quitting.".into();
            return;
        }
        // Reads can still install a requested setup. Never snapshot a fallback
        // while InitialLoad/Load/Import is outstanding.
        if self.profile_file.pending() || self.preferences_worker.pending() {
            self.status = if self.profile_file.pending() {
                "Finishing profile operation before quitting."
            } else {
                "Finishing application preference operation before quitting."
            }
            .into();
            return;
        }
        if self.session.is_some()
            || !self.recovery.snapshot().can_start
            || !crate::gui_agent_broker::quit_ready(true, &self.agent_broker.status().phase)
        {
            return;
        }
        if self.profile_exit.ready_to_submit(
            true,
            self.app_preferences.save_profile_on_exit,
            !self.profile_file_ui.initial_unresolved,
        ) {
            let path = PathBuf::from(&self.profile_path);
            let profile = std::sync::Arc::new(self.profile.clone());
            match self.profile_file.submit(
                crate::gui_profile_file::Kind::ExitSave,
                path.clone(),
                profile.clone(),
                self.profile_file_stamp(),
            ) {
                Ok(ticket) => {
                    if !self.profile_exit.submitted(ticket, path, profile) {
                        self.status =
                            "Exit profile save admission was not recorded; window remains open."
                                .into();
                        return;
                    }
                    self.status = "Saving applied profile settings before exit.".into();
                    return;
                }
                Err(error) => self
                    .profile_exit
                    .submission_failed(format!("Could not begin exit profile save: {error:#}")),
            }
        }
        if !self
            .profile_exit
            .permits_close(Path::new(&self.profile_path), &self.profile)
        {
            if let Some(error) = self.profile_exit.status().error {
                self.fail_profile_exit(ctx, error);
            }
            return;
        }
        if self.preferences_worker.shutdown_complete() {
            self.profile_file.invalidate();
            self.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if self.preferences_worker.retiring() {
            self.preferences_quit_waiting = false;
            self.quit_after_stop = false;
            self.status =
                "Application service shutdown is unconfirmed. The window remains open.".into();
        } else {
            self.submit_preferences_action(crate::gui_app_preferences::Kind::Shutdown);
        }
    }

    fn output_controls(&mut self, ui: &mut egui::Ui) {
        let active_users: Vec<bool> = (0..self.profile.seats.len())
            .map(|user| self.user_active(user))
            .collect();
        let routing_supported = self.profile.desktop.is_some() || self.profile.concurrent;
        let action = self.output_page.show(
            ui,
            &self.profile.seats,
            &active_users,
            self.session.is_some(),
            self.session_ready,
            routing_supported,
            self.session_busy,
            self.script.as_mut(),
            &mut self.action_frame,
            &mut self.frame_actions,
        );
        match action {
            Some(crate::output_ui::PageAction::SaveToProfile { user, settings }) => {
                let result = self.validate_output_settings(user, &settings, false);
                match result {
                    Ok(()) => {
                        if let Some(seat) = self.profile.seats.get_mut(user) {
                            seat.output = settings.clone();
                            self.output_page.saved_to_profile(user, settings);
                            self.status =
                                "Output rules saved to profile. Save the profile to keep them."
                                    .into();
                        }
                    }
                    Err(error) => {
                        self.output_page.request_failed(format!("{error:#}"));
                        self.status = format!("Output rules rejected: {error:#}");
                    }
                }
            }
            Some(crate::output_ui::PageAction::Apply { user, settings }) => {
                match self.validate_output_settings(user, &settings, true) {
                    Ok(()) => {
                        if let Some(session) = &self.session {
                            session.request(Request::Output(
                                crate::output_runtime::Command::Configure { user, settings },
                            ));
                            self.session_busy = true;
                        }
                    }
                    Err(error) => {
                        self.output_page.request_failed(format!("{error:#}"));
                        self.status = format!("Output rules rejected: {error:#}");
                    }
                }
            }
            None => (),
        }
    }
    fn validate_output_settings(
        &self,
        user: usize,
        settings: &mux_core::output::Settings,
        live: bool,
    ) -> Result<()> {
        crate::output_runtime::validate_keys(settings, mux_core::emulated::BASE | 3)?;
        let seat = self
            .profile
            .seats
            .get(user)
            .context("Unknown output-rules user")?;
        anyhow::ensure!(
            self.user_active(user),
            "Output rules cannot be changed for a removed user"
        );
        let mouse = emulated::parse(&seat.mouse)?;
        if settings != &mux_core::output::Settings::default() {
            anyhow::ensure!(
                mouse.is_some(),
                "Output conversion requires a virtual mouse user"
            );
        }
        if settings.requires_keyboard() || !settings.keys.is_empty() {
            anyhow::ensure!(mouse.is_some(), "Key output requires a virtual mouse user");
            let keyboard = seat
                .keyboard
                .as_deref()
                .context("Key output requires an assigned virtual keyboard")?;
            anyhow::ensure!(
                emulated::parse(keyboard)?.is_some(),
                "Key output requires an assigned virtual keyboard"
            );
        }
        if live {
            anyhow::ensure!(
                self.session.is_some() && self.session_ready,
                "Start routing before applying live output rules"
            );
            anyhow::ensure!(
                self.profile.desktop.is_some() || self.profile.concurrent,
                "Output conversion needs automatic desktop or concurrent assigned routing"
            );
        }
        Ok(())
    }
    fn annotation_controls(&mut self, ui: &mut egui::Ui, user: Option<usize>) {
        if let Some(user) = user
            && !self.annotation_page.focus_user(&self.profile.seats, user)
        {
            Text::new("Waiting for the previous user's drawing command to finish.")
                .wrap()
                .show(ui);
            return;
        }
        let user_active = self.user_active(self.annotation_page.selected_user());
        if !user_active {
            ui.colored_label(
                Color32::LIGHT_RED,
                "Virtual replica removed; annotation is unavailable for this user.",
            );
        }
        let annotation_routing_supported = self.profile.concurrent
            || self
                .profile
                .desktop
                .as_ref()
                .is_some_and(|desktop| desktop.mode == mux_core::routing::Mode::Concurrent);
        if !annotation_routing_supported {
            ui.colored_label(
                Color32::LIGHT_RED,
                "Annotation needs automatic desktop or concurrent assigned routing; it is unavailable in this fixed switched mode.",
            );
        }
        let selected_target_available = user_active
            && self.session_ready
            && self
                .states
                .get(self.annotation_page.selected_user())
                .is_some_and(|state| state.pid > 0);
        let action = self.annotation_page.show(
            ui,
            &self.profile.seats,
            user.is_none(),
            self.session.is_some(),
            self.session_ready && annotation_routing_supported && user_active,
            annotation_routing_supported,
            selected_target_available,
            self.session_busy,
            self.script.as_mut(),
            &mut self.action_frame,
            &mut self.frame_actions,
        );
        match action {
            Some(crate::annotation_ui::PageAction::SaveToProfile { user, settings }) => {
                let result = settings.validate().and_then(|()| {
                    anyhow::ensure!(
                        settings.clear_key.is_none()
                            || self
                                .profile
                                .seats
                                .get(user)
                                .is_some_and(|seat| seat.keyboard.is_some()),
                        "annotation clear shortcut requires an assigned keyboard"
                    );
                    anyhow::ensure!(
                        settings.tool == mux_core::annotation::Tool::Off
                            || self.profile.concurrent
                            || self.profile.desktop.as_ref().is_some_and(
                                |desktop| desktop.mode == mux_core::routing::Mode::Concurrent
                            ),
                        "annotation needs automatic desktop or concurrent assigned routing"
                    );
                    Ok(())
                });
                match result {
                    Ok(()) => {
                        if let Some(seat) = self.profile.seats.get_mut(user) {
                            seat.annotation = settings;
                            self.status = "Annotation settings saved to profile.".into();
                        }
                    }
                    Err(error) => self.status = format!("Annotation settings rejected: {error:#}"),
                }
            }
            Some(crate::annotation_ui::PageAction::Worker(command)) => {
                if let Some(session) = &self.session {
                    session.request(Request::Annotation(command));
                    self.session_busy = true;
                }
            }
            None => (),
        }
    }
    fn magnifier_controls(&mut self, ui: &mut egui::Ui, user: Option<usize>) {
        if let Some(user) = user
            && !self.magnifier_page.focus_user(&self.profile.seats, user)
        {
            Text::new("Waiting for the previous user's magnifier command to finish.")
                .wrap()
                .show(ui);
            return;
        }
        let user_active = self.user_active(self.magnifier_page.selected_user());
        if !user_active {
            ui.colored_label(
                Color32::LIGHT_RED,
                "Virtual replica removed; magnification is unavailable for this user.",
            );
        }
        let routing_supported = self.profile.concurrent
            || self
                .profile
                .desktop
                .as_ref()
                .is_some_and(|desktop| desktop.mode == mux_core::routing::Mode::Concurrent);
        if !routing_supported {
            ui.colored_label(
                Color32::LIGHT_RED,
                "Magnification needs automatic desktop or concurrent assigned routing; it is unavailable in this fixed switched mode.",
            );
        }
        let selected_target_available = user_active
            && self.session_ready
            && self
                .states
                .get(self.magnifier_page.selected_user())
                .is_some_and(|state| state.pid > 0);
        let action = self.magnifier_page.show(
            ui,
            &self.profile.seats,
            user.is_none(),
            &self.monitors,
            self.session.is_some(),
            self.session_ready && user_active,
            routing_supported,
            selected_target_available,
            self.session_busy,
            self.monitor_error.as_deref(),
            self.script.as_mut(),
            &mut self.action_frame,
            &mut self.frame_actions,
        );
        match action {
            Some(crate::magnifier_ui::PageAction::SaveToProfile { user, settings }) => {
                let result = (|| -> Result<()> {
                    settings.validate()?;
                    crate::magnifier_runtime::validate_settings(&settings)?;
                    anyhow::ensure!(
                        settings.mode == mux_core::magnifier::Mode::Off || routing_supported,
                        "magnifier needs automatic desktop or concurrent assigned routing"
                    );
                    if settings.source == mux_core::magnifier::Source::Desktop
                        && let Some(region) = settings.desktop_region
                    {
                        anyhow::ensure!(
                            mux_windows::targets::desktop_bounds().intersection(region)
                                == Some(region),
                            "desktop capture region must lie inside the physical virtual desktop"
                        );
                    }
                    if let Some(monitor_id) = &settings.monitor_id {
                        mux_windows::monitors::validate_settings(
                            &mux_core::desktop::CursorSettings {
                                bounds_mode: mux_core::desktop::BoundsMode::Monitor,
                                monitor_id: Some(monitor_id.clone()),
                                monitor_work_area: settings.work_area,
                                ..Default::default()
                            },
                        )?;
                    }
                    Ok(())
                })();
                match result {
                    Ok(()) => {
                        if let Some(seat) = self.profile.seats.get_mut(user) {
                            seat.magnifier = settings;
                            self.status = "Magnifier settings saved to profile.".into();
                        }
                    }
                    Err(error) => self.status = format!("Magnifier settings rejected: {error:#}"),
                }
            }
            Some(crate::magnifier_ui::PageAction::Worker(command)) => {
                if let Some(session) = &self.session {
                    session.request(Request::Magnifier(command));
                    self.session_busy = true;
                }
            }
            None => (),
        }
    }
    fn picker_context_matches(&self, scope: &crate::gui_discovery::PickerScope, id: &str) -> bool {
        let mut allowed = self
            .profile
            .desktop
            .as_ref()
            .map(|desktop| desktop.allowed_pids.clone())
            .unwrap_or_default();
        allowed.sort_unstable();
        allowed.dedup();
        (self.session.is_some()
            || self.user_tab == UserTab::Lock
                && self.profile.seats.get(scope.user).is_some_and(|seat| {
                    seat.cursor.bounds_mode == mux_core::desktop::BoundsMode::Window
                }))
            && self.profile.desktop.is_some()
            && self.selected == Some(scope.user)
            && self.user_active(scope.user)
            && self
                .profile
                .seats
                .get(scope.user)
                .is_some_and(|seat| seat.identity() == id)
            && scope.allowed_pids == allowed
    }
    fn discovery_scope(&self) -> crate::gui_discovery::Scope {
        let mut readiness_pids: Vec<_> = self
            .profile
            .seats
            .iter()
            .map(|seat| seat.target_pid)
            .filter(|pid| *pid != 0)
            .collect();
        readiness_pids.sort_unstable();
        readiness_pids.dedup();
        crate::gui_discovery::Scope {
            readiness_pids,
            picker: self
                .discovery_picker
                .as_ref()
                .filter(|(scope, id)| self.picker_context_matches(scope, id))
                .map(|(scope, _)| scope.clone()),
        }
    }
    fn discovery_current(&self) -> bool {
        self.discovery_applied_generation != 0
            && self.discovery_applied_generation == self.discovery.latest_generation()
            && self.discovery.requested_scope().as_ref() == Some(&self.discovery_scope())
            && !self.discovery.status().worker_failed
    }
    fn refresh(&mut self) {
        self.discovery.request(self.discovery_scope());
        self.last_refresh = Instant::now();
        self.start_readiness = None;
    }
    fn poll_discovery(&mut self) {
        let picker_valid = self
            .discovery_picker
            .as_ref()
            .is_none_or(|(scope, id)| self.picker_context_matches(scope, id));
        if !picker_valid {
            self.discovery_picker = None;
            self.selection_windows.clear();
            self.selection_candidate = None;
            self.selection_highlight = None;
        }
        self.discovery.ensure(self.discovery_scope());
        if let Some(snapshot) = self.discovery.poll() {
            self.last_refresh = Instant::now();
            self.discovery_applied_generation = snapshot.generation;
            self.start_readiness = None;
            self.target_readiness = snapshot.readiness;
            match snapshot.devices {
                Ok(devices) => {
                    self.discovery_devices_ok = true;
                    self.pointers
                        .retain(|handle, _| devices.iter().any(|d| d.handle == *handle));
                    self.keyboard_packets
                        .retain(|handle, _| devices.iter().any(|d| d.handle == *handle));
                    self.devices = devices;
                    if self.session.is_some()
                        && self.profile.resolve(&self.devices).is_err()
                        && !self.session_busy
                    {
                        // Preserve the assigned-device disconnect Stop path; never attach from cached handles.
                        self.request_routing_stop(false);
                        self.status = "Assigned device disconnected; stopping routing.".into();
                    }
                }
                Err(error) => {
                    self.discovery_devices_ok = false;
                    self.status = format!("Device discovery: {error}");
                }
            }
            self.targets = snapshot.windows.unwrap_or_default();
            self.desktop_bounds = snapshot.desktop_bounds.ok();
            match snapshot.monitors {
                Ok(monitors) => {
                    self.monitors = monitors;
                    self.monitor_error = None;
                }
                Err(error) => {
                    self.monitor_error = Some(error);
                }
            }
            if let Some(scope) = snapshot.scope.picker
                && self
                    .discovery_picker
                    .as_ref()
                    .is_some_and(|(current, _)| *current == scope)
                && let Some(picker) = snapshot.picker
            {
                self.selection_highlight = None;
                match picker {
                    Ok(windows) => {
                        self.selection_windows = windows;
                        self.selection_error = None;
                        if self.selection_candidate.as_ref().is_some_and(|candidate| {
                            candidate.user != scope.user
                                || selection_snapshot_index(candidate, &self.selection_windows)
                                    .is_none()
                        }) {
                            self.selection_candidate = None;
                            self.selection_error = Some((scope.user, "The highlighted window changed or is unavailable. Choose a window again.".into()));
                        }
                    }
                    Err(error) => {
                        self.selection_windows.clear();
                        self.selection_candidate = None;
                        self.selection_error =
                            Some((scope.user, format!("Refresh windows: {error}")));
                    }
                }
            }
        }
        let status = self.discovery.status();
        self.app_request_handle.publish_discovery(status.clone());
        if let Some(script) = &self.script {
            script.discovery_state(&status);
        }
    }
    fn discovery_status_controls(&mut self, ui: &mut egui::Ui) {
        let status = self.discovery.status();
        let pending = status.in_flight || status.queued;
        let message = if status.worker_failed {
            "Discovery stopped. Restart the control center to retry.".to_owned()
        } else if pending {
            "Refreshing devices and applications…".to_owned()
        } else if let Some(age) = status.age_ms {
            format!("Last checked {} seconds ago", age / 1000)
        } else {
            "Waiting for device discovery…".to_owned()
        };
        let response = Text::new(&message).caption().wrap().show(ui);
        if let Some(script) = self.script.as_mut() {
            script.observe("Discovery status", &response);
            script.observe(
                if status.worker_failed {
                    "Discovery failed"
                } else if pending {
                    "Discovery refreshing"
                } else if status.completed_generation == 0 {
                    "Discovery waiting"
                } else if !status.errors.is_empty() {
                    "Discovery failed"
                } else {
                    "Discovery ready"
                },
                &response,
            );
        }
        for error in &status.errors {
            Text::new(error).caption().wrap().show(ui);
        }
    }
    fn routing(&mut self, ui: &mut egui::Ui) {
        let recovery = self.recovery.snapshot();
        if self.session.is_none() {
            let required = crate::session::start_target_pids(&self.profile);
            let virtual_only = crate::session::virtual_only(&self.profile);
            let error = if !self.discovery_current() {
                Some(if self.discovery.status().worker_failed {
                    "Discovery stopped; restart the control center.".into()
                } else {
                    "Checking devices and applications…".into()
                })
            } else if !self.discovery_devices_ok {
                Some("Device discovery failed; refresh devices before starting.".into())
            } else {
                required
                    .iter()
                    .find_map(|pid| match self.target_readiness.get(pid) {
                        Some(readiness) if readiness.can_start(virtual_only) => None,
                        Some(readiness) => Some(readiness.reason.clone()),
                        None => Some(format!("PID {pid} has not been checked yet")),
                    })
            };
            self.start_readiness = Some((required, virtual_only, error));
            let target_error = self
                .start_readiness
                .as_ref()
                .and_then(|(_, _, error)| error.as_deref());
            let targets_ready = target_error.is_none();
            let emergency_error = self.lifecycle.emergency_error();
            let emergency_ready = emergency_error.is_none();
            let shortcut_status = self.shortcut_status();
            if let Some(error) = target_error {
                if self.discovery_current() || self.discovery.status().worker_failed {
                    ui.add(egui::Label::new(RichText::new(error).color(COLORS[1])).wrap());
                } else {
                    Text::new(error).caption().wrap().show(ui);
                }
            }
            if recovery.phase == crate::session_recovery::Phase::Quarantined {
                let warning = bounded_display_text(
                    ui,
                    "Cleanup incomplete; restart affected apps",
                    ui.style().text_styles[&egui::TextStyle::Small].clone(),
                    ui.available_width(),
                );
                ui.colored_label(COLORS[1], warning)
                    .on_hover_text(recovery.error.as_deref().unwrap_or("Cleanup incomplete"));
            }
            if !shortcut_status.active {
                ui.horizontal_wrapped(|ui| {
                    Text::new("Shortcuts off").caption().show(ui).on_hover_text(
                        shortcut_status
                            .error
                            .as_deref()
                            .unwrap_or("Saved shortcuts are inactive"),
                    );
                    if self.home_button(
                        ui,
                        "Retry shortcuts",
                        !self.preferences_worker.pending()
                            && !self.preferences_worker.retiring()
                            && !self.preferences_quit_waiting,
                        ButtonVariant::Outline,
                    ) {
                        self.submit_preferences_action(
                            crate::gui_app_preferences::Kind::RetryShortcuts,
                        );
                    }
                });
            } else if emergency_error.is_some() {
                ui.horizontal_wrapped(|ui| {
                    Text::new("Stop unavailable").caption().wrap().show(ui);
                    if self.home_button(
                        ui,
                        "Retry emergency stop",
                        !self.preferences_worker.pending()
                            && !self.preferences_worker.retiring()
                            && !self.preferences_quit_waiting,
                        ButtonVariant::Outline,
                    ) {
                        self.submit_preferences_action(
                            crate::gui_app_preferences::Kind::RetryEmergency,
                        );
                    }
                });
            }
            let graph_ready = self.profile.replication.resolve(&self.profile.seats);
            if let Err(error) = &graph_ready {
                ui.colored_label(COLORS[1], "Repair the replication graph before starting")
                    .on_hover_text(format!("{error:#}"));
            }
            ui.horizontal_wrapped(|ui| {
                if self.prominent_button(
                    ui,
                    "Start routing",
                    graph_ready.is_ok()
                        && emergency_ready
                        && shortcut_status.active
                        && targets_ready
                        && recovery.can_start
                        && !self.quit_after_stop
                        && !self.allow_close
                        && !self.close_ready
                        && !self.preferences_worker.pending()
                        && !self.preferences_worker.retiring()
                        && !self.preferences_worker.degraded()
                        && !self.preferences_quit_waiting
                        && !self.profile_file.pending()
                        && !self.profile_file_ui.initial_unresolved
                        && !self.profile_file_ui.quit_waiting,
                    None,
                ) {
                    self.start_routing();
                }
                if self.restart_requested
                    && self.home_button(ui, "Stop routing", true, ButtonVariant::Outline)
                {
                    self.request_routing_stop(false);
                    self.status = "Pending Start cancelled; Agent Access cleanup continues.".into();
                }
                if recovery.phase == crate::session_recovery::Phase::Quarantined
                    && self.home_button(ui, "Recovery details", true, ButtonVariant::Outline)
                {
                    self.recovery_dialog_open = true;
                }
                let secondary = Theme::get(ui).muted_foreground;
                ui.label(
                    RichText::new(if self.session_busy {
                        "Working…"
                    } else {
                        "Stopped"
                    })
                    .color(secondary),
                );
            });
        } else {
            ui.horizontal_wrapped(|ui| {
                if self.home_button(
                    ui,
                    if self.session_paused {
                        "Resume"
                    } else {
                        "Pause"
                    },
                    self.session_ready && !self.session_busy && !self.session_stopping,
                    ButtonVariant::Outline,
                ) {
                    self.session
                        .as_ref()
                        .unwrap()
                        .request(if self.session_paused {
                            Request::Resume
                        } else {
                            Request::Pause
                        });
                    self.session_busy = true;
                }
                if self.prominent_button(ui, "Stop routing", true, Some(ButtonVariant::Destructive))
                {
                    self.request_routing_stop(false);
                }
                let secondary = Theme::get(ui).muted_foreground;
                ui.label(
                    RichText::new(if self.session_stopping {
                        "Stopping…"
                    } else if !self.session_ready {
                        "Starting…"
                    } else if self.session_busy {
                        "Working…"
                    } else if self.session_paused {
                        "Paused"
                    } else {
                        "Routing active"
                    })
                    .color(secondary),
                );
            });
        }
    }

    fn start_routing(&mut self) {
        self.restart_requested = false;
        if self.preferences_worker.pending()
            || self.preferences_worker.retiring()
            || self.preferences_worker.degraded()
            || self.preferences_quit_waiting
        {
            self.status = "Resolve the application preference operation before starting.".into();
            return;
        }
        if self.profile_file.pending()
            || self.profile_file_ui.initial_unresolved
            || self.profile_file_ui.quit_waiting
        {
            self.status = "Resolve the profile file operation before starting.".into();
            return;
        }
        if self.session.is_some() {
            return;
        }
        if !self.discovery_current() || !self.discovery_devices_ok {
            self.status = "Wait for device and application discovery before starting.".into();
            return;
        }
        if !self.recovery.snapshot().can_start
            || self.quit_after_stop
            || self.allow_close
            || self.close_ready
        {
            self.status =
                "Restart the affected applications before starting another session.".into();
            return;
        }
        let virtual_only = crate::session::virtual_only(&self.profile);
        if !self.shortcut_status().active
            || self.lifecycle.emergency_error().is_some()
            || crate::session::start_target_pids(&self.profile)
                .iter()
                .any(|pid| {
                    !self
                        .target_readiness
                        .get(pid)
                        .is_some_and(|readiness| readiness.can_start(virtual_only))
                })
        {
            self.status =
                "Start cancelled. Check targets and stop controls before starting again.".into();
            return;
        }
        self.stop_all_sdks();
        if !self.all_sdks_retired()
            || !matches!(
                self.agent_broker.status().phase,
                crate::gui_agent_broker::Phase::Stopped
            )
        {
            if self.setup_handle.enabled()
                || self.setup_pending.is_some()
                || self.setup_page.descriptor.is_some()
            {
                self.setup_retiring = true;
            }
            self.setup_handle.revoke();
            self.setup_pending = None;
            self.setup_page.pending = false;
            self.setup_page.descriptor = None;
            self.app_request_handle.revoke();
            self.agent_pending_enable = None;
            self.restart_requested = true;
            self.restart_discovery_generation = self.discovery_applied_generation;
            self.agent_broker.request_stop(self.agent_session_epoch);
            self.status =
                "Waiting for previous Agent Access cleanup before starting routing.".into();
            return;
        }
        self.restart_requested = false;
        self.setup_handle.revoke();
        self.setup_page.descriptor = None;
        self.app_request_handle.revoke();
        if let Err(error) = self.profile.replication.resolve(&self.profile.seats) {
            self.status = format!("Repair the replication graph before starting: {error:#}");
            return;
        }
        let broker_before_controller = self.agent_broker.status();
        match Controller::start_background_with_options(
            self.profile.clone(),
            self.runtime.clone().into(),
            crate::session::StartOptions {
                app_hotkeys: true,
                preflight_delay: self.start_preflight_delay,
                emergency_check: Some(self.lifecycle.emergency_check()),
            },
        ) {
            Ok(session) => {
                self.agent_session_epoch = self.agent_session_epoch.saturating_add(1);
                self.setup_start_order =
                    self.setup_retired_generation
                        .take()
                        .map(|retired_generation| SetupStartOrder {
                            retired_broker_generation: retired_generation,
                            broker_generation_before_controller: broker_before_controller
                                .generation,
                            broker_phase_before_controller: broker_phase_label(
                                &broker_before_controller.phase,
                            )
                            .to_owned(),
                            controller_epoch: self.agent_session_epoch,
                        });
                initialize_controller_agent_access(
                    &mut self.agent_page,
                    &mut self.app_request_handle,
                    self.agent_session_epoch,
                );
                self.cursor_settings.reset(&self.profile.seats);
                self.advance_macro_file_session();
                self.startup_window_selections = self
                    .staged_window_locks
                    .take_for_start(&self.profile)
                    .into();
                self.startup_window_selection_active = !self.startup_window_selections.is_empty();
                self.startup_window_selection_error = None;
                self.attachment = AttachmentNotice::default();
                self.recovery = session.recovery();
                self.recovery_bridge_cursor = None;
                self.recovery_dialog_open = false;
                self.user_pause.reset();
                self.session = Some(session);
                self.virtual_users_base_seat_count = Some(self.profile.seats.len());
                self.virtual_users_page.reset_session();
                self.session_ready = false;
                self.session_paused = false;
                self.session_busy = true;
                self.session_stopping = false;
                self.status = "Checking devices and applications.".into();
            }
            Err(error) => self.status = format!("{error:#}"),
        }
    }

    fn continue_startup_window_selection(&mut self) {
        if self.session_stopping || self.session.is_none() {
            return;
        }
        if let Some(mut selection) = self.startup_window_selections.pop_front() {
            selection.issue();
            self.session
                .as_ref()
                .expect("controller checked")
                .request(Request::SelectWindow(selection.clone()));
            self.selection_pending = Some(selection);
            self.session_ready = false;
            self.session_busy = true;
            self.status = "Checking staged windows before routing is ready.".into();
        } else {
            self.startup_window_selection_active = false;
            self.session_ready = true;
            self.session_busy = false;
            self.status = format!(
                "Routing ready for {} users; staged windows selected.",
                self.profile.seats.len()
            );
        }
    }
    fn fail_startup_window_selection(&mut self, error: String) {
        self.startup_window_selection_error = Some(error);
        self.request_routing_stop(false);
    }

    fn request_routing_stop(&mut self, quit_after: bool) {
        self.user_pause.reset();
        self.stop_all_sdks();
        self.restart_requested = false;
        if self.session.is_none() && self.setup_handle.enabled() {
            self.setup_retiring = true;
            self.setup_handle.revoke();
            self.setup_pending = None;
            self.setup_page.pending = false;
            self.setup_page.descriptor = None;
        }
        if quit_after {
            self.profile_exit.begin();
            self.app_request_handle.revoke();
            self.agent_pending_enable = None;
        }
        self.advance_macro_file_session();
        self.staged_window_locks.clear();
        self.startup_window_selections.clear();
        self.startup_window_selection_active = false;
        self.selection_pending = None;
        if self.recovery.snapshot().phase == crate::session_recovery::Phase::Quarantined {
            self.status = "Cleanup failed. Restart the affected applications to recover.".into();
            if quit_after {
                self.agent_broker.request_stop(self.agent_session_epoch);
                self.recovery_dialog_open = true;
            }
            return;
        }
        if let Some(session) = &self.session {
            let shared = session.shared_handle();
            self.app_request_handle.begin_stop();
            let broker_epoch = self.agent_session_epoch;
            self.agent_pending_enable = None;
            stop_and_latch_before_broker_cleanup(
                || shared.request(Request::Stop),
                || {
                    latch_routing_stopping(
                        &mut self.session_ready,
                        &mut self.session_busy,
                        &mut self.session_stopping,
                        &mut self.quit_after_stop,
                        quit_after,
                        &mut self.status,
                    );
                },
                || {
                    if quit_after || !self.app_request_handle.continuing() {
                        self.agent_broker.request_stop(broker_epoch);
                    }
                },
            );
            for command_id in std::mem::take(&mut self.mapper_commands) {
                self.mapper_page.request_failed(
                    command_id,
                    "Routing is stopping; this mapper command was cancelled.",
                );
            }
            self.mapper_owns_session_busy = false;
        } else {
            self.agent_pending_enable = None;
            if quit_after || !self.app_request_handle.continuing() {
                self.agent_broker.request_stop(self.agent_session_epoch);
            }
            if quit_after {
                self.quit_after_stop = true;
                if matches!(
                    self.agent_broker.status().phase,
                    crate::gui_agent_broker::Phase::Stopped
                ) {
                    self.close_ready = self.all_sdks_retired();
                }
            }
        }
    }

    fn poll_failed_cleanup(&mut self) {
        let published = self.recovery.published();
        let ticket = self.app_request_handle.publication_ticket();
        let cursor = self
            .recovery_bridge_cursor
            .get_or_insert_with(|| RecoveryBridgeCursor::new(published.generation, ticket));
        if let Some((sample_ticket, sample)) = cursor.next_fresh(&published, ticket) {
            self.app_request_handle.publish_recovery_polled(
                sample_ticket,
                sample.disclosure,
                sample.poll_started,
            );
        }
        let recovered = published.snapshot;
        let was_quarantined = self.recovery_phase == crate::session_recovery::Phase::Quarantined;
        self.recovery_phase = recovered.phase;
        if recovered.can_start && was_quarantined {
            self.status =
                "Old target processes exited. Select restarted applications before routing again."
                    .into();
            self.refresh();
        }
    }

    fn recovery_dialog(&mut self, ctx: &egui::Context) {
        if !self.recovery_dialog_open {
            return;
        }
        let snapshot = self.recovery.snapshot();
        let width = (ctx.content_rect().width() - 80.0).clamp(160.0, 420.0);
        let height = (ctx.content_rect().height() - 190.0).max(60.0);
        let mut close = false;
        let dismiss = Dialog::new("Routing recovery").id_source("routing_recovery").show(ctx, |ui| {
            ui.set_width(width);
            egui::ScrollArea::vertical().max_height(height).show(ui, |ui| {
                Text::new(if snapshot.can_start {
                    "Affected applications have exited. Select fresh applications, then start routing when ready."
                } else {
                    "Close and restart the affected applications below. Routing stays stopped until their original processes exit."
                }).wrap().show(ui);
                for target in &snapshot.targets {
                    let status = match target.status {
                        crate::session_recovery::TargetStatus::Running => "Still running",
                        crate::session_recovery::TargetStatus::Exited => "Exited",
                        crate::session_recovery::TargetStatus::Unwatched => "Exit unconfirmed",
                    };
                    let application = target
                        .application_name
                        .as_deref()
                        .map_or_else(|| format!("Application {}", target.pid), |name| {
                            format!("{name} · {}", target.pid)
                        });
                    Text::new(format!("{application} · {status}")).wrap().show(ui);
                }
                if let Some(error) = &snapshot.error {
                    let details = Collapsible::new("Technical details")
                        .id_source("routing-recovery-technical-details")
                        .default_open(false)
                        .show(ui, |ui| {
                            Text::new(error).wrap().show(ui);
                        });
                    if let Some(script) = self.script.as_mut() {
                        script.observe("Recovery technical details", &details);
                    }
                }
            });
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                if self.home_button(ui, "Check recovery", true, ButtonVariant::Outline) {
                    self.poll_failed_cleanup();
                }
                close = self.home_button(ui, "Close recovery", true, ButtonVariant::Default);
            });
        });
        if close || dismiss {
            self.recovery_dialog_open = false;
        }
    }

    fn input_release_dialog(&mut self, ctx: &egui::Context) {
        if !self.input_release_dialog_open {
            return;
        }
        let pending: Vec<_> = self
            .states
            .iter()
            .enumerate()
            .filter_map(|(index, state)| {
                let count = state.pending_releases.unwrap_or(0);
                (count > 0).then(|| {
                    let name = self
                        .profile
                        .seats
                        .get(index)
                        .map_or_else(|| state.name.clone(), |seat| seat.name.clone());
                    (name, count, state.status.clone())
                })
            })
            .collect();
        let width = (ctx.content_rect().width() - 80.0).clamp(160.0, 420.0);
        // Keep the title, explanatory copy, and wrapped button footer outside
        // the scroll area so they remain visible even at compact window sizes.
        let body_height = (ctx.content_rect().height() - 230.0).clamp(64.0, 360.0);
        let mut close = false;
        let dismiss = Dialog::new("Input release details")
            .id_source("input-release-details")
            .show(ctx, |ui| {
                ui.set_width(width);
                egui::ScrollArea::vertical()
                    .max_height(body_height)
                    .show(ui, |ui| {
                        Text::new("Saved releases retry automatically for their original users.")
                            .wrap()
                            .show(ui);
                        if pending.is_empty() {
                            ui.add_space(8.0);
                            ui.weak("No input releases are pending.");
                        }
                        for (name, count, status) in &pending {
                            ui.add_space(8.0);
                            let release_label = if *count == 1 {
                                "pending release"
                            } else {
                                "pending releases"
                            };
                            ui.strong(format!("{name} · {count} {release_label}"));
                            if !status.trim().is_empty() {
                                let redundant = format!("{count} release(s) pending: ");
                                let detail = status.strip_prefix(&redundant).unwrap_or(status);
                                let visible: String = detail.chars().take(240).collect();
                                ui.add(egui::Label::new(visible).truncate())
                                    .on_hover_text(status);
                            }
                        }
                    });
                ui.add_space(8.0);
                Text::new("Release all input attempts releases for every virtual user.")
                    .caption()
                    .wrap()
                    .show(ui);
                ui.horizontal_wrapped(|ui| {
                    let enabled = self.session_ready
                        && self.session.is_some()
                        && !self.session_busy
                        && !self.session_stopping;
                    if self.home_button(ui, "Release all input", enabled, ButtonVariant::Outline) {
                        self.session.as_ref().unwrap().request(Request::ReleaseAll);
                        self.session_busy = true;
                    }
                    close =
                        self.home_button(ui, "Close release details", true, ButtonVariant::Default);
                });
            });
        if close || dismiss {
            self.input_release_dialog_open = false;
        }
    }

    fn set_live_mode(&mut self, mode: mux_core::routing::Mode, multi_keyboard: bool) {
        if self.profile.desktop.is_none() {
            self.status = "Routing mode shortcuts require automatic desktop routing.".into();
            return;
        }
        if self.session.is_none() {
            self.status =
                "Mode and keyboard shortcuts require a ready automatic routing session.".into();
            return;
        }
        let options = self.profile.desktop.as_ref().expect("checked above");
        if !self.session_ready || self.session_busy || self.session_stopping {
            self.status =
                "Wait for automatic desktop routing to be ready before changing mode.".into();
            return;
        }
        if options.mode == mode && options.multi_keyboard == multi_keyboard {
            return;
        }
        if let Some(session) = &self.session {
            session.request(Request::Mode {
                mode,
                multi_keyboard,
            });
            self.session_busy = true;
        }
    }

    fn handle_lifecycle_action(
        &mut self,
        action: crate::app_lifecycle::LifecycleAction,
        ctx: &egui::Context,
    ) {
        use crate::app_lifecycle::LifecycleAction as Action;
        if let Some(trace) = &mut self.lifecycle_trace {
            trace.record(action, ctx.cumulative_frame_nr());
        }
        match action {
            Action::Restore => {
                self.restore_started_frame = Some(ctx.cumulative_frame_nr());
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            Action::Stop => self.request_routing_stop(false),
            Action::EmergencyUnavailable => {
                self.request_routing_stop(false);
                self.status = "Emergency stop unavailable; routing is stopping. Retry emergency stop before starting again.".into();
            }
            Action::Quit => {
                if self.setup_handle.enabled()
                    || self.setup_pending.is_some()
                    || self.setup_page.descriptor.is_some()
                {
                    self.setup_retiring = true;
                }
                self.setup_handle.revoke();
                self.setup_pending = None;
                self.setup_page.pending = false;
                self.setup_page.descriptor = None;
                self.app_request_handle.revoke();
                let waiting = self
                    .profile_file_ui
                    .request_quit(self.profile_file.pending());
                self.request_routing_stop(true);
                if self.recovery.snapshot().phase == crate::session_recovery::Phase::Quarantined {
                    self.handle_lifecycle_action(Action::Restore, ctx);
                } else {
                    self.preferences_quit_waiting = true;
                    if waiting || self.preferences_worker.pending() {
                        self.handle_lifecycle_action(Action::Restore, ctx);
                    }
                }
            }
            Action::ToggleRouting => {
                if self.session.is_some() {
                    self.request_routing_stop(false);
                } else {
                    self.start_routing();
                }
            }
            Action::PauseResume => {
                if self.session_ready
                    && !self.session_busy
                    && !self.session_stopping
                    && let Some(session) = &self.session
                {
                    session.request(if self.session_paused {
                        Request::Resume
                    } else {
                        Request::Pause
                    });
                    self.session_busy = true;
                }
            }
            Action::ToggleMode => {
                if let Some(options) = self.profile.desktop.as_ref() {
                    let mode = match options.mode {
                        mux_core::routing::Mode::Switched => mux_core::routing::Mode::Concurrent,
                        mux_core::routing::Mode::Concurrent => mux_core::routing::Mode::Switched,
                    };
                    self.set_live_mode(mode, options.multi_keyboard);
                } else {
                    self.status = "Mode shortcuts require automatic desktop routing.".into();
                }
            }
            Action::Switched | Action::Concurrent => {
                let mode = if action == Action::Switched {
                    mux_core::routing::Mode::Switched
                } else {
                    mux_core::routing::Mode::Concurrent
                };
                let multi_keyboard = self
                    .profile
                    .desktop
                    .as_ref()
                    .map(|options| options.multi_keyboard)
                    .unwrap_or(false);
                self.set_live_mode(mode, multi_keyboard);
            }
            Action::ToggleKeyboard => {
                if let Some(options) = self.profile.desktop.as_ref() {
                    self.set_live_mode(options.mode, !options.multi_keyboard);
                } else {
                    self.status = "Keyboard shortcuts require automatic desktop routing.".into();
                }
            }
        }
    }

    fn poll_lifecycle_actions(&mut self, ctx: &egui::Context) {
        let actions: Vec<_> = self.lifecycle_actions.try_iter().take(32).collect();
        for action in actions {
            self.handle_lifecycle_action(action, ctx);
        }
    }

    fn desktop_controls(&mut self, ui: &mut egui::Ui) {
        use mux_core::routing::{DesktopRouting, Mode as RoutingMode};
        let mut automatic = self.profile.desktop.is_some();
        let response = Checkbox::new(&mut automatic)
            .label("Automatic window targeting")
            .enabled(self.session.is_none())
            .show(ui);
        let accepted = self
            .script
            .as_mut()
            .is_none_or(|script| script.observe("Automatic window targeting", &response));
        if response.changed()
            && accepted
            && self.consume_action(ui.ctx(), "Automatic window targeting")
        {
            self.profile.desktop = automatic.then(|| DesktopRouting {
                mode: RoutingMode::Switched,
                ..Default::default()
            });
            if automatic {
                self.profile.concurrent = true;
                for seat in &mut self.profile.seats {
                    seat.cursor.lock_window = false;
                }
            }
        }
        if let Some(options) = self.profile.desktop.clone() {
            for (label, mode) in [
                ("Switched", RoutingMode::Switched),
                ("Concurrent", RoutingMode::Concurrent),
            ] {
                let enabled = self.session.is_none() || self.session_ready && !self.session_busy;
                let response = ouroboros_ui::atoms::Radio::new(mode == options.mode)
                    .label(label)
                    .enabled(enabled)
                    .sm()
                    .show(ui);
                let accepted = self
                    .script
                    .as_mut()
                    .is_none_or(|script| script.observe(label, &response));
                if response.clicked() && accepted && self.consume_action(ui.ctx(), label) {
                    if let Some(session) = &self.session {
                        session.request(Request::Mode {
                            mode,
                            multi_keyboard: options.multi_keyboard,
                        });
                        self.session_busy = true;
                    } else {
                        self.profile.desktop.as_mut().unwrap().mode = mode;
                    }
                }
            }
            let mut multi = options.multi_keyboard;
            let response = Checkbox::new(&mut multi)
                .label("Multi keyboard")
                .enabled(self.session.is_none() || self.session_ready && !self.session_busy)
                .show(ui);
            let accepted = self
                .script
                .as_mut()
                .is_none_or(|script| script.observe("Multi keyboard", &response));
            if response.changed() && accepted && self.consume_action(ui.ctx(), "Multi keyboard") {
                if let Some(session) = &self.session {
                    session.request(Request::Mode {
                        mode: options.mode,
                        multi_keyboard: multi,
                    });
                    self.session_busy = true;
                } else {
                    self.profile.desktop.as_mut().unwrap().multi_keyboard = multi;
                }
            }
        }
    }
    fn poll_session(&mut self, ctx: &egui::Context) {
        let events: Vec<_> = self
            .session
            .as_ref()
            .map(|s| drain_session_events(&s.events))
            .unwrap_or_default();
        if events.len() == SESSION_EVENTS_PER_FRAME {
            ctx.request_repaint();
        }
        for event in events {
            // Stop is latched for this owned controller. Late readiness or command
            // acknowledgements must not reopen its UI before terminal cleanup.
            if self.session_stopping
                && !matches!(event, SessionEvent::Stopped | SessionEvent::Failed(_))
            {
                continue;
            }
            if self.session.is_none()
                && matches!(
                    &event,
                    SessionEvent::UserPause(_)
                        | SessionEvent::UserPauseStates(_)
                        | SessionEvent::UserPauseAcknowledged { .. }
                )
            {
                continue;
            }
            self.attachment.receive(&event);
            let cursor_completion = self
                .cursor_settings
                .receive(&event, &mut self.profile.seats);
            let periodic_automation = matches!(
                &event,
                SessionEvent::Automation(state) if !state.acknowledged
            );
            let mapper_reply = matches!(&event, SessionEvent::Automation(state) if state.mapper.reply_to.is_some());
            let periodic_annotation = matches!(
                &event,
                SessionEvent::Annotation(state) if !state.acknowledged
            );
            let periodic_magnifier = matches!(
                &event,
                SessionEvent::Magnifier(state) if !state.acknowledged
            );
            let periodic_macro_studio = matches!(
                &event,
                SessionEvent::MacroStudio(state) if !state.acknowledged
            );
            let periodic_virtual_users = matches!(
                &event,
                SessionEvent::VirtualUsers(state) if !state.acknowledged
            );
            let output_event = matches!(&event, SessionEvent::Output(_));
            let remote_agent_event = matches!(
                &event,
                SessionEvent::Agent(state) if state.client != 0 || !state.acknowledged
            );
            if !matches!(
                &event,
                SessionEvent::State(_)
                    | SessionEvent::SettingsAcknowledged(_)
                    | SessionEvent::UserPause(_)
                    | SessionEvent::UserPauseStates(_)
                    | SessionEvent::UserPauseAcknowledged { .. }
                    | SessionEvent::WindowSelected(_)
                    | SessionEvent::Attaching(_)
                    | SessionEvent::AttachmentFinished { .. }
                    | SessionEvent::EmergencyUnavailable(_)
            ) && !self.cursor_settings.pending()
                && !periodic_automation
                && !mapper_reply
                && !periodic_annotation
                && !periodic_magnifier
                && !periodic_macro_studio
                && !periodic_virtual_users
                && !output_event
                && !remote_agent_event
            {
                self.session_busy = false;
            }
            match event {
                SessionEvent::UserPause(state) => {
                    self.user_pause.publish(state);
                }
                SessionEvent::UserPauseStates(states) => {
                    self.user_pause.states = states;
                }
                SessionEvent::UserPauseAcknowledged { ticket, state } => {
                    if self
                        .user_pause
                        .acknowledge(&ticket, &state, &self.profile.seats)
                    {
                        self.status = state.error.as_ref().map_or_else(
                            || {
                                if state.paused {
                                    "Selected user paused.".into()
                                } else {
                                    "Selected user resumed.".into()
                                }
                            },
                            |error| format!("User pause request failed: {error}"),
                        );
                        if let Some(script) = &self.script {
                            script.user_pause_ack(&ticket, &state);
                        }
                    }
                }
                SessionEvent::SettingsAcknowledged(_) => {
                    if let Some(result) = cursor_completion {
                        self.session_busy = false;
                        self.status = result.map_or_else(
                            |error| format!("Cursor settings not applied: {error}"),
                            |()| "Cursor settings applied.".into(),
                        );
                    }
                }
                SessionEvent::WindowSelected(acknowledgement) => {
                    if selection_ack_matches(self.selection_pending.as_ref(), &acknowledgement) {
                        self.selection_pending = None;
                        self.selection_error = acknowledgement
                            .error
                            .as_ref()
                            .map(|error| (acknowledgement.selection.user, error.clone()));
                        if let Some(script) = &self.script {
                            script.window_selection_ack(&acknowledgement);
                        }
                        if self.startup_window_selection_active {
                            if let Some(error) = acknowledgement.error {
                                self.fail_startup_window_selection(format!(
                                    "Staged window was rejected: {error}"
                                ));
                            } else {
                                self.continue_startup_window_selection();
                            }
                        } else {
                            self.session_busy = false;
                            self.status = acknowledgement.error.as_ref().map_or_else(
                                || "Window selection acknowledged.".into(),
                                |error| format!("Window selection failed: {error}"),
                            );
                        }
                    } else if self.startup_window_selection_active {
                        self.fail_startup_window_selection(
                            "Startup window acknowledgement did not match the current request."
                                .into(),
                        );
                    }
                }
                SessionEvent::State(states) => {
                    if let Some(script) = &self.script {
                        script.state(&states);
                    }
                    self.sync_keyboard_settings(&states);
                    self.states = states;
                }
                SessionEvent::Attaching(_) => {}
                SessionEvent::AttachmentFinished { .. } => {}
                SessionEvent::ModeChanged(mode, multi_keyboard) => {
                    if let Some(options) = &mut self.profile.desktop {
                        options.mode = mode;
                        options.multi_keyboard = multi_keyboard;
                    }
                    self.status = format!("{mode:?} routing · held input released");
                }
                SessionEvent::Configured(count) => {
                    if self.startup_window_selection_active {
                        self.continue_startup_window_selection();
                    } else {
                        self.session_ready = true;
                        self.status = format!("Routing ready for {count} users.");
                    }
                }
                SessionEvent::Paused => {
                    self.session_paused = true;
                    self.status = "Pause requests sent.".into();
                }
                SessionEvent::Resumed => {
                    self.session_paused = false;
                    self.status = "Resume requests sent.".into();
                }
                SessionEvent::Stopped => {
                    self.app_request_handle.terminal();
                    self.agent_pending_enable = None;
                    if self.quit_after_stop || !self.app_request_handle.continuing() {
                        self.agent_broker.request_stop(self.agent_session_epoch);
                    }
                    self.advance_macro_file_session();
                    self.reset_window_selection();
                    self.session_stopping = false;
                    self.input_release_dialog_open = false;
                    if !self.app_request_handle.continuing() {
                        self.agent_page.reset_session();
                    }
                    self.session_ready = false;
                    self.states.clear();
                    self.cursor_settings.reset(&[]);
                    self.user_pause.reset();
                    self.session_busy = false;
                    self.annotation_page.session_stopped();
                    self.output_page.session_stopped();
                    self.magnifier_page.session_stopped();
                    self.screen_page.reset_session();
                    self.screen_targets.clear();
                    self.macro_studio_page.reset_session();
                    self.reset_virtual_users_session();
                    self.replication_page.session_stopped();
                    self.forwarding_page.session_stopped();
                    self.reset_mapper_session();
                    self.replication_revision = 0;
                    self.session = None;
                    if self.quit_after_stop {
                        self.close_ready = self.all_sdks_retired()
                            && crate::gui_agent_broker::quit_ready(
                                self.session.is_none(),
                                &self.agent_broker.status().phase,
                            );
                    }
                    self.startup_window_selections.clear();
                    self.startup_window_selection_active = false;
                    self.staged_window_locks.clear();
                    self.status = self.startup_window_selection_error.take().map_or_else(
                        || "Routing stopped. Compatible virtual targets can be used again.".into(),
                        |error| {
                            format!(
                                "{error} Routing stopped; choose the window again before Start."
                            )
                        },
                    );
                    if !matches!(
                        self.agent_broker.status().phase,
                        crate::gui_agent_broker::Phase::Stopped
                    ) {
                        self.status = if self.app_request_handle.continuing() {
                            "Routing stopped; existing agents can inspect recovery.".into()
                        } else {
                            "Routing stopped; Agent Access cleanup is pending.".into()
                        };
                    }
                    self.start_readiness = None;
                    self.target_readiness.clear();
                }
                SessionEvent::InputSent => {
                    self.status = "Emulated input sent through ProtoInput.".into();
                }
                SessionEvent::EmergencyUnavailable(error) => {
                    self.status = error;
                }
                SessionEvent::InputRejected(error) => {
                    // Saved releases remain retryable after this nonterminal failure.
                    self.session_busy = false;
                    self.status = format!("Input release incomplete: {error}");
                }
                SessionEvent::CommandRejected(error) => {
                    // Prevalidation ran before effects; keep the worker and drafts live.
                    self.session_busy = false;
                    self.status = format!("Change not applied: {error}");
                }
                SessionEvent::MapperMonitors(receipt) => {
                    self.mapper_page.receive_monitors(&receipt);
                }
                SessionEvent::Automation(state) => {
                    self.receive_mapper_state(&state.mapper, MapperPublisher::Direct);
                    if state.mapper.reply_to.is_some() {
                        // A mapper acknowledgement is not a legacy graph/macro command receipt.
                        continue;
                    }
                    if self.macro_file.pending()
                        && macro_file_conflicts_with_automation(
                            &state,
                            self.macro_file_observed_revision,
                            self.macro_recording,
                            self.macro_loaded_events,
                            self.macro_playback.as_ref().map(|play| play.state.as_str()),
                        )
                    {
                        // An acknowledged Inspect carrying a clip is conservatively treated as
                        // a possible remote Load: the legacy ACK has no command identity.
                        self.invalidate_macro_file();
                    }
                    self.macro_file_observed_revision =
                        self.macro_file_observed_revision.max(state.macro_revision);
                    if let Some(script) = &self.script {
                        script.graph_ack(&state);
                        script.forwarding_ack(&state);
                    }
                    if let Some(definition) =
                        self.forwarding_page.receive(&state, &self.profile.seats)
                    {
                        self.profile.forwarding = definition;
                    }
                    if let Some(script) = &self.script {
                        script.forwarding_state(&self.forwarding_page.snapshot());
                    }
                    let graph_is_current = state.graph_revision >= self.replication_revision;
                    self.replication_revision = self.replication_revision.max(state.graph_revision);
                    let graph_was_pending = self.replication_page.snapshot().pending;
                    if let Some(definition) =
                        self.replication_page.receive(&state, &self.profile.seats)
                    {
                        self.profile.replication = definition;
                    } else if graph_is_current
                        && !graph_was_pending
                        && state.acknowledged
                        && state.error.is_none()
                        && let Ok(definition) = mux_core::replication::Definition::from_resolved(
                            &self.profile.seats,
                            &state.edges,
                        )
                    {
                        self.profile.replication = definition;
                        self.replication_page
                            .sync_applied(&self.profile.replication);
                    }
                    self.macro_recording = state.recording;
                    self.macro_recorded_events = state.recorded_events;
                    self.macro_loaded_events = state.loaded_events;
                    self.macro_playback = state.playback;
                    if graph_is_current
                        && (!self.replication_dirty
                            || edge_lists_match(&state.edges, &self.replication_edges))
                    {
                        self.replication_edges = state.edges;
                        self.replication_dirty = false;
                    }
                    if let Some(clip) = state.clip {
                        self.macro_clip = Some(Arc::new(clip));
                    } else if state.loaded_events == 0 {
                        self.macro_clip = None;
                    }
                    if state.acknowledged || state.error.is_some() {
                        self.macro_error = state.error;
                    }
                    if state.acknowledged {
                        if let Some(error) = &self.macro_error {
                            self.status = error.clone();
                        } else {
                            self.status = "Automation state updated.".into();
                        }
                    }
                }
                SessionEvent::Output(state) => {
                    let acknowledged = state.acknowledged;
                    let error = state.error.clone();
                    let owns_ack = self.output_page.owns_ack(&state);
                    if let Some(script) = &self.script {
                        script.output_state(&state);
                    }
                    let committed = self.output_page.receive(state.clone());
                    if owns_ack {
                        self.session_busy = false;
                    }
                    if let Some((user, settings)) = committed
                        && let Some(seat) = self.profile.seats.get_mut(user)
                    {
                        seat.output = settings;
                    }
                    if acknowledged && owns_ack {
                        self.status = error.map_or_else(
                            || "Output rules applied to the virtual user.".into(),
                            |error| format!("Output rules rejected: {error}"),
                        );
                    }
                }
                SessionEvent::Annotation(state) => {
                    if let Some(script) = &self.script {
                        script.annotation_state(&state);
                    }
                    let acknowledged = state.acknowledged;
                    let error = state.error.clone();
                    if let Some((user, settings)) = self.annotation_page.receive(state)
                        && let Some(seat) = self.profile.seats.get_mut(user)
                    {
                        seat.annotation = settings;
                    }
                    if acknowledged {
                        if let Some(error) = error {
                            self.status = format!("Annotation command failed: {error}");
                        } else {
                            self.status = "Annotation settings or command acknowledged.".into();
                        }
                    }
                }
                SessionEvent::Magnifier(state) => {
                    if let Some(script) = &self.script {
                        script.magnifier_state(&state);
                    }
                    let acknowledged = state.acknowledged;
                    let error = state.error.clone();
                    if let Some((user, settings)) = self.magnifier_page.receive(state)
                        && let Some(seat) = self.profile.seats.get_mut(user)
                    {
                        seat.magnifier = settings;
                    }
                    if acknowledged {
                        self.status = error.map_or_else(
                            || "Magnifier command acknowledged.".into(),
                            |error| format!("Magnifier command failed: {error}"),
                        );
                    }
                }
                SessionEvent::MacroStudio(state) => {
                    if self.macro_file.pending() && macro_file_conflicts_with_studio(&state) {
                        // Studio ACKs do not identify Inspect versus mutation. Discard a pending
                        // file Load rather than overwrite a possible remote studio command.
                        self.invalidate_macro_file();
                    }
                    if let Some(script) = &self.script {
                        script.macro_studio_state(&state);
                    }
                    let acknowledged = state.acknowledged;
                    let error = state.error.clone();
                    self.macro_studio_page.receive(state);
                    if acknowledged {
                        self.status = error.map_or_else(
                            || "Macro Studio command acknowledged.".into(),
                            |error| format!("Macro Studio command failed: {error}"),
                        );
                    }
                }
                SessionEvent::VirtualUsers(state) => {
                    self.app_request_handle.observe_roster(&state);
                    let acknowledged = state.acknowledged;
                    let error = state.error.clone();
                    self.merge_virtual_users_roster(&state);
                    let ack_state = state.clone();
                    self.virtual_users_page.receive(state);
                    if let Some(script) = &self.script {
                        script.virtual_users_state(&self.virtual_users_page.snapshot());
                        script.virtual_users_ack(&ack_state);
                    }
                    if acknowledged {
                        self.status = error.map_or_else(
                            || "Virtual user command acknowledged.".into(),
                            |error| format!("Virtual user command failed: {error}"),
                        );
                    }
                }
                SessionEvent::Agent(state) => {
                    self.app_request_handle.observe_policy(&state);
                    let local_ack = state.acknowledged && state.client == 0;
                    let error = state.error.clone();
                    for nested in &state.events {
                        if let SessionEvent::UserPauseStates(states) = nested {
                            for state in states {
                                self.user_pause.publish(state.clone());
                            }
                        }
                        if let SessionEvent::UserPause(state) = nested {
                            self.user_pause.publish(state.clone());
                        }
                        // Nested agent events never consume private GUI pause tickets.
                        if let SessionEvent::VirtualUsers(users_state) = nested {
                            self.merge_virtual_users_roster(users_state);
                            self.virtual_users_page
                                .receive_external(users_state.clone());
                            if let Some(script) = &self.script {
                                script.virtual_users_state(&self.virtual_users_page.snapshot());
                            }
                        }
                        if let SessionEvent::Automation(automation_state) = nested {
                            self.receive_mapper_state(
                                &automation_state.mapper,
                                MapperPublisher::Agent,
                            );
                            if self.macro_file.pending()
                                && macro_file_conflicts_with_automation(
                                    automation_state,
                                    self.macro_file_observed_revision,
                                    self.macro_recording,
                                    self.macro_loaded_events,
                                    self.macro_playback.as_ref().map(|play| play.state.as_str()),
                                )
                            {
                                self.invalidate_macro_file();
                            }
                            self.macro_file_observed_revision = self
                                .macro_file_observed_revision
                                .max(automation_state.macro_revision);
                            if let Some(definition) = self
                                .forwarding_page
                                .receive_external(automation_state, &self.profile.seats)
                            {
                                self.profile.forwarding = definition;
                            }
                            if let Some(script) = &self.script {
                                script.forwarding_state(&self.forwarding_page.snapshot());
                            }
                            if let Some(definition) = self
                                .replication_page
                                .receive_external(automation_state, &self.profile.seats)
                            {
                                self.profile.replication = definition;
                                self.replication_revision = automation_state.graph_revision;
                                if !self.replication_dirty
                                    || edge_lists_match(
                                        &automation_state.edges,
                                        &self.replication_edges,
                                    )
                                {
                                    self.replication_edges = automation_state.edges.clone();
                                    self.replication_dirty = false;
                                }
                            }
                            if let Some(script) = &self.script {
                                script.graph_state(&self.replication_page.snapshot());
                            }
                        }
                        if let SessionEvent::Annotation(annotation_state) = nested {
                            self.annotation_page
                                .receive_external(annotation_state.clone());
                            if let Some(script) = &self.script {
                                script.annotation_state(annotation_state);
                            }
                        }
                        if let SessionEvent::Output(output_state) = nested {
                            self.output_page.receive(output_state.clone());
                            if let Some(script) = &self.script {
                                script.output_state(output_state);
                            }
                        }
                        if let SessionEvent::Magnifier(magnifier_state) = nested {
                            self.magnifier_page
                                .receive_external(magnifier_state.clone());
                            if let Some(script) = &self.script {
                                script.magnifier_state(magnifier_state);
                            }
                        }
                        if let SessionEvent::MacroStudio(macro_state) = nested {
                            if self.macro_file.pending()
                                && macro_file_conflicts_with_studio(macro_state)
                            {
                                self.invalidate_macro_file();
                            }
                            self.macro_studio_page.receive_external(macro_state.clone());
                            if let Some(script) = &self.script {
                                script.macro_studio_state(macro_state);
                            }
                        }
                    }
                    if let Some(actual_profile) = &state.profile {
                        self.cursor_settings
                            .observe_profile(&mut self.profile.seats, &actual_profile.seats);
                        // The region form caches numeric controls separately; reseed
                        // from the preserved/synchronized draft on its next frame.
                        self.region_draft_user = None;
                        for seat in &mut self.profile.seats {
                            if let Some(actual) = actual_profile
                                .seats
                                .iter()
                                .find(|candidate| candidate.identity() == seat.identity())
                            {
                                seat.output = actual.output.clone();
                                seat.annotation = actual.annotation.clone();
                                seat.magnifier = actual.magnifier.clone();
                            }
                        }
                        self.output_page.sync_profile(&self.profile.seats);
                        self.annotation_page.sync_profile(&self.profile.seats);
                        self.magnifier_page.sync_profile(&self.profile.seats);
                    }
                    let enabled = state.policy.enabled;
                    for event in &state.events {
                        match event {
                            SessionEvent::Paused => self.session_paused = true,
                            SessionEvent::Resumed => self.session_paused = false,
                            _ => {}
                        }
                    }
                    if let Some(profile) = &state.profile
                        && let (Some(current), Some(actual)) =
                            (&mut self.profile.desktop, &profile.desktop)
                    {
                        current.mode = actual.mode;
                        current.multi_keyboard = actual.multi_keyboard;
                    }
                    self.agent_page.receive(state);
                    if let Some(script) = &self.script {
                        let snapshot = self.agent_page.snapshot();
                        script.agent_state(&snapshot);
                        script.agent_ack(&snapshot, local_ack);
                    }
                    if local_ack {
                        if !enabled {
                            self.agent_pending_enable = None;
                            self.agent_broker.request_stop(self.agent_session_epoch);
                        }
                        self.status = if !enabled
                            && !matches!(
                                self.agent_broker.status().phase,
                                crate::gui_agent_broker::Phase::Stopped
                            ) {
                            error.map_or_else(
                                || "Agent Access disabled; broker cleanup is pending.".into(),
                                |error| format!("Agent Access policy failed: {error}"),
                            )
                        } else {
                            error.map_or_else(
                                || "Agent Access policy acknowledged.".into(),
                                |error| format!("Agent Access policy failed: {error}"),
                            )
                        };
                    }
                }
                SessionEvent::Capture(state) => {
                    self.screen_page.receive(state, ctx);
                }
                SessionEvent::ScreenTargets(targets) => {
                    self.screen_targets = targets;
                    self.sync_screen_target();
                }
                SessionEvent::Failed(error) => {
                    self.app_request_handle.terminal();
                    self.agent_pending_enable = None;
                    if self.quit_after_stop || !self.app_request_handle.continuing() {
                        self.agent_broker.request_stop(self.agent_session_epoch);
                    }
                    self.advance_macro_file_session();
                    self.staged_window_locks.clear();
                    self.startup_window_selections.clear();
                    self.startup_window_selection_active = false;
                    self.startup_window_selection_error = None;
                    self.reset_window_selection();
                    if !self.app_request_handle.continuing() {
                        self.agent_page.reset_session();
                    }
                    self.input_release_dialog_open = false;
                    self.session_ready = false;
                    self.session_busy = false;
                    self.session_stopping = false;
                    self.session = None;
                    self.quit_after_stop = false;
                    self.states.clear();
                    self.cursor_settings.reset(&[]);
                    self.user_pause.reset();
                    self.annotation_page.session_stopped();
                    self.output_page.session_stopped();
                    self.magnifier_page.session_stopped();
                    self.screen_page.reset_session();
                    self.screen_targets.clear();
                    self.macro_studio_page.reset_session();
                    self.reset_virtual_users_session();
                    self.replication_page.session_stopped();
                    self.forwarding_page.session_stopped();
                    self.reset_mapper_session();
                    self.replication_revision = 0;
                    self.status = routing_failure_footer(error, &self.recovery.snapshot());
                }
            }
        }
        if self.startup_window_selection_active {
            self.session_ready = false;
            self.session_busy = true;
        }
        if let Some(script) = &self.script {
            script.user_pause_state(&self.user_pause.snapshot(self.selected));
        }
        self.session_busy = self.cursor_settings.keep_busy(self.session_busy);
        if !self.mapper_commands.is_empty() {
            // Once unrelated work finishes, pending mapper work takes over the busy latch.
            if !self.session_busy {
                self.mapper_owns_session_busy = true;
            }
            self.session_busy = true;
        }
    }
    fn add_seat(&mut self, mouse: String) {
        let mut number = self.profile.seats.len() + 1;
        while self
            .profile
            .seats
            .iter()
            .any(|s| s.name.eq_ignore_ascii_case(&format!("User {number}")))
        {
            number += 1;
        }
        let color: [u8; 3] = COLORS[(number - 1) % COLORS.len()].to_array()[..3]
            .try_into()
            .unwrap();
        self.profile.seats.push(Seat {
            id: Seat::new_id(),
            name: format!("User {number}"),
            mouse,
            keyboard: None,
            target_pid: 0,
            desktop_start: None,
            annotation: mux_core::annotation::Settings {
                color,
                ..Default::default()
            },
            magnifier: mux_core::magnifier::Settings {
                border_color: color,
                ..Default::default()
            },
            output: mux_core::output::Settings::default(),
            cursor: mux_core::desktop::CursorSettings {
                color,
                ..Default::default()
            },
        });
        self.selected = Some(self.profile.seats.len() - 1);
        self.page = Page::User;
    }
    fn roster(&mut self, ui: &mut egui::Ui) {
        ui.heading("Users & devices");
        ui.label(format!("{} users assigned", self.profile.seats.len()));
        egui::ScrollArea::vertical()
            .id_salt("roster")
            .max_height(360.0)
            .show(ui, |ui| {
                for i in 0..self.profile.seats.len() {
                    let seat = self.profile.seats[i].clone();
                    self.user_row(ui, i, &seat);
                }
                if self.profile.seats.is_empty() {
                    ui.add_space(12.0);
                    ui.label("No users yet");
                    ui.weak("Add a connected or emulated mouse to create a user.");
                }
            });
        ui.add_space(8.0);
        if self.button(ui, "Add connected device", self.session.is_none()) {
            self.show_devices = true;
        }
        if self.button(ui, "Add emulated user", self.session.is_none()) {
            let id = (1..=0x7FFFFF)
                .find(|id| {
                    !self
                        .profile
                        .seats
                        .iter()
                        .any(|s| s.mouse == format!("virtual://mouse/{id}"))
                })
                .unwrap();
            self.add_seat(emulated::device(DeviceKind::Mouse, id).unwrap().path);
            self.profile.seats.last_mut().unwrap().keyboard =
                Some(emulated::device(DeviceKind::Keyboard, id).unwrap().path);
            let supported = (self.profile.concurrent || self.profile.desktop.is_some())
                && self.profile.seats.last().is_some_and(|seat| {
                    emulated::parse(&seat.mouse)
                        .ok()
                        .flatten()
                        .is_some_and(|device| device.kind == DeviceKind::Mouse)
                        && seat
                            .keyboard
                            .as_deref()
                            .and_then(|path| emulated::parse(path).ok().flatten())
                            .is_some_and(|device| device.kind == DeviceKind::Keyboard)
                });
            if supported {
                let seat = self.profile.seats.last_mut().unwrap();
                let color = seat.cursor.color;
                seat.cursor = mux_core::desktop::CursorSettings::for_new_virtual_user();
                seat.cursor.color = color;
            }
        }
    }
    fn user_row(&mut self, ui: &mut egui::Ui, index: usize, seat: &Seat) {
        let virtual_mouse = emulated::parse(&seat.mouse).ok().flatten().is_some();
        let connected = virtual_mouse
            || self
                .devices
                .iter()
                .any(|d| d.path.eq_ignore_ascii_case(&seat.mouse));
        let color = Color32::from_rgb(
            seat.cursor.color[0],
            seat.cursor.color[1],
            seat.cursor.color[2],
        );
        let selected = self.selected == Some(index);
        let role = if self.states.get(index).is_some_and(|state| state.root) {
            "Root"
        } else {
            "User"
        };
        let pointer = if virtual_mouse {
            "Virtual pointer"
        } else if connected {
            "Pointer ready"
        } else {
            "Pointer offline"
        };
        let keyboard = if seat.keyboard.is_some() {
            "Keyboard"
        } else {
            "No keyboard"
        };
        let mut tooltip = self.states.get(index).map_or_else(
            || {
                format!(
                    "{}\n{} · {} · {}\nTarget PID {}",
                    seat.name, role, pointer, keyboard, seat.target_pid
                )
            },
            |state| {
                format!(
                    "{}\n{} · {} · {}\n{} · PID {}\n{}\n{} buttons · {} keys held",
                    seat.name,
                    role,
                    pointer,
                    keyboard,
                    state.status,
                    state.pid,
                    state.title,
                    state.buttons.len(),
                    state.keys.len()
                )
            },
        );
        let home = self.page == Page::Home;
        if home {
            tooltip.push_str("\nDouble-click to configure");
        }
        if selected && self.states.get(index).is_none() {
            let selected_status = if self.session.is_some() {
                "Starting"
            } else {
                "Ready"
            };
            tooltip.push_str(&format!("\nSelected · {selected_status}"));
        }
        ui.horizontal(|ui| {
            let (marker, _) = ui.allocate_exact_size(egui::vec2(8., 8.), egui::Sense::hover());
            ui.painter().rect_filled(marker, 0., color);
            let observer_label = format!("Select user {}", index + 1);
            let pointer_status = if virtual_mouse {
                "virtual pointer"
            } else if connected {
                "pointer ready"
            } else {
                "pointer offline"
            };
            let keyboard_status = if seat.keyboard.is_some() {
                "keyboard paired"
            } else {
                "no keyboard"
            };
            let accessible_name = format!(
                "Select user {}, {}, {}, {}, {}, {}",
                index + 1,
                keyboard_slider::bounded_accessible_name(&seat.name),
                role,
                pointer_status,
                keyboard_status,
                if selected { "selected" } else { "not selected" },
            );
            let response = ui
                .allocate_ui_with_layout(
                    egui::vec2(USER_IDENTITY_WIDTH, ui.spacing().interact_size.y),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_min_width(USER_IDENTITY_WIDTH);
                        let button = roster_number(ui, index, &seat.identity(), selected)
                            .on_hover_text(&tooltip);
                        keyboard_slider::set_accessible_label(ui, &button, &accessible_name);
                        button
                    },
                )
                .inner;
            let accepted = self
                .script
                .as_mut()
                .is_none_or(|script| script.observe(&observer_label, &response));
            if response.clicked() && accepted && self.consume_action(ui.ctx(), &observer_label) {
                self.selected = Some(index);
                if home && selected && response.double_clicked() {
                    self.go_to_page(ui.ctx(), Page::User);
                }
            }
            roster_glyph(
                ui,
                RosterGlyph::Role,
                role == "Root",
                if role == "Root" {
                    "Click owner"
                } else {
                    "User"
                },
            );
            roster_glyph(
                ui,
                RosterGlyph::Pointer,
                connected,
                if virtual_mouse {
                    "Virtual pointer"
                } else if connected {
                    "Pointer ready"
                } else {
                    "Pointer offline"
                },
            );
            roster_glyph(
                ui,
                RosterGlyph::Keyboard,
                seat.keyboard.is_some(),
                if seat.keyboard.is_some() {
                    "Keyboard paired"
                } else {
                    "No keyboard"
                },
            );
            let name_width = ui.available_width().max(0.);
            let display_name = bounded_display_text(
                ui,
                &seat.name,
                ui.style().text_styles[&egui::TextStyle::Body].clone(),
                name_width,
            );
            let (name_rect, name_response) =
                ui.allocate_exact_size(egui::vec2(name_width, 24.), egui::Sense::hover());
            ui.painter().text(
                name_rect.left_center(),
                egui::Align2::LEFT_CENTER,
                display_name,
                ui.style().text_styles[&egui::TextStyle::Body].clone(),
                ui.visuals().text_color(),
            );
            name_response.on_hover_text(&tooltip);
        });
    }
    fn pending_release_total(&self) -> usize {
        self.states
            .iter()
            .filter_map(|state| state.pending_releases)
            .fold(0, usize::saturating_add)
    }

    fn home(&mut self, ui: &mut egui::Ui) {
        let configure_enabled = self
            .selected
            .is_some_and(|index| index < self.profile.seats.len());
        let allow_access = !self.show_devices
            && !self.recovery_dialog_open
            && !self.input_release_dialog_open
            && !self.preferences_quit_waiting
            && !self.quit_after_stop
            && !self.allow_close;
        let access = allow_access.then(|| home_access_key(ui.ctx())).flatten();
        // Keep hit geometry stable on the frame that receives the chord.
        let show_hints = allow_access
            && access.is_none()
            && ui.input(|input| input.focused && input.modifiers == egui::Modifiers::ALT);
        let mut clicked = None;
        let mut exit = false;
        let launcher_visible = self.app_preferences.show_app_launcher;
        egui::Panel::bottom("home-actions").show_inside(ui, |ui| {
            Divider::horizontal().show(ui);
            ui.vertical(|ui| {
                if self.home_configure_button(
                    ui,
                    &HomeAccess::Configure.caption(show_hints),
                    configure_enabled,
                ) {
                    clicked = Some(HomeAccess::Configure);
                }
                ui.horizontal(|ui| {
                    for (action, variant) in [
                        (HomeAccess::Settings, ButtonVariant::Outline),
                        (HomeAccess::Hide, ButtonVariant::Ghost),
                    ] {
                        if self.home_button_caption(
                            ui,
                            action.label(),
                            &action.caption(show_hints),
                            true,
                            variant,
                        ) {
                            clicked = Some(action);
                        }
                    }
                    if launcher_visible {
                        let action = HomeAccess::Apps;
                        if self.home_button_caption(
                            ui,
                            action.label(),
                            &action.caption(show_hints),
                            allow_access,
                            ButtonVariant::Ghost,
                        ) {
                            clicked = Some(action);
                        }
                    } else {
                        exit = self.home_button(ui, "Exit", allow_access, ButtonVariant::Ghost);
                    }
                });
            });
        });
        if exit {
            self.handle_lifecycle_action(crate::app_lifecycle::LifecycleAction::Quit, ui.ctx());
            return;
        }
        let keyboard = clicked.is_none();
        if let Some(action) = clicked.or_else(|| {
            access.filter(|action| {
                (*action != HomeAccess::Configure || configure_enabled)
                    && (*action != HomeAccess::Apps || launcher_visible)
                    && self.consume_action(ui.ctx(), action.label())
            })
        }) {
            match action {
                HomeAccess::Configure => self.go_to_page(ui.ctx(), Page::User),
                HomeAccess::Settings => self.go_to_page(ui.ctx(), Page::Settings),
                HomeAccess::Apps => self.go_to_page(ui.ctx(), Page::Apps),
                HomeAccess::Hide => ui
                    .ctx()
                    .send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
            }
            if keyboard && let Some(script) = &mut self.script {
                script.home_access_action(action.label());
            }
            return;
        }

        // Route controls get their own pinned panel. Its disclosure viewport is
        // budgeted from the space left after the footer so the roster retains
        // room for the table heading and one compact row.
        const ROSTER_RESERVE: f32 = 64.0;
        const MIN_OPTIONS_VIEWPORT: f32 = 32.0;
        const MAX_OPTIONS_VIEWPORT: f32 = 96.0;
        let available_after_footer = ui.available_height().max(0.0);
        let routing_open_id = egui::Id::new("home-routing-options-open");
        let routing_height_id = egui::Id::new("home-routing-controls-height");
        let routing_options_open = ui
            .ctx()
            .data(|data| data.get_temp::<bool>(routing_open_id).unwrap_or(false));
        let measured_routing_height = ui
            .ctx()
            .data(|data| data.get_temp::<f32>(routing_height_id));
        let routing_frame = egui::Frame::side_top_panel(ui.style());
        let frame_height = routing_frame.inner_margin.sum().y;
        let spacing = ui.spacing().item_spacing.y;
        // Before the first route measurement, reserve two rows for wrapping
        // controls plus the warnings that are already known to be visible.
        let action_rows = 2.0;
        let warning_rows = (self.recovery.snapshot().phase
            == crate::session_recovery::Phase::Quarantined) as usize
            + (self.session.is_none()
                && self
                    .profile
                    .replication
                    .resolve(&self.profile.seats)
                    .is_err()) as usize;
        let row_height = ui.spacing().interact_size.y;
        let warning_height = ui.text_style_height(&egui::TextStyle::Body);
        let routing_rows = action_rows + warning_rows as f32;
        let estimated_routing_height = action_rows * row_height
            + warning_rows as f32 * warning_height
            + (routing_rows - 1.0).max(0.0) * spacing;
        let routing_chrome = measured_routing_height
            .filter(|height| height.is_finite() && *height > 0.0)
            .unwrap_or(estimated_routing_height);
        // CollapsingHeader includes vertical button padding around its text.
        let header_height = (ui.text_style_height(&egui::TextStyle::Button)
            + 2.0 * ui.spacing().button_padding.y)
            .max(row_height);
        let divider_height = ouroboros_ui::tokens::core::BORDER_THIN + spacing;
        let fixed_panel_height =
            frame_height + routing_chrome + divider_height + header_height + 2.0 * spacing;
        let options_viewport = if routing_options_open {
            (available_after_footer - fixed_panel_height - ROSTER_RESERVE)
                .clamp(MIN_OPTIONS_VIEWPORT, MAX_OPTIONS_VIEWPORT)
                .min((available_after_footer - fixed_panel_height).max(0.0))
        } else {
            0.0
        };
        let routing_panel_height =
            (fixed_panel_height + options_viewport).min(available_after_footer);
        let mode_label = self.profile.desktop.as_ref().map_or_else(
            || {
                if self.profile.concurrent {
                    "Assigned / concurrent".to_owned()
                } else {
                    "Assigned / fixed".to_owned()
                }
            },
            |options| {
                format!(
                    "{}{}",
                    match options.mode {
                        mux_core::routing::Mode::Switched => "Switched",
                        mux_core::routing::Mode::Concurrent => "Concurrent",
                    },
                    if options.multi_keyboard {
                        " + keyboards"
                    } else {
                        ""
                    }
                )
            },
        );
        let secondary = Theme::get(ui).muted_foreground;
        let routing_options = egui::Panel::bottom("home-routing")
            .frame(routing_frame)
            .exact_size(routing_panel_height)
            .show_inside(ui, |ui| {
                let divider = Divider::horizontal().show(ui);
                let current_divider_height = divider.rect.height() + spacing;
                let route_controls = ui.scope_builder(
                    egui::UiBuilder::new().id_salt("home-routing-controls"),
                    |ui| self.routing(ui),
                );
                let current_routing_height = route_controls.response.rect.height();
                ui.ctx().data_mut(|data| {
                    data.insert_temp(routing_height_id, current_routing_height)
                });
                let current_fixed_height = frame_height
                    + current_divider_height
                    + current_routing_height
                    + header_height
                    + 2.0 * spacing;
                let current_options_viewport = if routing_options_open {
                    (available_after_footer - current_fixed_height - ROSTER_RESERVE)
                        .clamp(MIN_OPTIONS_VIEWPORT, MAX_OPTIONS_VIEWPORT)
                        .min((available_after_footer - current_fixed_height).max(0.0))
                } else {
                    0.0
                };
                egui::CollapsingHeader::new(
                    RichText::new(format!("Routing options · {mode_label}"))
                        .color(secondary),
                )
                .id_salt("routing-options")
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("home-routing-options")
                        .min_scrolled_height(0.0)
                        .max_height(current_options_viewport)
                        .show(ui, |ui| {
                            self.desktop_controls(ui);
                            if self.button(ui, "Device status", true) { self.show_devices = true; }
                            if self.profile.desktop.is_none() {
                                ui.add_enabled_ui(false, |ui| {
                                    ui.label("Mode switching and the Multi keyboard option require automatic targeting.");
                                });
                                ui.add_enabled_ui(self.session.is_none(), |ui| {
                                    Checkbox::new(&mut self.profile.concurrent)
                                        .label("Concurrent desktop cursors")
                                        .show(ui);
                                });
                            }
                        });
                })
            });
        let options_open = routing_options.inner.body_response.is_some();
        ui.ctx()
            .data_mut(|data| data.insert_temp(routing_open_id, options_open));
        if let Some(script) = &mut self.script {
            script.observe("Routing options", &routing_options.inner.header_response);
        }

        // The remaining central region must not paint or accept input in either
        // pinned panel, even when warning rows leave very little roster space.
        ui.set_clip_rect(ui.clip_rect().intersect(ui.available_rect_before_wrap()));
        let pending_releases = self.pending_release_total();
        ui.horizontal(|ui| {
            let secondary = Theme::get(ui).muted_foreground;
            ui.allocate_exact_size(egui::vec2(8., 8.), egui::Sense::hover());
            ui.allocate_ui_with_layout(
                egui::vec2(USER_IDENTITY_WIDTH, ui.spacing().interact_size.y),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_min_width(USER_IDENTITY_WIDTH);
                    ui.label(RichText::new("User").small().color(secondary));
                },
            );
            roster_glyph(ui, RosterGlyph::Role, true, "User role");
            roster_glyph(ui, RosterGlyph::Pointer, true, "Pointer status");
            roster_glyph(ui, RosterGlyph::Keyboard, true, "Keyboard status");
            if pending_releases > 0 {
                let visible = format!("{pending_releases} pending");
                let response = Button::new(visible)
                    .sm()
                    .variant(ButtonVariant::Ghost)
                    .show(ui)
                    .on_hover_text("Input release details");
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        true,
                        "Input release details".to_owned(),
                    )
                });
                let accepted = self
                    .script
                    .as_mut()
                    .is_none_or(|script| script.observe("Input release details", &response));
                if response.clicked()
                    && accepted
                    && self.consume_action(ui.ctx(), "Input release details")
                {
                    self.input_release_dialog_open = true;
                }
            } else {
                ui.label(RichText::new("Name").small().color(secondary));
            }
        });

        // Route and footer panels reserve their own viewport; the roster gets
        // the remaining central height and scrolls independently.
        let roster_height = ui.available_height().max(0.0);
        egui::ScrollArea::vertical()
            .id_salt("home-users")
            .max_height(roster_height)
            .min_scrolled_height(0.0)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                for index in 0..self.profile.seats.len() {
                    let seat = self.profile.seats[index].clone();
                    self.user_row(ui, index, &seat);
                    if self.page != Page::Home {
                        break;
                    }
                }
                if self.profile.seats.is_empty() {
                    ui.label("No users assigned");
                    ui.weak("Add a connected or emulated pointer in Users & devices.");
                }
            });
    }
    fn apps_launcher(&mut self, ui: &mut egui::Ui) {
        Heading::new("Apps").heading().show(ui);
        let action = egui::Panel::bottom("apps-actions").show_inside(ui, |ui| {
            Divider::horizontal().show(ui);
            ui.horizontal(|ui| {
                let open = self.home_button(ui, "Open app", true, ButtonVariant::Secondary);
                let label = if self.app_info_open {
                    "App list"
                } else {
                    "App info"
                };
                if self.home_button(ui, label, true, ButtonVariant::Outline) {
                    self.app_info_open = !self.app_info_open;
                }
                open
            })
            .inner
        });
        if action.inner {
            self.go_to_page(ui.ctx(), BUILT_IN_APPS[self.selected_app].page);
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt(("apps-content", self.app_info_open))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = core::SPACE_1;
                if self.app_info_open {
                    let app = &BUILT_IN_APPS[self.selected_app];
                    Heading::new(app.label).heading().show(ui);
                    Text::new(app.description).wrap().show(ui);
                    Divider::horizontal().show(ui);
                    Text::new(format!("Built in · Version {}", env!("CARGO_PKG_VERSION")))
                        .caption()
                        .wrap()
                        .show(ui);
                    Text::new(format!("Part of {}", crate::APP_NAME))
                        .caption()
                        .wrap()
                        .show(ui);
                    return;
                }
                for (index, app) in BUILT_IN_APPS.iter().enumerate() {
                    let selected = self.selected_app == index;
                    let response =
                        launcher_app_row(ui, app.label, selected).on_hover_text(app.description);
                    response.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::SelectableLabel,
                            true,
                            selected,
                            app.label,
                        )
                    });
                    if response.has_focus() {
                        ui.painter().rect_stroke(
                            response.rect.shrink(3.0),
                            core::RADIUS_SM,
                            egui::Stroke::new(core::BORDER_FOCUS, Theme::get(ui).ring),
                            egui::StrokeKind::Inside,
                        );
                    }
                    let label = format!("Select launcher app {}", app.label);
                    let accepted = self
                        .script
                        .as_mut()
                        .is_none_or(|script| script.observe(&label, &response));
                    if response.clicked() && accepted && self.consume_action(ui.ctx(), &label) {
                        self.selected_app = index;
                    }
                }
                self.home_button(ui, "App Library", false, ButtonVariant::Ghost);
                Text::new("External app installation is unavailable.")
                    .caption()
                    .wrap()
                    .show(ui);
            });
    }

    fn settings_landing(&mut self, ui: &mut egui::Ui) {
        ui.heading("Settings");
        ui.label("Configure users, profiles, and tools.");
        Divider::horizontal().show(ui);
        if ui.available_width() >= 460.0 {
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(148.0, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |rail| {
                        rail.set_width(148.0);
                        for category in SettingsCategory::ALL {
                            self.settings_category_control(rail, category, false);
                        }
                    },
                );
                ui.vertical(|ui| self.settings_destinations(ui));
            });
        } else {
            ui.horizontal_wrapped(|ui| {
                for category in SettingsCategory::ALL {
                    self.settings_category_control(ui, category, true);
                }
            });
            Divider::horizontal().show(ui);
            self.settings_destinations(ui);
        }
    }
    fn inventory(&mut self, ctx: &egui::Context) {
        let mut open = self.show_devices;
        let mut close_requested = false;
        egui::Window::new("Device inventory")
            .open(&mut open)
            .default_width(500.0)
            .show(ctx, |ui| {
                let busy = self.discovery.status();
                if self.button(
                    ui,
                    "Refresh devices",
                    !busy.in_flight && !busy.queued && !busy.worker_failed,
                ) {
                    self.refresh();
                }
                if self.button(ui, "Close inventory", true) {
                    close_requested = true;
                }
                self.discovery_status_controls(ui);
                let mut add = None;
                egui::ScrollArea::vertical()
                    .max_height(350.0)
                    .show(ui, |ui| {
                        for (i, d) in self.devices.iter().enumerate() {
                            ui.horizontal(|ui| {
                                ui.label(format!("{:?} {}", d.kind, i + 1))
                                    .on_hover_text(&d.path);
                                if d.kind == DeviceKind::Keyboard {
                                    Text::new(format!(
                                        "{} activity packets",
                                        self.keyboard_packets.get(&d.handle).copied().unwrap_or(0)
                                    ))
                                    .caption()
                                    .show(ui);
                                }
                                if d.kind == DeviceKind::Mouse {
                                    let assigned = self
                                        .profile
                                        .seats
                                        .iter()
                                        .any(|s| s.mouse.eq_ignore_ascii_case(&d.path));
                                    if Button::new(if assigned { "Assigned" } else { "Add user" })
                                        .enabled(!assigned && self.session.is_none())
                                        .sm()
                                        .show(ui)
                                        .clicked()
                                    {
                                        add = Some(d.path.clone());
                                    }
                                }
                            });
                            ui.collapsing(format!("Device details {}", i + 1), |ui| {
                                ui.label(&d.path);
                                ui.monospace(format!("Handle {:X}", d.handle));
                            });
                            Divider::horizontal().show(ui);
                        }
                        if self.devices.is_empty() {
                            let discovery = self.discovery.status();
                            ui.label(if discovery.in_flight || discovery.queued {
                                "Discovering input devices…"
                            } else if !self.discovery_devices_ok {
                                "Device discovery is unavailable."
                            } else {
                                "No mice or keyboards detected."
                            });
                        }
                    });
                if let Some(mouse) = add {
                    self.add_seat(mouse);
                }
            });
        self.show_devices = open && !close_requested;
    }
    fn user_pause_controls(&mut self, ui: &mut egui::Ui, user: usize) {
        let state = self
            .user_pause
            .states
            .iter()
            .find(|state| state.user == user)
            .cloned();
        let pending = self.user_pause.pending.is_some();
        let paused = state.as_ref().is_some_and(|state| state.paused);
        let reason = if pending {
            Some("Waiting for the selected user's pause acknowledgement.".to_owned())
        } else if !self.session_ready || self.session_stopping {
            Some("Wait for routing readiness.".to_owned())
        } else if self.session_paused {
            Some("Global Pause is active. Resume routing from Home first.".to_owned())
        } else if self.user_pause.next_request == u64::MAX {
            Some("Pause request identity exhausted. Restart the application.".to_owned())
        } else if self.session_busy {
            Some("Wait for the current routing operation.".to_owned())
        } else if let Some(state) = &state {
            if !state.supported {
                Some(
                    state
                        .error
                        .clone()
                        .unwrap_or_else(|| "This route does not support individual pause.".into()),
                )
            } else if state.pending_releases != 0 {
                Some(format!(
                    "{} saved releases remain pending. Resume stays unavailable.",
                    state.pending_releases
                ))
            } else {
                None
            }
        } else {
            Some("Waiting for current user pause state.".to_owned())
        };
        ui.horizontal_wrapped(|ui| {
            let label = if paused {
                "Resume this user"
            } else {
                "Pause this user"
            };
            if self.home_button(ui, label, reason.is_none(), ButtonVariant::Outline)
                && let Some(request) =
                    self.user_pause
                        .issue(user, self.profile.seats[user].identity(), !paused)
                && let Some(session) = &self.session
            {
                session.request(request);
            }
            Text::new(if pending {
                "Pause request pending"
            } else if state
                .as_ref()
                .is_some_and(|state| state.pending_releases != 0)
            {
                "User cleanup pending"
            } else if paused {
                "User paused"
            } else {
                "User running"
            })
            .caption()
            .show(ui);
        });
        let detail = reason.or_else(|| state.and_then(|state| state.error));
        if let Some(detail) = detail {
            ui.add(egui::Label::new(RichText::new(&detail).small()).truncate())
                .on_hover_text(&detail);
            ui.collapsing("User pause details", |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("user-pause-reason")
                    .max_height(44.0)
                    .show(ui, |ui| {
                        Text::new(detail).caption().wrap().show(ui);
                    });
            });
        }
    }
    fn user_settings(&mut self, ui: &mut egui::Ui) {
        let Some(index) = self.selected.filter(|i| *i < self.profile.seats.len()) else {
            ui.heading("Choose a user");
            ui.label("Select a user in Users & devices.");
            return;
        };
        if !self.user_active(index) {
            ui.colored_label(Color32::LIGHT_RED, "Virtual replica removed. Its roster entry remains visible, but settings are unavailable.");
            return;
        }
        if self.session.is_some() {
            egui::Panel::bottom("selected-user-pause")
                .show_inside(ui, |ui| self.user_pause_controls(ui, index));
        }
        const TABS: [(&str, UserTab); 12] = [
            ("Input", UserTab::Pointer),
            ("Keyboard", UserTab::Keyboard),
            ("Output", UserTab::Target),
            ("Motion", UserTab::Motion),
            ("Buttons", UserTab::Buttons),
            ("Wheel", UserTab::Wheel),
            ("Theme", UserTab::Theme),
            ("Overlay", UserTab::Overlay),
            ("Magnifier", UserTab::Magnifier),
            ("Lock", UserTab::Lock),
            ("Draw", UserTab::Draw),
            ("Macros", UserTab::Macros),
        ];
        // The pinned Tabs molecule updates its selected index directly, bypassing
        // this page's scripted-observation and consume-once guards. Compose the
        // same Ouroboros buttons in a Surface so each existing label is still
        // observed and admitted through the original guard.
        let tabs_width = ui.available_width();
        let reveal_selected = (tabs_width - self.user_tabs_width).abs() > 0.5;
        self.user_tabs_width = tabs_width;
        let rows: Vec<&[(&str, UserTab)]> = if tabs_width < 400.0 {
            vec![&TABS[..]]
        } else {
            vec![&TABS[..6], &TABS[6..]]
        };
        Surface::new().pad(core::SPACE_1).show(ui, |ui| {
            for (row_index, row) in rows.into_iter().enumerate() {
                // One scrolling strip preserves room for the page body on narrow
                // windows; normal widths keep the familiar two-row arrangement.
                ui.scope(|ui| {
                    // Reserve a separate strip so the scrollbar cannot cover labels.
                    ui.spacing_mut().scroll.floating = false;
                    let row_height =
                        core::Size::Sm.height() + ui.spacing().scroll.allocated_width();
                    egui::ScrollArea::horizontal()
                        .id_salt(("user-tab-row", row_index))
                        .auto_shrink([false, true])
                        .max_height(row_height)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                for &(label, tab) in row {
                                    let response = Button::new(label)
                                        .sm()
                                        .variant(if self.user_tab == tab {
                                            ButtonVariant::Secondary
                                        } else {
                                            ButtonVariant::Ghost
                                        })
                                        .show(ui);
                                    let accepted = self
                                        .script
                                        .as_mut()
                                        .is_none_or(|script| script.observe(label, &response));
                                    if response.clicked()
                                        && accepted
                                        && self.consume_action(ui.ctx(), label)
                                    {
                                        self.user_tab = tab;
                                        response.scroll_to_me(Some(egui::Align::Center));
                                    } else if reveal_selected && self.user_tab == tab {
                                        response.scroll_to_me(Some(egui::Align::Center));
                                    }
                                }
                            });
                        });
                });
            }
        });
        // Tool controllers own their drafts and acknowledged profile changes.
        // Dispatch before cloning a cursor draft so its writeback cannot undo them.
        let seat_id = self.profile.seats[index].identity();
        let tab = self.user_tab;
        match tab {
            UserTab::Target if self.session.is_some() => {
                with_user_tab_content(ui, &seat_id, tab, |ui| {
                    self.live_window_selection_controls(ui, index, false);
                });
                return;
            }
            UserTab::Draw => {
                with_user_tab_content(ui, &seat_id, tab, |ui| {
                    self.annotation_controls(ui, Some(index));
                });
                return;
            }
            UserTab::Magnifier => {
                with_user_tab_content(ui, &seat_id, tab, |ui| {
                    self.magnifier_controls(ui, Some(index));
                });
                return;
            }
            UserTab::Macros => {
                with_user_tab_content(ui, &seat_id, tab, |ui| {
                    self.macro_studio_controls(ui, Some(index));
                });
                return;
            }
            _ => {}
        }
        let mut seat = self.profile.seats[index].clone();
        let seat_accessible_name = keyboard_slider::bounded_accessible_name(&seat.name);
        let editable = self.session.is_none()
            || matches!(
                self.user_tab,
                UserTab::Keyboard
                    | UserTab::Motion
                    | UserTab::Buttons
                    | UserTab::Wheel
                    | UserTab::Theme
                    | UserTab::Overlay
                    | UserTab::Lock
            );
        let mut remove_user = false;
        let mut apply_requested = false;
        let mut use_applied_requested = false;
        let available_rect = ui.available_rect_before_wrap();
        let error_line_height = ui.text_style_height(&egui::TextStyle::Small);
        let row_gap = ui.spacing().item_spacing.y;
        let conflict_at_layout = self.cursor_settings.conflict(&seat);
        let (footer_rect, separate_conflict_row) = crate::cursor_settings_ui::footer_layout(
            available_rect,
            error_line_height,
            row_gap,
            conflict_at_layout,
        );
        let body_bottom = (footer_rect.top() - row_gap).max(available_rect.top());
        let body_rect = egui::Rect::from_min_max(
            available_rect.min,
            egui::pos2(available_rect.right(), body_bottom),
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(body_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
            |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(body_rect));
            let content_scroll = user_tab_scroll_area(&seat_id, self.user_tab);
            content_scroll
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    Surface::new()
                        .border_strong()
                        .pad(core::SPACE_2)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.add_enabled_ui(editable, |ui| match self.user_tab {
            UserTab::Pointer => {
                ui.horizontal(|ui| {
                    ui.label("Name");
                    ui.add_enabled_ui(self.session.is_none(), |ui| {
                        Input::new(&mut seat.name)
                            .id_source("seat_name")
                            .show(ui)
                    });
                });
                let device = self
                    .devices
                    .iter()
                    .find(|d| d.path.eq_ignore_ascii_case(&seat.mouse));
                let device_status = if emulated::parse(&seat.mouse).ok().flatten().is_some() {
                    "Emulated mouse ready"
                } else if device.is_some() {
                    "Mouse connected"
                } else {
                    "Assigned mouse is offline"
                };
                ui.label(device_status);
                ui.collapsing("Device identity", |ui| {
                    ui.label(&seat.mouse);
                    if let Some(device) = device {
                        ui.label(format!(
                            "{} preview packets received",
                            self.pointers.get(&device.handle).map_or(0, |p| p.packets)
                        ));
                    }
                    ui.weak("Each virtual device has its own input state and application route.");
                });
                ui.add_space(core::SPACE_3);
                if self.home_button(
                    ui,
                    "Remove this user",
                    self.session.is_none(),
                    ButtonVariant::Destructive,
                ) {
                    remove_user = true;
                }
            }
            UserTab::Motion => {
                if ui.available_width() < 360.0 {
                    ui.label(format!("Horizontal · {:.1}×", seat.cursor.speed_x));
                    let response = keyboard_slider::accessible_slider(
                        ui,
                        ("motion-speed-x", index),
                        &format!("Horizontal motion speed for {seat_accessible_name}"),
                        &mut seat.cursor.speed_x,
                        0.1,
                        8.0,
                        0.1,
                    );
                    if let Some(script) = self.script.as_mut() {
                        script.observe("Motion horizontal speed", &response);
                    }
                    ui.label(format!("Vertical · {:.1}×", seat.cursor.speed_y));
                    let response = keyboard_slider::accessible_slider(
                        ui,
                        ("motion-speed-y", index),
                        &format!("Vertical motion speed for {seat_accessible_name}"),
                        &mut seat.cursor.speed_y,
                        0.1,
                        8.0,
                        0.1,
                    );
                    if let Some(script) = self.script.as_mut() {
                        script.observe("Motion vertical speed", &response);
                    }
                } else {
                    let mut horizontal_response = None;
                    let mut vertical_response = None;
                    egui::Grid::new("user-motion-controls")
                        .num_columns(2)
                        .spacing([core::SPACE_2, core::SPACE_1])
                        .show(ui, |ui| {
                            ui.label(format!("Horizontal · {:.1}×", seat.cursor.speed_x));
                            let width = ui.available_width().clamp(120.0, 180.0);
                            ui.allocate_ui_with_layout(
                                egui::vec2(width, ui.spacing().interact_size.y),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    horizontal_response = Some(keyboard_slider::accessible_slider(
                                        ui,
                                        ("motion-speed-x", index),
                                        &format!(
                                            "Horizontal motion speed for {seat_accessible_name}"
                                        ),
                                        &mut seat.cursor.speed_x,
                                        0.1,
                                        8.0,
                                        0.1,
                                    ));
                                },
                            );
                            ui.end_row();
                            ui.label(format!("Vertical · {:.1}×", seat.cursor.speed_y));
                            let width = ui.available_width().clamp(120.0, 180.0);
                            ui.allocate_ui_with_layout(
                                egui::vec2(width, ui.spacing().interact_size.y),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    vertical_response = Some(keyboard_slider::accessible_slider(
                                        ui,
                                        ("motion-speed-y", index),
                                        &format!(
                                            "Vertical motion speed for {seat_accessible_name}"
                                        ),
                                        &mut seat.cursor.speed_y,
                                        0.1,
                                        8.0,
                                        0.1,
                                    ));
                                },
                            );
                            ui.end_row();
                        });
                    if let Some(script) = self.script.as_mut() {
                        if let Some(response) = &horizontal_response {
                            script.observe("Motion horizontal speed", response);
                        }
                        if let Some(response) = &vertical_response {
                            script.observe("Motion vertical speed", response);
                        }
                    }
                }
                ui.horizontal_wrapped(|ui| {
                    Checkbox::new(&mut seat.cursor.invert_x)
                        .label("Invert horizontal motion")
                        .show(ui);
                    Checkbox::new(&mut seat.cursor.invert_y)
                        .label("Invert vertical motion")
                        .show(ui);
                });
                Text::new("Per-packet acceleration curve (original response presets; not Windows pointer ballistics).")
                    .caption()
                    .wrap()
                    .show(ui);
                ui.horizontal_wrapped(|ui| {
                    for (label, curve) in [
                        ("Linear response", mux_core::desktop::MotionCurve::Linear),
                        ("Gentle response", mux_core::desktop::MotionCurve::Gentle),
                        ("Strong response", mux_core::desktop::MotionCurve::Strong),
                    ] {
                        if self.button(ui, label, true) {
                            seat.cursor.motion_curve = curve;
                        }
                    }
                });
                ui.label(format!("Selected response: {:?}", seat.cursor.motion_curve));

                ui.add_space(core::SPACE_2);
                ui.strong("Input coalescing");
                Text::new("Combine rapid motion updates before native publication. Either configured threshold flushes first; a 16 ms deadline or 64-event cap applies when only one is enabled.")
                    .caption()
                    .wrap()
                    .show(ui);

                let buffer_label = "Coalesce by event count";
                let mut buffer_enabled = seat.cursor.motion_coalescing.buffer_events.is_some();
                let buffer_toggle = Checkbox::new(&mut buffer_enabled)
                    .label(buffer_label)
                    .show(ui);
                let buffer_toggle_accepted = self
                    .script
                    .as_mut()
                    .is_none_or(|script| script.observe(buffer_label, &buffer_toggle));
                if buffer_toggle.changed()
                    && buffer_toggle_accepted
                    && self.consume_action(ui.ctx(), buffer_label)
                {
                    seat.cursor.motion_coalescing.buffer_events =
                        buffer_enabled.then_some(8);
                }
                if let Some(events) = seat.cursor.motion_coalescing.buffer_events {
                    let mut event_limit = f32::from(events);
                    ui.label(format!("Buffered event limit · {events} events"));
                    let response = keyboard_slider::accessible_slider(
                        ui,
                        ("motion-coalescing-buffer", index),
                        &format!("Buffered event limit for {seat_accessible_name}"),
                        &mut event_limit,
                        2.0,
                        64.0,
                        1.0,
                    );
                    if let Some(script) = self.script.as_mut() {
                        script.observe("Buffered event limit", &response);
                    }
                    if response.changed() {
                        seat.cursor.motion_coalescing.buffer_events =
                            Some(event_limit.round() as u8);
                    }
                }

                let interval_label = "Coalesce by time window";
                let mut interval_enabled = seat.cursor.motion_coalescing.interval_ms.is_some();
                let interval_toggle = Checkbox::new(&mut interval_enabled)
                    .label(interval_label)
                    .show(ui);
                let interval_toggle_accepted = self
                    .script
                    .as_mut()
                    .is_none_or(|script| script.observe(interval_label, &interval_toggle));
                if interval_toggle.changed()
                    && interval_toggle_accepted
                    && self.consume_action(ui.ctx(), interval_label)
                {
                    seat.cursor.motion_coalescing.interval_ms = interval_enabled.then_some(4);
                }
                if let Some(interval_ms) = seat.cursor.motion_coalescing.interval_ms {
                    let mut interval = f32::from(interval_ms);
                    ui.label(format!("Coalescing time window · {interval_ms} ms"));
                    let response = keyboard_slider::accessible_slider(
                        ui,
                        ("motion-coalescing-interval", index),
                        &format!("Coalescing time window for {seat_accessible_name}"),
                        &mut interval,
                        1.0,
                        16.0,
                        1.0,
                    );
                    if let Some(script) = self.script.as_mut() {
                        script.observe("Coalescing time window", &response);
                    }
                    if response.changed() {
                        seat.cursor.motion_coalescing.interval_ms =
                            Some(interval.round() as u8);
                    }
                }

                let mut high_dpi_only = false;
                let high_dpi_response = ui
                    .add_enabled_ui(false, |ui| {
                        Checkbox::new(&mut high_dpi_only)
                            .label("High-DPI / foreground-only mode")
                            .show(ui)
                    })
                    .inner;
                if let Some(script) = self.script.as_mut() {
                    script.observe("High-DPI / foreground-only mode", &high_dpi_response);
                }
                Text::new("Unavailable for virtual input: polling and foreground metadata are not provided.")
                    .caption()
                    .wrap()
                    .show(ui);
            }
            UserTab::Buttons => {
                Checkbox::new(&mut seat.cursor.swap_buttons).label("Swap primary and secondary buttons").show(ui);
                const BUTTONS: [Option<mux_core::desktop::MouseButton>; 6] = [
                    None,
                    Some(mux_core::desktop::MouseButton::Primary),
                    Some(mux_core::desktop::MouseButton::Secondary),
                    Some(mux_core::desktop::MouseButton::Middle),
                    Some(mux_core::desktop::MouseButton::X1),
                    Some(mux_core::desktop::MouseButton::X2),
                ];
                const OUTPUTS: [&str; 6] = [
                    "Disabled",
                    "Primary",
                    "Secondary",
                    "Middle",
                    "Back",
                    "Forward",
                ];
                const SOURCES: [&str; 5] = [
                    "Primary input",
                    "Secondary input",
                    "Middle input",
                    "Back input",
                    "Forward input",
                ];
                egui::Grid::new("user-button-mappings")
                    .num_columns(2)
                    .spacing([core::SPACE_2, core::SPACE_1])
                    .show(ui, |ui| {
                        for (source_index, (source, mapping)) in
                            SOURCES.iter().zip(&mut seat.cursor.button_mapping)
                                .enumerate()
                        {
                            ui.label(*source);
                            ui.push_id(("button-mapping", source_index), |ui| {
                                let mut selected = BUTTONS
                                    .iter()
                                    .position(|button| button == mapping)
                                    .unwrap_or(0);
                                if Select::new(&mut selected)
                                    .sm()
                                    .options(OUTPUTS)
                                    .placeholder("Choose output")
                                    .show(ui)
                                    .changed()
                                {
                                    *mapping = BUTTONS[selected];
                                }
                            });
                            ui.end_row();
                        }
                    });
                Text::new("Each output can be used once; Disabled drops input.")
                    .caption()
                    .wrap()
                    .show(ui);
                if let Err(error) = seat.cursor.validate() {
                    ui.colored_label(COLORS[1], format!("Invalid cursor settings: {error}"));
                }
                ui.weak("Held buttons belong to this user. Pause releases all buttons and keys.");
            }
            UserTab::Wheel => {
                if ui.available_width() < 480.0 {
                    ui.label(format!("Wheel multiplier · {:.1}×", seat.cursor.wheel_scale));
                    let response = keyboard_slider::accessible_slider(
                        ui,
                        ("wheel-scale", index),
                        &format!("Vertical wheel multiplier for {seat_accessible_name}"),
                        &mut seat.cursor.wheel_scale,
                        0.1,
                        8.0,
                        0.1,
                    );
                    if let Some(script) = self.script.as_mut() {
                        script.observe("Vertical wheel multiplier", &response);
                    }
                    Checkbox::new(&mut seat.cursor.invert_wheel)
                        .label("Reverse wheel direction")
                        .show(ui);
                } else {
                    let mut response = None;
                    ui.horizontal(|ui| {
                        ui.label(format!("Wheel multiplier · {:.1}×", seat.cursor.wheel_scale));
                        let width = ui.available_width().clamp(120.0, 180.0);
                        ui.allocate_ui_with_layout(
                            egui::vec2(width, ui.spacing().interact_size.y),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                response = Some(keyboard_slider::accessible_slider(
                                    ui,
                                    ("wheel-scale", index),
                                    &format!(
                                        "Vertical wheel multiplier for {seat_accessible_name}"
                                    ),
                                    &mut seat.cursor.wheel_scale,
                                    0.1,
                                    8.0,
                                    0.1,
                                ));
                            },
                        );
                    });
                    if let (Some(script), Some(response)) = (self.script.as_mut(), response.as_ref())
                    {
                        script.observe("Vertical wheel multiplier", response);
                    }
                    Checkbox::new(&mut seat.cursor.invert_wheel)
                        .label("Reverse wheel direction")
                        .show(ui);
                }
                ui.separator();
                ui.heading("Convert vertical to horizontal");
                let virtual_mouse = emulated::parse(&seat.mouse).ok().flatten().is_some_and(|device| device.kind == DeviceKind::Mouse);
                let route_supported = self.profile.desktop.is_some() || self.profile.concurrent;
                let emulation_available = virtual_mouse && route_supported;
                let emulation_options = [
                    "Never",
                    "Always",
                    "While Primary is down",
                    "While Secondary is down",
                    "While Middle is down",
                    "While Back is down",
                    "While Forward is down",
                ];
                let selected_emulation = match seat.cursor.horizontal_wheel_emulation {
                    mux_core::desktop::HorizontalWheelEmulation::Never => "Never",
                    mux_core::desktop::HorizontalWheelEmulation::Always => "Always",
                    mux_core::desktop::HorizontalWheelEmulation::WhilePrimary => {
                        "While Primary is down"
                    }
                    mux_core::desktop::HorizontalWheelEmulation::WhileSecondary => {
                        "While Secondary is down"
                    }
                    mux_core::desktop::HorizontalWheelEmulation::WhileMiddle => {
                        "While Middle is down"
                    }
                    mux_core::desktop::HorizontalWheelEmulation::WhileBack => {
                        "While Back is down"
                    }
                    mux_core::desktop::HorizontalWheelEmulation::WhileForward => {
                        "While Forward is down"
                    }
                };
                ui.horizontal_wrapped(|ui| {
                    ui.label("Emulate horizontal output");
                    ui.add_enabled_ui(emulation_available, |ui| {
                        let trigger = Button::new(format!("{selected_emulation}  ▾"))
                            .variant(ButtonVariant::Outline)
                            .show(ui);
                        if let Some(script) = self.script.as_mut() {
                            script.observe("Horizontal emulation selector", &trigger);
                        }

                        let ctx = ui.ctx().clone();
                        let mut chosen = None;
                        egui::Popup::menu(&trigger).show(|menu| {
                            for (index, label) in emulation_options.iter().enumerate() {
                                let option = MenuItem::new(*label)
                                    .checked(*label == selected_emulation)
                                    .id_source(("horizontal-wheel-emulation", index))
                                    .show(menu);
                                if let Some(script) = self.script.as_mut() {
                                    script.observe(label, &option);
                                }
                                if option.clicked() && self.consume_action(&ctx, label) {
                                    chosen = Some(index);
                                    menu.close();
                                }
                            }
                        });
                        if let Some(index) = chosen {
                            seat.cursor.horizontal_wheel_emulation = match index {
                                1 => mux_core::desktop::HorizontalWheelEmulation::Always,
                                2 => mux_core::desktop::HorizontalWheelEmulation::WhilePrimary,
                                3 => mux_core::desktop::HorizontalWheelEmulation::WhileSecondary,
                                4 => mux_core::desktop::HorizontalWheelEmulation::WhileMiddle,
                                5 => mux_core::desktop::HorizontalWheelEmulation::WhileBack,
                                6 => mux_core::desktop::HorizontalWheelEmulation::WhileForward,
                                _ => mux_core::desktop::HorizontalWheelEmulation::Never,
                            };
                        }
                    });
                });
                if !emulation_available {
                    ui.colored_label(
                        COLORS[4],
                        if !virtual_mouse {
                            "Unavailable for physical mice. The saved setting is retained."
                        } else {
                            "Unavailable in fixed routing without desktop cursors. The saved setting is retained."
                        },
                    );
                }
                Text::new("Convert vertical wheel input to horizontal output.")
                    .caption()
                    .wrap()
                    .show(ui)
                    .on_hover_text("The trigger uses the raw source button held by this user, even if that button is mapped to a key or disabled. This is separate from the policy for input that is already horizontal.");
                ui.separator();
                ui.heading("Incoming horizontal wheel");
                ui.weak("Choose how already-horizontal input is handled.");
                ui.horizontal_wrapped(|ui| {
                    if self.button(ui, "Ignore horizontal wheel", true) {
                        seat.cursor.horizontal_wheel =
                            mux_core::desktop::HorizontalWheelPolicy::Ignore;
                    }
                    if self.button(ui, "Convert to vertical", true) {
                        seat.cursor.horizontal_wheel =
                            mux_core::desktop::HorizontalWheelPolicy::ConvertToVertical;
                    }
                    if self.button(ui, "Preserve horizontal wheel", self.profile.desktop.is_some() || self.profile.concurrent) {
                        seat.cursor.horizontal_wheel = mux_core::desktop::HorizontalWheelPolicy::Preserve;
                    }
                });
                ui.label(format!(
                    "Policy: {}",
                    match seat.cursor.horizontal_wheel {
                        mux_core::desktop::HorizontalWheelPolicy::Ignore => "Ignore",
                        mux_core::desktop::HorizontalWheelPolicy::Preserve => "Preserve horizontal",
                        mux_core::desktop::HorizontalWheelPolicy::ConvertToVertical => {
                            "Convert to vertical"
                        }
                    }
                ));
                Text::new("Preserve sends to the eligible virtual cursor window.")
                    .caption()
                    .wrap()
                    .show(ui)
                    .on_hover_text("This policy applies only to input that is already horizontal. Preserving it sends the event to the virtual cursor's eligible window. Automatic targeting or concurrent assigned routing is required.");
                if ui.available_width() < 480.0 {
                    ui.label(format!(
                        "Horizontal wheel multiplier · {:.1}×",
                        seat.cursor.horizontal_wheel_scale
                    ));
                    let response = keyboard_slider::accessible_slider(
                        ui,
                        ("horizontal-wheel-scale", index),
                        &format!("Horizontal wheel multiplier for {seat_accessible_name}"),
                        &mut seat.cursor.horizontal_wheel_scale,
                        0.1,
                        8.0,
                        0.1,
                    );
                    if let Some(script) = self.script.as_mut() {
                        script.observe("Horizontal wheel multiplier", &response);
                    }
                } else {
                    let mut response = None;
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "Horizontal wheel multiplier · {:.1}×",
                            seat.cursor.horizontal_wheel_scale
                        ));
                        let width = ui.available_width().clamp(120.0, 180.0);
                        ui.allocate_ui_with_layout(
                            egui::vec2(width, ui.spacing().interact_size.y),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                response = Some(keyboard_slider::accessible_slider(
                                    ui,
                                    ("horizontal-wheel-scale", index),
                                    &format!(
                                        "Horizontal wheel multiplier for {seat_accessible_name}"
                                    ),
                                    &mut seat.cursor.horizontal_wheel_scale,
                                    0.1,
                                    8.0,
                                    0.1,
                                ));
                            },
                        );
                    });
                    if let (Some(script), Some(response)) = (self.script.as_mut(), response.as_ref())
                    {
                        script.observe("Horizontal wheel multiplier", response);
                    }
                }
                Checkbox::new(&mut seat.cursor.invert_horizontal_wheel)
                    .label("Reverse horizontal wheel direction")
                    .show(ui);
                Text::new("Horizontal scale and direction apply to all horizontal output, including emulated output.")
                    .caption()
                    .wrap()
                    .show(ui);
            }
            UserTab::Theme => {
                ui.heading("Cursor appearance");
                let extended_overlay = self.extended_overlay_available();
                let follow_available = self.profile.desktop.is_some()
                    && (!self.session_ready || self.states.get(index)
                        .and_then(|state| state.overlay.application_cursor.as_ref())
                        .is_some_and(|state| state.capability));
                let response = ui.add_enabled_ui(follow_available, |ui| {
                    Checkbox::new(&mut seat.cursor.follow_application)
                        .label("Follow application cursor").show(ui)
                }).inner;
                if let Some(script) = self.script.as_mut() {
                    script.observe("Follow application cursor", &response);
                }
                if !follow_available {
                    Text::new("Application cursors are unavailable here. Your configured shape is used.")
                        .caption().wrap().show(ui);
                } else {
                    Text::new("Uses the selected app's cursor. The preview below is your fallback shape.")
                        .caption().wrap().show(ui);
                }
                if let Some(state) = self.states.get(index)
                    .and_then(|state| state.overlay.application_cursor.as_ref())
                    .filter(|_| seat.cursor.follow_application && self.session_ready)
                {
                    Text::new(&state.state).caption().wrap().show(ui);
                }
                if let Some(state) = self.states.get(index)
                    .filter(|state| self.session_ready && state.overlay.hwnd != 0)
                {
                    let presentation = &state.overlay.presentation;
                    let backend = match presentation.raster_backend {
                        mux_windows::overlay::RasterBackend::Direct2D => "GPU drawing",
                        mux_windows::overlay::RasterBackend::Gdi if seat.cursor.renderer == mux_core::desktop::CursorRenderer::Gdi => "GDI compatibility",
                        mux_windows::overlay::RasterBackend::Gdi => "GDI fallback",
                        _ => "Waiting for the first cursor frame",
                    };
                    Text::new(format!("Cursor display: {backend}"))
                        .caption().wrap().show(ui);
                    if let Some(reason) = &presentation.fallback_reason {
                        Text::new(reason).caption().wrap().show(ui);
                    }
                }
                if let Some(reason) = self.states.get(index)
                    .and_then(|state| state.overlay.system_cursor_fallback.as_deref())
                {
                    Text::new(reason).caption().wrap().show(ui);
                }
                ui.horizontal_wrapped(|ui| {
                    if self.button(ui, "Hide cursor", true) { seat.cursor.visible = false; }
                    if self.button(ui, "Show cursor", true) { seat.cursor.visible = true; }
                });
                let mut visible = seat.cursor.visible;
                let visibility_response = Checkbox::new(&mut visible).label("Show desktop cursor").show(ui);
                if let Some(script) = self.script.as_mut() { script.observe("Show desktop cursor", &visibility_response); }
                if visibility_response.changed() && self.consume_action(ui.ctx(), "Show desktop cursor") {
                    seat.cursor.visible = visible;
                }
                Text::new("Visibility changes cursor appearance only; input routing continues while the cursor is hidden.")
                    .caption().wrap().show(ui);
                let mut idle_enabled = seat.cursor.idle_hide_ms.is_some();
                let idle_response = ui.add_enabled_ui(extended_overlay, |ui| Checkbox::new(&mut idle_enabled)
                    .label("Hide cursor after inactivity").show(ui)).inner;
                if let Some(script) = self.script.as_mut() { script.observe("Hide cursor after inactivity", &idle_response); }
                if idle_response.changed() && self.consume_action(ui.ctx(), "Hide cursor after inactivity") {
                    seat.cursor.idle_hide_ms = idle_enabled.then_some(5_000);
                }
                if let Some(current_timeout) = seat.cursor.idle_hide_ms {
                    let mut timeout_ms = current_timeout as f32;
                    ui.label(format!("Idle timeout · {:.0} ms", timeout_ms));
                    let timeout_response = ui.add_enabled_ui(extended_overlay, |ui| keyboard_slider::accessible_slider(
                        ui, ("idle-timeout", index), &format!("Idle timeout for {seat_accessible_name}"),
                        &mut timeout_ms, 250.0, 600_000.0, 250.0,
                    )).inner;
                    if let Some(script) = self.script.as_mut() { script.observe("Idle timeout", &timeout_response); }
                    if timeout_response.changed() && self.consume_action(ui.ctx(), "Idle timeout") {
                        seat.cursor.idle_hide_ms = Some(timeout_ms.round() as u64);
                    }
                }
                ui.horizontal(|ui| {
                    ui.label("User color");
                    ui.color_edit_button_srgb(&mut seat.cursor.color);
                });
                ui.label(format!("Cursor size · {:.0} pixels", seat.cursor.size));
                let size_response = ui
                    .add_enabled_ui(extended_overlay, |ui| {
                        keyboard_slider::accessible_slider(
                            ui,
                            ("cursor-size", index),
                            &format!("Cursor size for {seat_accessible_name}"),
                            &mut seat.cursor.size,
                            12.0,
                            256.0,
                            1.0,
                        )
                    })
                    .inner;
                if let Some(script) = self.script.as_mut() {
                    script.observe("Cursor size", &size_response);
                }
                ui.horizontal_wrapped(|ui| {
                    for (label, shape) in [
                        ("Arrow cursor", mux_core::desktop::CursorShape::Arrow),
                        ("Crosshair cursor", mux_core::desktop::CursorShape::Crosshair),
                        ("I-beam cursor", mux_core::desktop::CursorShape::IBeam),
                        ("Ring cursor", mux_core::desktop::CursorShape::Ring),
                        ("Windows cursor", mux_core::desktop::CursorShape::System),
                    ] {
                        let enabled = shape != mux_core::desktop::CursorShape::System || extended_overlay;
                        if self.button(ui, label, enabled) {
                            seat.cursor.shape = shape;
                        }
                    }
                });
                if !extended_overlay {
                    Text::new("Windows cursor requires automatic desktop or concurrent assigned routing.").caption().wrap().show(ui);
                }
                if !extended_overlay {
                    ui.colored_label(
                        COLORS[4],
                        "Extended overlay appearance is unavailable in fixed routing. Saved values are retained.",
                    );
                }
                // Keep the sample inside the visible scroll viewport at compact sizes.
                // The content remains useful at short heights and grows to 180 px when
                // more room is available.
                let canvas_size = egui::vec2(
                    ui.available_width().max(0.),
                    // Reserve the enclosing Surface padding and its two borders.
                    (ui.clip_rect().height() - 2. * core::SPACE_2 - 2.).clamp(0., 180.),
                );
                let (response, painter) = ui.allocate_painter(canvas_size, egui::Sense::hover());
                painter.rect_filled(response.rect, core::RADIUS_LG, Theme::get(ui).muted);
                let shape_name = match seat.cursor.shape {
                    mux_core::desktop::CursorShape::Arrow => "Arrow",
                    mux_core::desktop::CursorShape::Crosshair => "Crosshair",
                    mux_core::desktop::CursorShape::IBeam => "I-beam",
                    mux_core::desktop::CursorShape::Ring => "Ring",
                    mux_core::desktop::CursorShape::System => "Windows cursor",
                };
                response.widget_info(|| egui::WidgetInfo::labeled(
                    egui::WidgetType::Label,
                    true,
                    format!("Cursor appearance preview · {shape_name} · {}", if seat.cursor.visible { "visible" } else { "hidden" }),
                ));
                if let Some(script) = self.script.as_mut() {
                    script.observe("Cursor appearance preview", &response);
                    script.observe(
                        if seat.cursor.visible { "Visible cursor preview" } else { "Hidden cursor preview" },
                        &response,
                    );
                }
                if seat.cursor.shape == mux_core::desktop::CursorShape::System
                    && let Some(script) = self.script.as_mut()
                {
                    script.observe("Windows cursor preview", &response);
                }
                let [r, g, b] = seat.cursor.color;
                let color = Color32::from_rgb(r, g, b);
                // Scale the cursor and its associated text/badge together when their
                // combined footprint would exceed this canvas.
                let base_scale = (80. / seat.cursor.size).min(1.);
                let base_font_size = (seat.cursor.label_font_size * base_scale).max(6.);
                let estimated_name_width = if seat.cursor.label {
                    ui.fonts_mut(|fonts| {
                        fonts.layout_no_wrap(
                            seat.name.clone(),
                            egui::FontId::proportional(base_font_size),
                            color,
                        ).size().x
                    })
                } else { 0. };
                let base_badge_galley = seat.cursor.badge.as_deref().map(|badge| {
                    ui.fonts_mut(|fonts| {
                        fonts.layout_no_wrap(
                            badge.to_owned(),
                            egui::FontId::proportional(base_font_size),
                            Color32::WHITE,
                        )
                    })
                });
                let estimated_badge_width = base_badge_galley.as_ref().map_or(0., |galley| {
                    let width = galley.size().x + 12. * base_scale;
                    if seat.cursor.badge_shape == mux_core::desktop::BadgeShape::Circle {
                        width.max(galley.size().y + 8. * base_scale)
                    } else {
                        width
                    }
                });
                let estimated_badge_height = base_badge_galley.as_ref().map_or(0., |galley| {
                    let height = galley.size().y + 8. * base_scale;
                    if seat.cursor.badge_shape == mux_core::desktop::BadgeShape::Circle {
                        height.max(estimated_badge_width)
                    } else {
                        height
                    }
                });
                let core_size = seat.cursor.size * base_scale;
                let footprint_width = (core_size + estimated_name_width + estimated_badge_width + 36. * base_scale).max(core_size);
                let footprint_height = core_size + if seat.cursor.label || seat.cursor.badge.is_some() { (base_font_size + 16. * base_scale).max(estimated_badge_height) } else { 0. };
                let fit_scale = ((response.rect.width() - 20.).max(1.) / footprint_width.max(1.))
                    .min((response.rect.height() - 20.).max(1.) / footprint_height.max(1.))
                    .min(1.);
                let preview_scale = base_scale * fit_scale;
                let size = seat.cursor.size * preview_scale;
                let origin = response.rect.center() - egui::vec2(size * 0.5, size * 0.5);
                let cursor_rect = egui::Rect::from_min_size(origin, egui::vec2(size, size));
                let system_texture = if seat.cursor.shape == mux_core::desktop::CursorShape::System {
                    if !self.theme_cursor_textures.contains_key(&seat.cursor.color)
                        && let Some(image) = mux_windows::overlay::theme_cursor_preview(seat.cursor.color)
                    {
                        let mut rgba = Vec::with_capacity(image.pixels.len());
                        for bgra in image.pixels.as_chunks::<4>().0 {
                            rgba.extend_from_slice(&[bgra[2], bgra[1], bgra[0], bgra[3]]);
                        }
                        let color_image = egui::ColorImage::from_rgba_premultiplied(
                            [image.width as usize, image.height as usize],
                            &rgba,
                        );
                        if self.theme_cursor_textures.len() >= 16
                            && let Some(oldest_color) = self.theme_cursor_textures.keys().next().copied()
                        {
                            self.theme_cursor_textures.remove(&oldest_color);
                        }
                        let texture = ui.ctx().load_texture(
                            format!("theme-windows-arrow-{}-{}-{}", seat.cursor.color[0], seat.cursor.color[1], seat.cursor.color[2]),
                            color_image,
                            egui::TextureOptions::LINEAR,
                        );
                        self.theme_cursor_textures.insert(seat.cursor.color, texture);
                    }
                    self.theme_cursor_textures
                        .get(&seat.cursor.color)
                        .map(|texture| (texture.id(), texture.size_vec2()))
                } else {
                    None
                };
                if seat.cursor.visible {
                match seat.cursor.shape {
                    mux_core::desktop::CursorShape::Arrow => {
                        painter.add(egui::Shape::convex_polygon(
                            vec![origin, origin + egui::vec2(0., size), origin + egui::vec2(size * 0.3, size * 0.7), origin + egui::vec2(size * 0.8, size * 0.6)],
                            color,
                            egui::Stroke::new(1.5_f32, Color32::WHITE),
                        ));
                    }
                    mux_core::desktop::CursorShape::Crosshair => {
                        painter.line_segment([origin + egui::vec2(size * 0.5, 0.), origin + egui::vec2(size * 0.5, size)], egui::Stroke::new(2.0_f32, color));
                        painter.line_segment([origin + egui::vec2(0., size * 0.5), origin + egui::vec2(size, size * 0.5)], egui::Stroke::new(2.0_f32, color));
                        painter.circle_stroke(origin + egui::vec2(size * 0.5, size * 0.5), 3.0_f32, egui::Stroke::new(1.5_f32, color));
                    }
                    mux_core::desktop::CursorShape::IBeam => {
                        let mid = origin.x + size * 0.5;
                        painter.line_segment([egui::pos2(mid, origin.y), egui::pos2(mid, origin.y + size)], egui::Stroke::new(2.0_f32, color));
                        painter.line_segment([egui::pos2(mid - 5.0_f32, origin.y), egui::pos2(mid + 5.0_f32, origin.y)], egui::Stroke::new(2.0_f32, color));
                        painter.line_segment([egui::pos2(mid - 5.0_f32, origin.y + size), egui::pos2(mid + 5.0_f32, origin.y + size)], egui::Stroke::new(2.0_f32, color));
                    }
                    mux_core::desktop::CursorShape::Ring => {
                        painter.circle_stroke(origin + egui::vec2(size * 0.5, size * 0.5), size * 0.4, egui::Stroke::new(2.0_f32, color));
                    }
                    mux_core::desktop::CursorShape::System => {
                        if let Some((texture_id, texture_size)) = system_texture {
                            let fit_scale = (cursor_rect.width() / texture_size.x)
                                .min(cursor_rect.height() / texture_size.y);
                            let fitted_size = texture_size * fit_scale;
                            let fitted_rect = egui::Rect::from_center_size(
                                cursor_rect.center(),
                                fitted_size,
                            );
                            painter.image(
                                texture_id,
                                fitted_rect,
                                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1., 1.)),
                                Color32::WHITE,
                            );
                        } else {
                            painter.text(
                                response.rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "Windows arrow preview unavailable",
                                egui::FontId::proportional(12.),
                                Theme::get(ui).muted_foreground,
                            );
                        }
                    }
                }
                } else {
                    painter.text(
                        response.rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "Cursor hidden",
                        egui::FontId::proportional(14.),
                        Theme::get(ui).muted_foreground,
                    );
                }
                let mut preview_name_width = 0.;
                if seat.cursor.visible && seat.cursor.label {
                    let name_size = (seat.cursor.label_font_size * preview_scale).max(6.);
                    let name_font = egui::FontId::proportional(name_size);
                    let display_name = bounded_display_text(
                        ui,
                        &seat.name,
                        name_font.clone(),
                        (response.rect.width() - 16.).max(0.),
                    );
                    preview_name_width = ui.fonts_mut(|fonts| {
                        fonts
                            .layout_no_wrap(display_name.clone(), name_font.clone(), color)
                            .size()
                            .x
                    });
                    let name_pos = match seat.cursor.label_position {
                        mux_core::desktop::LabelPosition::Right => (cursor_rect.right_top() + egui::vec2(8. * preview_scale, 0.), egui::Align2::LEFT_TOP),
                        mux_core::desktop::LabelPosition::Left => (cursor_rect.left_top() - egui::vec2(8. * preview_scale, 0.), egui::Align2::RIGHT_TOP),
                        mux_core::desktop::LabelPosition::Above => (cursor_rect.center_top() - egui::vec2(0., 5. * preview_scale), egui::Align2::CENTER_BOTTOM),
                        mux_core::desktop::LabelPosition::Below => (cursor_rect.center_bottom() + egui::vec2(0., 5. * preview_scale), egui::Align2::CENTER_TOP),
                    };
                    let text_height = ui.fonts_mut(|fonts| fonts.row_height(&name_font));
                    let text_rect = egui::Rect::from_min_size(
                        match seat.cursor.label_position {
                            mux_core::desktop::LabelPosition::Right => name_pos.0,
                            mux_core::desktop::LabelPosition::Left => name_pos.0 - egui::vec2(preview_name_width, 0.),
                            mux_core::desktop::LabelPosition::Above => name_pos.0 - egui::vec2(preview_name_width * 0.5, text_height),
                            mux_core::desktop::LabelPosition::Below => name_pos.0 - egui::vec2(preview_name_width * 0.5, 0.),
                        },
                        egui::vec2(preview_name_width, text_height),
                    );
                    let contained = text_rect.translate(egui::vec2(
                        (response.rect.left() + 8. - text_rect.left()).max(0.) - (text_rect.right() - response.rect.right() + 8.).max(0.),
                        (response.rect.top() + 8. - text_rect.top()).max(0.) - (text_rect.bottom() - response.rect.bottom() + 8.).max(0.),
                    ));
                    let aligned_pos = match seat.cursor.label_position {
                        mux_core::desktop::LabelPosition::Right => contained.left_top(),
                        mux_core::desktop::LabelPosition::Left => contained.right_top(),
                        mux_core::desktop::LabelPosition::Above => contained.center_bottom(),
                        mux_core::desktop::LabelPosition::Below => contained.center_top(),
                    };
                    painter.text(
                        aligned_pos,
                        name_pos.1,
                        display_name,
                        name_font,
                        color,
                    );
                }
                if seat.cursor.visible && let Some(badge) = seat.cursor.badge.as_deref() {
                    let badge_font_size = (seat.cursor.label_font_size * preview_scale).max(6.);
                    let badge_font = egui::FontId::proportional(badge_font_size);
                    let badge_galley = ui.fonts_mut(|fonts| {
                        fonts.layout_no_wrap(badge.to_owned(), badge_font.clone(), Color32::WHITE)
                    });
                    let badge_size = {
                        let size = egui::vec2(
                            badge_galley.size().x + 12. * preview_scale,
                            badge_galley.size().y + 8. * preview_scale,
                        );
                        if seat.cursor.badge_shape == mux_core::desktop::BadgeShape::Circle {
                            egui::vec2(size.x.max(size.y), size.x.max(size.y))
                        } else {
                            size
                        }
                    };
                    let badge_min = match seat.cursor.badge_position {
                        mux_core::desktop::BadgePosition::Inline => {
                            let label_advance = if seat.cursor.label
                                && seat.cursor.label_position
                                    == mux_core::desktop::LabelPosition::Right
                            {
                                preview_name_width + 4.
                            } else {
                                0.
                            };
                            cursor_rect.right_top() + egui::vec2(8. + label_advance, 0.)
                        }
                        mux_core::desktop::BadgePosition::TopLeft => {
                            cursor_rect.left_top() - badge_size - egui::vec2(4., 4.)
                        }
                        mux_core::desktop::BadgePosition::TopRight => {
                            cursor_rect.right_top() + egui::vec2(4., -4.)
                        }
                        mux_core::desktop::BadgePosition::BottomLeft => {
                            cursor_rect.left_bottom() - badge_size + egui::vec2(-4., 4.)
                        }
                        mux_core::desktop::BadgePosition::BottomRight => {
                            cursor_rect.right_bottom() + egui::vec2(4., -badge_size.y + 4.)
                        }
                    };
                    let mut badge_rect = egui::Rect::from_min_size(badge_min, badge_size);
                    let correction = egui::vec2(
                        (response.rect.left() - badge_rect.left()).max(0.)
                            - (badge_rect.right() - response.rect.right()).max(0.),
                        (response.rect.top() - badge_rect.top()).max(0.)
                            - (badge_rect.bottom() - response.rect.bottom()).max(0.),
                    );
                    badge_rect = badge_rect.translate(correction);
                    match seat.cursor.badge_shape {
                        mux_core::desktop::BadgeShape::Rounded => {
                            painter.rect_filled(badge_rect, 6., color);
                        }
                        mux_core::desktop::BadgeShape::Box => {
                            painter.rect_filled(badge_rect, 0., color);
                        }
                        mux_core::desktop::BadgeShape::Circle => {
                            painter.circle_filled(
                                badge_rect.center(),
                                badge_rect.width().max(badge_rect.height()) * 0.5,
                                color,
                            );
                        }
                    }
                    painter.galley(
                        badge_rect.center() - badge_galley.size() * 0.5,
                        badge_galley,
                        Color32::WHITE,
                    );
                }
            }
            UserTab::Overlay => {
                ui.heading("Desktop overlay");
                let extended_overlay = self.extended_overlay_available();
                ui.label("Cursor renderer");
                let renderers = ["Auto · GPU drawing", "GDI compatibility"];
                let selected = match seat.cursor.renderer {
                    mux_core::desktop::CursorRenderer::Auto => renderers[0],
                    mux_core::desktop::CursorRenderer::Gdi => renderers[1],
                };
                if let Some(choice) = self.choice_menu(
                    ui, "Cursor renderer selector", selected, &renderers, extended_overlay,
                ) {
                    seat.cursor.renderer = if choice == 1 {
                        mux_core::desktop::CursorRenderer::Gdi
                    } else {
                        mux_core::desktop::CursorRenderer::Auto
                    };
                }
                ui.weak("Auto draws on the GPU and falls back to GDI if unavailable. GDI uses CPU drawing.");
                let old_label = seat.cursor.label;
                let mut show_label = old_label;
                let response = Checkbox::new(&mut show_label)
                    .label("Show user name beside cursor")
                    .show(ui);
                if let Some(script) = self.script.as_mut() {
                    script.observe("Show user name beside cursor", &response);
                }
                if response.changed()
                    && self.consume_action(ui.ctx(), "Show user name beside cursor")
                {
                    seat.cursor.label = show_label;
                }
                ui.weak("The short badge is shown independently of the user-name label.");
                ui.label("Short badge (up to 8 characters)");
                let mut badge = seat.cursor.badge.clone().unwrap_or_default();
                let badge_response = Input::new(&mut badge)
                    .id_source("cursor_badge")
                    .show(ui);
                if let Some(script) = self.script.as_mut() {
                    script.observe("Cursor badge text", &badge_response);
                }
                if badge_response.changed()
                    && self.consume_action(ui.ctx(), "Cursor badge text")
                {
                    seat.cursor.badge = (!badge.is_empty()).then_some(badge.chars().take(8).collect());
                }
                ui.label("Badge shape");
                let badge_shapes = ["Badge Rounded", "Badge Box", "Badge Circle"];
                let badge_shape = match seat.cursor.badge_shape {
                    mux_core::desktop::BadgeShape::Rounded => "Badge Rounded",
                    mux_core::desktop::BadgeShape::Box => "Badge Box",
                    mux_core::desktop::BadgeShape::Circle => "Badge Circle",
                };
                if let Some(choice) = self.choice_menu(
                    ui,
                    "Badge shape selector",
                    badge_shape,
                    &badge_shapes,
                    extended_overlay,
                ) {
                    seat.cursor.badge_shape = match choice {
                        1 => mux_core::desktop::BadgeShape::Box,
                        2 => mux_core::desktop::BadgeShape::Circle,
                        _ => mux_core::desktop::BadgeShape::Rounded,
                    };
                }
                ui.label("Badge placement");
                let badge_positions = [
                    "Badge Inline",
                    "Badge Top left",
                    "Badge Top right",
                    "Badge Bottom left",
                    "Badge Bottom right",
                ];
                let badge_position = match seat.cursor.badge_position {
                    mux_core::desktop::BadgePosition::Inline => "Badge Inline",
                    mux_core::desktop::BadgePosition::TopLeft => "Badge Top left",
                    mux_core::desktop::BadgePosition::TopRight => "Badge Top right",
                    mux_core::desktop::BadgePosition::BottomLeft => "Badge Bottom left",
                    mux_core::desktop::BadgePosition::BottomRight => "Badge Bottom right",
                };
                if let Some(choice) = self.choice_menu(
                    ui,
                    "Badge position selector",
                    badge_position,
                    &badge_positions,
                    extended_overlay,
                ) {
                    seat.cursor.badge_position = match choice {
                        1 => mux_core::desktop::BadgePosition::TopLeft,
                        2 => mux_core::desktop::BadgePosition::TopRight,
                        3 => mux_core::desktop::BadgePosition::BottomLeft,
                        4 => mux_core::desktop::BadgePosition::BottomRight,
                        _ => mux_core::desktop::BadgePosition::Inline,
                    };
                }
                ui.label("User-name label placement");
                let label_positions = ["Label Right", "Label Left", "Label Above", "Label Below"];
                let label_position = match seat.cursor.label_position {
                    mux_core::desktop::LabelPosition::Right => "Label Right",
                    mux_core::desktop::LabelPosition::Left => "Label Left",
                    mux_core::desktop::LabelPosition::Above => "Label Above",
                    mux_core::desktop::LabelPosition::Below => "Label Below",
                };
                if let Some(choice) = self.choice_menu(ui, "Label position selector", label_position, &label_positions, extended_overlay && seat.cursor.label) {
                    seat.cursor.label_position = match choice {
                        1 => mux_core::desktop::LabelPosition::Left,
                        2 => mux_core::desktop::LabelPosition::Above,
                        3 => mux_core::desktop::LabelPosition::Below,
                        _ => mux_core::desktop::LabelPosition::Right,
                    };
                }
                ui.label(format!("Overlay text size · {:.0} px", seat.cursor.label_font_size));
                ui.weak("This size applies to both the user-name label and short badge text.");
                let font_response = ui.add_enabled_ui(extended_overlay && (seat.cursor.label || seat.cursor.badge.is_some()), |ui| {
                    keyboard_slider::accessible_slider(ui, ("label-font-size", index), &format!("Overlay text size for {seat_accessible_name}"), &mut seat.cursor.label_font_size, 8.0, 48.0, 1.0)
                }).inner;
                if let Some(script) = self.script.as_mut() { script.observe("Label font size", &font_response); }
                if let Err(error) = seat.cursor.validate() {
                    ui.colored_label(COLORS[1], format!("Invalid overlay settings: {error}"));
                }
                if !extended_overlay {
                    ui.colored_label(
                        COLORS[4],
                        "Extended overlay controls are unavailable in fixed routing. Saved values are retained.",
                    );
                }
                ui.label("Cursors stay above applications and allow clicks through. They do not take keyboard focus.");
            }
            UserTab::Lock => {
                let mode_labels = [
                    "Legacy bounds",
                    "Desktop bounds",
                    "Window bounds",
                    "Monitor bounds",
                    "Region bounds",
                ];
                let selected_mode = match seat.cursor.bounds_mode {
                    mux_core::desktop::BoundsMode::Legacy => mode_labels[0],
                    mux_core::desktop::BoundsMode::Desktop => mode_labels[1],
                    mux_core::desktop::BoundsMode::Window => mode_labels[2],
                    mux_core::desktop::BoundsMode::Monitor => mode_labels[3],
                    mux_core::desktop::BoundsMode::Region => mode_labels[4],
                };
                if let Some(choice) = self.choice_menu(
                    ui,
                    "Movement bounds",
                    selected_mode,
                    &mode_labels,
                    true,
                ) {
                    seat.cursor.bounds_mode = match choice {
                        1 => mux_core::desktop::BoundsMode::Desktop,
                        2 => mux_core::desktop::BoundsMode::Window,
                        3 => mux_core::desktop::BoundsMode::Monitor,
                        4 => mux_core::desktop::BoundsMode::Region,
                        _ => mux_core::desktop::BoundsMode::Legacy,
                    };
                    if seat.cursor.bounds_mode == mux_core::desktop::BoundsMode::Monitor
                        && seat.cursor.monitor_id.is_none()
                    {
                        seat.cursor.monitor_id = self
                            .monitors
                            .iter()
                            .find(|monitor| monitor.primary && monitor.stable_id)
                            .map(|monitor| monitor.id.clone());
                    }
                    if seat.cursor.bounds_mode == mux_core::desktop::BoundsMode::Region {
                        self.seed_region_draft(index);
                        seat.cursor.region = Some(self.region_draft);
                    }
                }
                let draft_mode_label = match seat.cursor.bounds_mode {
                    mux_core::desktop::BoundsMode::Legacy => "Legacy",
                    mux_core::desktop::BoundsMode::Desktop => "Desktop",
                    mux_core::desktop::BoundsMode::Window => "Window",
                    mux_core::desktop::BoundsMode::Monitor => "Monitor",
                    mux_core::desktop::BoundsMode::Region => "Region",
                };
                let applied_mode = self
                    .session
                    .as_ref()
                    .and_then(|_| self.virtual_users_page.snapshot().state)
                    .and_then(|state| {
                        state
                            .slots
                            .get(index)
                            .map(|slot| slot.seat.cursor.bounds_mode)
                    });
                let applied_label = match (self.session.is_some(), applied_mode) {
                    (false, _) => "not running".to_owned(),
                    (true, Some(mode)) => match mode {
                        mux_core::desktop::BoundsMode::Legacy => "Legacy".to_owned(),
                        mux_core::desktop::BoundsMode::Desktop => "Desktop".to_owned(),
                        mux_core::desktop::BoundsMode::Window => "Window".to_owned(),
                        mux_core::desktop::BoundsMode::Monitor => "Monitor".to_owned(),
                        mux_core::desktop::BoundsMode::Region => "Region".to_owned(),
                    },
                    (true, None) => "waiting for worker state".to_owned(),
                };
                let draft_summary = Text::new(format!(
                    "Draft: {draft_mode_label} · Applied: {applied_label}"
                ))
                .caption()
                .show(ui);
                if let Some(script) = self.script.as_mut() {
                    script.observe("Lock draft summary", &draft_summary);
                }
                match seat.cursor.bounds_mode {
                    mux_core::desktop::BoundsMode::Legacy => {
                        Checkbox::new(&mut seat.cursor.lock_window)
                            .label("Keep cursor inside assigned window")
                            .show(ui);
                        Text::new("Legacy mode keeps the existing profile behavior.")
                            .caption()
                            .show(ui);
                    }
                    mux_core::desktop::BoundsMode::Desktop => {
                        Text::new("Cursor movement is clipped to the virtual desktop bounds.")
                            .caption()
                            .show(ui);
                    }
                    mux_core::desktop::BoundsMode::Window => {
                        self.live_window_selection_controls(ui, index, true);
                    }
                    mux_core::desktop::BoundsMode::Monitor | mux_core::desktop::BoundsMode::Region => {
                        if let Some(error) = &self.monitor_error {
                            ui.colored_label(COLORS[1], format!("Display inventory unavailable: {error}"));
                        }
                        let selectable: Vec<_> = self.monitors.iter().filter(|m| m.stable_id).collect();
                        if seat.cursor.bounds_mode == mux_core::desktop::BoundsMode::Monitor {
                            let mut selected_monitor = seat
                                .cursor
                                .monitor_id
                                .as_ref()
                                .and_then(|id| selectable.iter().position(|monitor| monitor.id == *id))
                                .unwrap_or(usize::MAX);
                            if Select::new(&mut selected_monitor)
                                .options(selectable.iter().map(|monitor| {
                                    format!(
                                        "{}{}",
                                        if monitor.primary { "Primary · " } else { "" },
                                        monitor.name
                                    )
                                }))
                                .placeholder("Choose a connected display")
                                .show(ui)
                                .changed()
                                && let Some(monitor) = selectable.get(selected_monitor)
                            {
                                seat.cursor.monitor_id = Some(monitor.id.clone());
                            }
                            Checkbox::new(&mut seat.cursor.monitor_work_area)
                                .label("Use display work area")
                                .show(ui);
                        } else {
                            let mut selected_monitor = seat
                                .cursor
                                .monitor_id
                                .as_ref()
                                .and_then(|id| selectable.iter().position(|monitor| monitor.id == *id))
                                .map_or(0, |i| i + 1);
                            let mut choices = vec!["Entire desktop · no monitor clip".to_owned()];
                            choices.extend(selectable.iter().map(|monitor| {
                                format!(
                                    "{}{}",
                                    if monitor.primary { "Primary · " } else { "" },
                                    monitor.name
                                )
                            }));
                            if Select::new(&mut selected_monitor)
                                .options(choices)
                                .placeholder("Choose a region clip")
                                .show(ui)
                                .changed()
                            {
                                seat.cursor.monitor_id = selected_monitor
                                    .checked_sub(1)
                                    .and_then(|i| selectable.get(i))
                                    .map(|monitor| monitor.id.clone());
                            }
                            if seat.cursor.monitor_id.is_some() {
                                Checkbox::new(&mut seat.cursor.monitor_work_area)
                                    .label("Clip to display work area")
                                    .show(ui);
                            }
                        }
                        if let Some(monitor) = seat
                            .cursor
                            .monitor_id
                            .as_deref()
                            .and_then(|id| self.monitors.iter().find(|monitor| monitor.id == id))
                        {
                            Text::new(format!("Display: {}", monitor.name))
                                .caption()
                                .show(ui)
                                .on_hover_text(&monitor.id);
                        }
                        if seat.cursor.bounds_mode == mux_core::desktop::BoundsMode::Region {
                            self.seed_region_draft(index);
                            Text::new("Region coordinates use global desktop pixels.")
                                .caption()
                                .show(ui);
                            for (label, value) in [
                                ("Left", &mut self.region_draft.left),
                                ("Top", &mut self.region_draft.top),
                                ("Right", &mut self.region_draft.right),
                                ("Bottom", &mut self.region_draft.bottom),
                            ] {
                                let row = ui.horizontal(|ui| {
                                    ui.label(label);
                                    ui.add_sized(
                                        [ui.available_width(), ui.spacing().interact_size.y],
                                        egui::DragValue::new(value).range(-1_000_000..=1_000_000),
                                    );
                                });
                                if let Some(script) = self.script.as_mut() {
                                    script.observe(&format!("Lock region {label}"), &row.response);
                                }
                            }
                            seat.cursor.region = Some(self.region_draft);
                        }
                        Collapsible::new("Bounds details")
                            .id_source(("lock-bounds-details", seat.identity()))
                            .default_open(false)
                            .show(ui, |ui| {
                                if let Some(monitor) = seat
                                    .cursor
                                    .monitor_id
                                    .as_deref()
                                    .and_then(|id| self.monitors.iter().find(|monitor| monitor.id == id))
                                {
                                    ui.label(format!("Stable monitor path: {}", monitor.id));
                                    ui.label(format!("Physical bounds: {:?}", monitor.bounds));
                                    ui.label(format!("Work area: {:?}", monitor.work_area));
                                }
                                if let Some(state) = self.states.get(index) {
                                    if state.pid != 0 && state.bounds.valid() {
                                        ui.label(format!("Target client bounds: {:?}", state.bounds));
                                    }
                                    ui.label(format!("Effective movement bounds: {:?}", state.movement_bounds));
                                }
                            });
                    }
                }
                if let Some(error) = self.cursor_settings_error(index, &seat.cursor) {
                    ui.colored_label(COLORS[1], format!("Cannot apply these bounds: {error}"));
                }
            }
            UserTab::Keyboard => {
                ui.spacing_mut().item_spacing.y = core::SPACE_1;
                let virtual_keyboard = seat
                    .keyboard
                    .as_deref()
                    .and_then(|p| emulated::parse(p).ok().flatten())
                    .filter(|device| device.kind == DeviceKind::Keyboard);
                let virtual_mouse = emulated::parse(&seat.mouse)
                    .ok()
                    .flatten()
                    .is_some_and(|device| device.kind == DeviceKind::Mouse);
                let keyboard_delivery_supported = virtual_mouse
                    && virtual_keyboard.is_some()
                    && (self.profile.concurrent || self.profile.desktop.is_some());
                let unsupported_reason = if virtual_keyboard.is_none() {
                    if seat.keyboard.is_some() {
                        "Keyboard routing requires a virtual keyboard."
                    } else {
                        "Pair a virtual keyboard to configure delivery."
                    }
                } else if !virtual_mouse {
                    "Keyboard delivery requires a virtual mouse and keyboard."
                } else {
                    "Keyboard delivery requires concurrent or automatic routing."
                };

                let mut enabled = seat.cursor.keyboard.enabled;
                let enabled_response = Checkbox::new(&mut enabled)
                    .label("Enable keyboard delivery")
                    .enabled(keyboard_delivery_supported)
                    .show(ui);
                let accepted = self
                    .script
                    .as_mut()
                    .is_none_or(|script| script.observe("Keyboard enabled", &enabled_response));
                if enabled_response.changed()
                    && accepted
                    && self.consume_action(ui.ctx(), "Keyboard enabled")
                {
                    seat.cursor.keyboard.enabled = enabled;
                }

                let application_focus_supported =
                    virtual_mouse && virtual_keyboard.is_some() && self.profile.desktop.is_some();
                let application_focus_idle = self.session.is_none()
                    && !self.session_busy
                    && !self.session_stopping;
                ui.add_space(core::SPACE_1);
                Text::new("Application focus").caption().show(ui);
                ui.horizontal_wrapped(|ui| {
                    for (label, application_focus) in [
                        (
                            "Compatibility",
                            mux_core::keyboard::ApplicationFocus::Compatibility,
                        ),
                        (
                            "Native controls",
                            mux_core::keyboard::ApplicationFocus::NativeControls,
                        ),
                    ] {
                        let choice_enabled = application_focus_idle
                            && (application_focus_supported
                                || application_focus
                                    == mux_core::keyboard::ApplicationFocus::Compatibility);
                        let response = ouroboros_ui::atoms::Radio::new(
                            seat.cursor.keyboard.application_focus == application_focus,
                        )
                        .label(label)
                        .enabled(choice_enabled)
                        .show(ui);
                        let accepted = self
                            .script
                            .as_mut()
                            .is_none_or(|script| script.observe(label, &response));
                        if response.clicked()
                            && accepted
                            && choice_enabled
                            && self.consume_action(ui.ctx(), label)
                        {
                            seat.cursor.keyboard.application_focus = application_focus;
                        }
                    }
                });

                let focus_options = [
                    ("Focused control", mux_core::keyboard::KeyboardFocus::Focused),
                    ("Clicked control", mux_core::keyboard::KeyboardFocus::Clicked),
                    ("Direct lookup", mux_core::keyboard::KeyboardFocus::Native),
                    ("Active window", mux_core::keyboard::KeyboardFocus::Active),
                    ("Caret window", mux_core::keyboard::KeyboardFocus::Caret),
                ];
                let selected_focus = focus_options
                    .iter()
                    .find(|(_, focus)| seat.cursor.keyboard.focus == *focus)
                    .map_or(focus_options[0].0, |(label, _)| *label);
                let mut chosen_focus = None;
                ui.add_enabled_ui(keyboard_delivery_supported, |ui| {
                    let trigger = Button::new(format!("Recipient: {selected_focus}"))
                        .icon_right(ouroboros_ui::egui_phosphor::light::CARET_DOWN)
                        .variant(ButtonVariant::Outline)
                        .show(ui);
                    if let Some(script) = self.script.as_mut() {
                        script.observe("Keyboard recipient menu", &trigger);
                    }
                    let ctx = ui.ctx().clone();
                    egui::Popup::menu(&trigger).show(|menu| {
                        for (label, focus) in focus_options {
                            let option = MenuItem::new(label)
                                .checked(seat.cursor.keyboard.focus == focus)
                                .enabled(keyboard_delivery_supported)
                                .id_source(("keyboard-recipient", label))
                                .show(menu);
                            let action = format!("Keyboard recipient: {label}");
                            let accepted = self
                                .script
                                .as_mut()
                                .is_none_or(|script| script.observe(&action, &option));
                            if option.clicked()
                                && accepted
                                && keyboard_delivery_supported
                                && self.consume_action(&ctx, &action)
                            {
                                chosen_focus = Some(focus);
                                menu.close();
                            }
                        }
                    });
                });
                if let Some(focus) = chosen_focus {
                    seat.cursor.keyboard.focus = focus;
                }
                let focus_help = match seat.cursor.keyboard.focus {
                    mux_core::keyboard::KeyboardFocus::Focused => {
                        "Recommended - retries focused-control lookup up to three times."
                    }
                    mux_core::keyboard::KeyboardFocus::Clicked => {
                        "Uses the last control clicked in the assigned application."
                    }
                    mux_core::keyboard::KeyboardFocus::Native => "Performs one direct focus lookup.",
                    mux_core::keyboard::KeyboardFocus::Active => {
                        "Uses the active window in the assigned application."
                    }
                    mux_core::keyboard::KeyboardFocus::Caret => {
                        "Uses the caret window, then focused and clicked controls."
                    }
                };
                ui.collapsing("Delivery help", |ui| {
                    if !keyboard_delivery_supported {
                        Text::new(unsupported_reason).caption().wrap().show(ui);
                    }
                    Text::new(focus_help).caption().wrap().show(ui);
                    Text::new("Compatibility uses simulated focus. Native controls use the application's own focus and require automatic desktop routing.")
                        .caption().wrap().show(ui);
                    if !application_focus_idle {
                        Text::new("Stop routing before changing application focus.")
                            .caption().wrap().show(ui);
                    }
                });

                if let Some(state) = self.states.get(index)
                    && let Some(live) = &state.keyboard
                {
                    ui.add_space(core::SPACE_1);
                    let live_enabled = if live.settings.enabled { "On" } else { "Off" };
                    let live_focus = focus_options
                        .iter()
                        .find(|(_, focus)| live.settings.focus == *focus)
                        .map_or("Unknown", |(label, _)| *label);
                    ui.weak(format!("Live: {live_enabled}; {live_focus}"));
                    if seat.cursor.keyboard != live.settings {
                        ui.colored_label(COLORS[4], "Draft differs from live settings. Apply to update routing.");
                    }
                    ui.collapsing("Delivery diagnostics", |ui| {
                        ui.label(&live.status);
                        if let Some(window) = live.resolved {
                            ui.monospace(format!("Recipient HWND {:X}; PID {}", window.hwnd, window.pid));
                        } else {
                            ui.weak("No recipient window is resolved yet.");
                        }
                        if live.fallback {
                            ui.weak("Fallback recipient selection is active.");
                        }
                        if let Some(window) = live.clicked {
                            ui.monospace(format!("Last clicked HWND {:X}; PID {}", window.hwnd, window.pid));
                        }
                    });
                }

                let keyboards: Vec<_> = self
                    .devices
                    .iter()
                    .filter(|device| device.kind == DeviceKind::Keyboard)
                    .chain(virtual_keyboard.iter())
                    .cloned()
                    .collect();
                let mut selected = seat.keyboard.as_ref().map_or(0, |path| {
                    keyboards
                        .iter()
                        .position(|device| device.path.eq_ignore_ascii_case(path))
                        .map_or(usize::MAX, |position| position + 1)
                });
                let mut options = vec!["No keyboard".to_owned()];
                options.extend(keyboards.iter().enumerate().map(|(i, device)| {
                    format!(
                        "Keyboard {}: {} packets",
                        i + 1,
                        self.keyboard_packets.get(&device.handle).copied().unwrap_or(0)
                    )
                }));
                let pairing_enabled = self.session.is_none();
                ui.add_space(core::SPACE_1);
                ui.collapsing("Assigned keyboard", |ui| {
                let select_response = ui
                    .add_enabled_ui(pairing_enabled, |ui| {
                        Select::new(&mut selected)
                            .options(options)
                            .placeholder("Assigned keyboard is offline")
                            .show(ui)
                    })
                    .inner;
                let accepted = self.script.as_mut().is_none_or(|script| {
                    script.observe("Keyboard identity selector", &select_response)
                });
                if select_response.changed()
                    && accepted
                    && self.consume_action(ui.ctx(), "Keyboard identity selector")
                {
                    seat.keyboard = selected
                        .checked_sub(1)
                        .and_then(|i| keyboards.get(i))
                        .map(|device| device.path.clone());
                }
                if let Some(path) = &seat.keyboard {
                    ui.collapsing("Device path", |ui| {
                        Text::new(path).caption().wrap().show(ui);
                    });
                }
                if pairing_enabled {
                    ui.weak("Profile validation prevents assigning a device to more than one user.");
                } else {
                    ui.weak("Stop routing to change the assigned keyboard.");
                }
                });
            }
            UserTab::Target => {
                ui.label(if self.profile.desktop.is_some(){"Starting application (optional). Click windows to choose targets while routing."}else{"Choose the application this user controls"});
                if Button::new("Open routing test window")
                    .outline()
                    .show(ui)
                    .clicked()
                {
                    use std::os::windows::process::CommandExt;
                    let result = std::env::current_exe().and_then(|exe| {
                        std::process::Command::new(exe)
                            .arg("probe")
                            .creation_flags(0x08000000)
                            .spawn()
                    });
                    self.status = match result {
                        Ok(child) => {
                            seat.target_pid = child.id();
                            format!("Test process {} selected.", child.id())
                        }
                        Err(e) => format!("Open test window: {e}"),
                    };
                }
                let mut selected = self
                    .targets
                    .iter()
                    .position(|t| t.pid == seat.target_pid)
                    .unwrap_or(usize::MAX);
                if Select::new(&mut selected)
                    .options(self.targets.iter().map(|t| {
                        format!(
                            "{} (PID {})",
                            t.title.chars().take(48).collect::<String>(),
                            t.pid
                        )
                    }))
                    .placeholder("Select an application")
                    .show(ui)
                    .changed()
                {
                    seat.target_pid = self.targets[selected].pid;
                }
                ui.horizontal(|ui| {
                    ui.label("Process ID");
                    ui.add(egui::DragValue::new(&mut seat.target_pid).range(0..=u32::MAX));
                });
                ui.label("Use a separate process for each user. Update the PID after restarting the application.");
                if self.session.is_none() && seat.target_pid != 0 {
                    if self.discovery_current() && let Some(readiness) = self.target_readiness.get(&seat.target_pid) {
                        Text::new(&readiness.reason).muted().wrap().show(ui);
                    } else { Text::new("Checking this application…").caption().wrap().show(ui); }
                }
                ui.weak("Start, pause and stop routing from Home.");
                if Button::new("Open profiles & backend")
                    .outline()
                    .show(ui)
                    .clicked()
                {
                    self.go_to_page(ui.ctx(), Page::Profiles);
                }
            }
            UserTab::Draw | UserTab::Magnifier | UserTab::Macros => {}
                            });
                        });
                });
            },
        );
        self.profile.seats[index] = seat;
        if remove_user {
            self.profile.seats.remove(index);
            self.replication_page.refresh_seats(&self.profile.seats);
            self.replication_edges = self
                .profile
                .replication
                .resolve(&self.profile.seats)
                .unwrap_or_default();
            self.selected = None;
            return;
        }

        let validation_error = self.cursor_settings_error(index, &self.profile.seats[index].cursor);
        let keyboard_apply_supported = self.user_tab != UserTab::Keyboard
            || emulated::parse(&self.profile.seats[index].mouse)
                .ok()
                .flatten()
                .is_some_and(|device| device.kind == DeviceKind::Mouse)
                && self.profile.seats[index]
                    .keyboard
                    .as_deref()
                    .and_then(|path| emulated::parse(path).ok().flatten())
                    .is_some_and(|device| device.kind == DeviceKind::Keyboard)
                && (self.profile.concurrent || self.profile.desktop.is_some());
        let can_apply = self.session_ready
            && !self.session_busy
            && (self.profile.concurrent || self.profile.desktop.is_some())
            && keyboard_apply_supported
            && validation_error.is_none();
        let cursor_conflict = self.cursor_settings.conflict(&self.profile.seats[index]);
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(footer_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
            |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(footer_rect));
                ui.vertical(|ui| {
                    if let Some(error) = &validation_error {
                        let display = bounded_display_text(
                            ui,
                            &format!("Cannot apply cursor settings: {error}"),
                            ui.style().text_styles[&egui::TextStyle::Small].clone(),
                            ui.available_width(),
                        );
                        ui.add(
                            egui::Label::new(
                                RichText::new(display)
                                    .text_style(egui::TextStyle::Small)
                                    .color(COLORS[1]),
                            )
                            .truncate(),
                        )
                        .on_hover_text(error);
                    } else if cursor_conflict {
                        Text::new("Applied settings changed; draft kept.")
                            .caption().show(ui)
                            .on_hover_text("Another controller changed the applied cursor settings. Use applied settings replaces this draft with the latest acknowledged settings.");
                    } else if !editable {
                        const ASSIGNMENT_WARNING: &str =
                            "Stop routing before editing device assignments.";
                        let display = bounded_display_text(
                            ui,
                            ASSIGNMENT_WARNING,
                            ui.style().text_styles[&egui::TextStyle::Small].clone(),
                            ui.available_width(),
                        );
                        Text::new(display)
                            .caption()
                            .show(ui)
                            .on_hover_text(ASSIGNMENT_WARNING);
                    } else {
                        ui.add_space(error_line_height);
                    }
                    Divider::horizontal().show(ui);
                    let apply_label = if self.user_tab == UserTab::Lock {
                                let mode = match self.profile.seats[index].cursor.bounds_mode {
                                    mux_core::desktop::BoundsMode::Legacy => "Legacy",
                                    mux_core::desktop::BoundsMode::Desktop => "Desktop",
                                    mux_core::desktop::BoundsMode::Window => "Window",
                                    mux_core::desktop::BoundsMode::Monitor => "Monitor",
                                    mux_core::desktop::BoundsMode::Region => "Region",
                                };
                                format!("Apply {mode} bounds")
                            } else {
                                "Apply cursor settings".to_owned()
                            };
                    let can_use_applied = !self.session_busy;
                    (apply_requested, use_applied_requested) = crate::cursor_settings_ui::footer_buttons(
                        ui, &apply_label, can_apply, cursor_conflict, separate_conflict_row, can_use_applied,
                        |ui, label, enabled| self.button(ui, label, enabled),
                    );
                });
            },
        );
        if use_applied_requested {
            self.cursor_settings
                .use_applied(&mut self.profile.seats[index]);
            self.region_draft_user = None;
        }
        if apply_requested
            && let Some(ticket) = self.cursor_settings.begin(index, &self.profile.seats)
        {
            // The routing worker rechecks current display bounds before changing input ownership.
            self.session.as_ref().unwrap().request(Request::Settings {
                index,
                settings: self.profile.seats[index].cursor.clone(),
                gui: Some(ticket),
            });
            self.session_busy = true;
        }
    }
    fn preview(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Interaction").strong());
        ui.colored_label(
            COLORS[0],
            if self.preview_paused {
                "Preview paused"
            } else {
                "Preview running"
            },
        );
        if Button::new(if self.preview_paused {
            "Resume preview"
        } else {
            "Pause preview"
        })
        .outline()
        .show(ui)
        .clicked()
        {
            self.preview_paused = !self.preview_paused;
        }
        ui.heading("Cursor preview");
        ui.label("Move each mouse to identify its independent pointer.");
        let (response, painter) = ui.allocate_painter(
            egui::vec2(ui.available_width(), 280.0),
            egui::Sense::hover(),
        );
        let rect = response.rect.shrink(core::SPACE_4);
        painter.rect_filled(response.rect, core::RADIUS_LG, Theme::get(ui).muted);
        for (i, d) in self
            .devices
            .iter()
            .enumerate()
            .filter(|(_, d)| d.kind == DeviceKind::Mouse)
        {
            let p = self.pointers.get(&d.handle).cloned().unwrap_or_default();
            let pos = egui::pos2(
                rect.left() + p.x * rect.width(),
                rect.top() + p.y * rect.height(),
            );
            let seat = self
                .profile
                .seats
                .iter()
                .enumerate()
                .find(|(_, s)| s.mouse.eq_ignore_ascii_case(&d.path));
            let color = COLORS[seat.map_or(i, |(index, _)| index) % COLORS.len()];
            let full_label =
                seat.map_or_else(|| format!("Mouse {}", i + 1), |(_, s)| s.name.clone());
            let label = bounded_display_text(
                ui,
                &full_label,
                typography::label().font_id(),
                (rect.right() - pos.x - 16.).max(0.),
            );
            painter.circle_filled(pos, if p.buttons != 0 { 10.0 } else { 6.0 }, color);
            painter.text(
                pos + egui::vec2(12.0, 12.0),
                egui::Align2::LEFT_TOP,
                label,
                typography::label().font_id(),
                color,
            );
        }
        if Button::new("Center preview pointers")
            .outline()
            .show(ui)
            .clicked()
        {
            self.pointers.clear();
        }
        ui.weak("Preview only: ordinary desktop input stays active.");
        if self.preview_paused {
            ui.colored_label(
                COLORS[4],
                "Motion preview is paused. Resume it from Interaction.",
            );
        }
    }
    fn profile_file_stamp(&self) -> crate::gui_profile_file::Stamp {
        crate::gui_profile_file::Stamp {
            session: self.agent_session_epoch,
            profile: self.profile_install_epoch,
            interaction: self.profile_interaction_epoch,
            active_path: self.profile_path.clone(),
            transfer_path: self.profile_transfer_path.clone(),
        }
    }

    fn has_local_profile_drafts(&self) -> bool {
        self.annotation_page.has_profile_draft()
            || self.magnifier_page.has_profile_draft()
            || self.output_page.has_profile_draft()
            || self.replication_page.has_profile_draft()
            || self.forwarding_page.has_profile_draft()
            || self.mapper_page.has_profile_draft()
            || self.cursor_settings.pending()
            || self.replication_dirty
            || self.selection_candidate.is_some()
            || self
                .profile
                .seats
                .iter()
                .enumerate()
                .any(|(user, _)| self.staged_window_locks.get(&self.profile, user).is_some())
    }

    fn setup_guard(&self, request: &crate::gui_setup::Request) -> Result<()> {
        ensure!(
            self.setup_handle.permits(request.client, request.epoch),
            "Stopped setup access was revoked"
        );
        ensure!(
            !request.decision.cancelled(),
            "Stopped setup request was abandoned before commit"
        );
        ensure!(
            self.session.is_none()
                && !self.restart_requested
                && !self.quit_after_stop
                && !self.preferences_quit_waiting
                && !self.allow_close,
            "GUI is starting or closing; setup is unavailable"
        );
        Ok(())
    }

    fn setup_recovery_ready(&self) -> bool {
        let recovery = self.recovery.snapshot();
        recovery.phase == crate::session_recovery::Phase::Stopped && recovery.can_start
    }

    fn setup_snapshot(&self) -> serde_json::Value {
        let discovery = self.discovery.status();
        let recovery = self.recovery.snapshot();
        let broker_phase = broker_phase_label(&self.agent_broker.status().phase);
        let fresh = self.discovery_current()
            && self.discovery_devices_ok
            && discovery.age_ms.is_some_and(|age| age <= 3_000);
        let devices = if fresh {
            crate::gui_setup::public_devices(&self.devices)
        } else {
            serde_json::json!([])
        };
        serde_json::json!({
            "revision": self.setup_revision,
            "active_path": self.profile_path,
            "transfer_path": self.profile_transfer_path,
            "dirty_local_drafts": self.has_local_profile_drafts(),
            "file": self.profile_file_snapshot(),
            "profile_persistence": self.app_preferences.profile_persistence(),
            "preferences_operation": self.preferences_worker.status(),
            "exit_save": self.profile_exit.status(),
            "discovery": discovery,
            "devices_current": fresh,
            "devices": devices,
            "configured_virtual_devices": crate::gui_setup::configured_virtual_devices(&self.profile),
            "virtual_path_formats": {"mouse": "virtual://mouse/{decimal_id}", "keyboard": "virtual://keyboard/{decimal_id}", "id_range": "1..=8388607"},
            "recovery": {"phase": recovery.phase, "can_start": recovery.can_start},
            "setup_access": {"enabled": self.setup_handle.enabled(), "broker_phase": broker_phase},
        })
    }

    fn validate_new_device_paths(&self, candidate: &Profile) -> Result<()> {
        let fresh = self.discovery_current()
            && self.discovery_devices_ok
            && self
                .discovery
                .status()
                .age_ms
                .is_some_and(|age| age <= 3_000);
        crate::gui_setup::validate_new_device_paths(&self.profile, candidate, fresh, &self.devices)
    }

    fn submit_setup_file(
        &mut self,
        kind: crate::gui_profile_file::Kind,
        request: &crate::gui_setup::Request,
    ) -> Result<u64> {
        use crate::gui_profile_file::Kind;
        let selected_path = if matches!(kind, Kind::Import | Kind::Export) {
            PathBuf::from(&self.profile_transfer_path)
        } else {
            PathBuf::from(&self.profile_path)
        };
        ensure!(
            !selected_path.as_os_str().is_empty(),
            "Choose a profile path in the GUI first"
        );
        let profile = Arc::new(self.profile.clone());
        let stamp = self.profile_file_stamp();
        ensure!(
            self.setup_handle.permits(request.client, request.epoch),
            "Stopped setup access was revoked before file submission"
        );
        let ticket = self
            .profile_file
            .submit(kind, selected_path, profile, stamp)?;
        self.profile_file_ui.result = None;
        self.status = "Connected profile file operation in progress.".into();
        Ok(ticket)
    }

    fn submit_setup_persistence(
        &mut self,
        policy: crate::app_preferences::ProfilePersistence,
        request: &crate::gui_setup::Request,
    ) -> Result<(u64, crate::app_preferences_ui::PreferencesEditorSubmission)> {
        self.setup_guard(request)?;
        ensure!(
            !self.profile_file.pending(),
            "Finish the profile file operation before changing persistence preferences"
        );
        let editor = self
            .app_preferences_page
            .capture_for_submit(&self.app_preferences);
        let candidate = self.app_preferences.with_profile_persistence(policy);
        let ticket = self.preferences_worker.submit(
            crate::gui_app_preferences::Kind::Configure,
            Some(candidate),
            crate::gui_app_preferences::Origin::Setup {
                client: request.client,
                sequence: request.sequence,
                epoch: request.epoch,
            },
            request.decision.clone(),
        )?;
        self.app_preferences_page.set_pending(true);
        self.status = "Applying stopped setup profile preferences.".into();
        Ok((ticket, editor))
    }

    fn drain_setup_requests(&mut self) {
        use crate::gui_setup::PreparedCommand as Command;
        enum Admitted {
            Immediate,
            File(u64),
            Persistence(u64, crate::app_preferences_ui::PreferencesEditorSubmission),
        }
        let requests: Vec<_> = self.setup_requests.try_iter().take(8).collect();
        for request in requests {
            let result = (|| -> Result<Admitted> {
                self.setup_guard(&request)?;
                match &request.command {
                    Command::Inspect => Ok(Admitted::Immediate),
                    Command::RefreshDiscovery => {
                        self.refresh();
                        Ok(Admitted::Immediate)
                    }
                    Command::ConfigurePersistence {
                        expected_revision,
                        policy,
                    } => {
                        ensure!(
                            self.setup_recovery_ready(),
                            "Recovery is not ready; stopped setup is inspection-only"
                        );
                        ensure!(
                            *expected_revision == self.setup_revision,
                            "Stopped setup revision changed; inspect again"
                        );
                        let (ticket, editor) = self.submit_setup_persistence(*policy, &request)?;
                        Ok(Admitted::Persistence(ticket, editor))
                    }
                    Command::Configure {
                        expected_revision,
                        profile,
                    } => {
                        ensure!(
                            self.setup_recovery_ready(),
                            "Recovery is not ready; stopped setup is inspection-only"
                        );
                        crate::gui_setup::check_mutation(
                            *expected_revision,
                            self.setup_revision,
                            self.has_local_profile_drafts(),
                            self.profile_file.pending() || self.preferences_worker.pending(),
                        )?;
                        ensure!(
                            !self.profile_file_ui.initial_unresolved,
                            "Resolve the initial profile before Configure"
                        );
                        self.validate_new_device_paths(profile)?;
                        let profile = profile.as_ref().clone();
                        ensure!(
                            self.setup_handle.permits(request.client, request.epoch),
                            "Stopped setup access was revoked before Configure commit"
                        );
                        self.install_profile(profile);
                        self.selected = (!self.profile.seats.is_empty()).then_some(0);
                        self.status = "Connected stopped profile installed in the GUI; Save profile to persist it.".into();
                        Ok(Admitted::Immediate)
                    }
                    Command::Save { expected_revision }
                    | Command::Load { expected_revision }
                    | Command::Import { expected_revision }
                    | Command::Export { expected_revision } => {
                        ensure!(
                            self.setup_recovery_ready(),
                            "Recovery is not ready; stopped setup is inspection-only"
                        );
                        crate::gui_setup::check_mutation(
                            *expected_revision,
                            self.setup_revision,
                            self.has_local_profile_drafts(),
                            self.profile_file.pending() || self.preferences_worker.pending(),
                        )?;
                        let kind = match &request.command {
                            Command::Save { .. } => crate::gui_profile_file::Kind::Save,
                            Command::Load { .. } => crate::gui_profile_file::Kind::Load,
                            Command::Import { .. } => crate::gui_profile_file::Kind::Import,
                            Command::Export { .. } => crate::gui_profile_file::Kind::Export,
                            _ => unreachable!(),
                        };
                        ensure!(
                            !self.profile_file_ui.initial_unresolved
                                || matches!(kind, crate::gui_profile_file::Kind::Load),
                            "Resolve the initial profile before this file operation"
                        );
                        Ok(Admitted::File(self.submit_setup_file(kind, &request)?))
                    }
                }
            })();
            match result {
                Ok(Admitted::File(ticket)) => self.setup_file_pending = Some((ticket, request)),
                Ok(Admitted::Persistence(ticket, editor)) => {
                    self.preferences_pending = Some(PreferencesPending {
                        ticket,
                        editor: Some(editor),
                        request: None,
                        setup_request: Some(request),
                    });
                }
                Ok(Admitted::Immediate) => {
                    let profile =
                        matches!(&request.command, Command::Inspect).then(|| self.profile.clone());
                    request.reply(self.setup_snapshot(), None, profile);
                }
                Err(error) => {
                    request.reply(serde_json::Value::Null, Some(format!("{error:#}")), None)
                }
            }
        }
    }
    fn profile_file_snapshot(&self) -> crate::gui_profile_file::Snapshot {
        crate::gui_profile_file::Snapshot {
            operation: self.profile_file.status(),
            ui: self.profile_file_ui.clone(),
        }
    }
    fn submit_profile_file(&mut self, kind: crate::gui_profile_file::Kind) {
        use crate::gui_profile_file::Kind;
        let path = if matches!(kind, Kind::Import | Kind::Export) {
            &self.profile_transfer_path
        } else {
            &self.profile_path
        };
        self.submit_profile_file_at(kind, PathBuf::from(path));
    }
    fn submit_profile_file_at(&mut self, kind: crate::gui_profile_file::Kind, path: PathBuf) {
        let result = self.profile_file.submit(
            kind,
            path,
            std::sync::Arc::new(self.profile.clone()),
            self.profile_file_stamp(),
        );
        self.profile_file_ui.result = None;
        self.status = match result {
            Ok(_) => "Profile file operation in progress.".into(),
            Err(error) => {
                self.profile_file_ui.result = Some(crate::gui_profile_file::Outcome::Failed);
                format!("{error:#}")
            }
        };
        self.profile_feedback = Some((self.status.clone(), self.profile_file_ui.result.is_some()));
    }
    fn poll_profile_file(&mut self, ctx: &egui::Context) {
        use crate::gui_profile_file::{Kind, Outcome, Value};
        let Some(completion) = self.profile_file.poll() else {
            return;
        };
        if self
            .profile_exit
            .observe(&completion, Path::new(&self.profile_path), &self.profile)
        {
            self.profile_file_ui.result = Some(if self.profile_exit.status().error.is_some() {
                Outcome::Failed
            } else {
                Outcome::Saved
            });
            if let Some(error) = self.profile_exit.status().error {
                self.fail_profile_exit(ctx, error);
            } else {
                self.status = format!("Saved {} before exit.", completion.path.display());
                self.profile_feedback = Some((self.status.clone(), false));
            }
            return;
        }
        let settling_quit_read = matches!(
            self.profile_exit.status().phase,
            crate::gui_profile_policy::Phase::AwaitingQuiescence
        ) && self.session.is_none()
            && !self.allow_close
            && !self.has_local_profile_drafts()
            && completion.matches_quit_read(&self.profile_file_stamp(), &self.profile);
        let current = settling_quit_read
            || completion.is_current(
                &self.profile_file_stamp(),
                &self.profile,
                self.session.is_none()
                    && !self.quit_after_stop
                    && !self.allow_close
                    && !self.profile_file_ui.quit_waiting,
            );
        let path = completion.path.display().to_string();
        let outcome = match completion.result {
            Err(error) => {
                self.status = format!("Profile file operation failed for {path}: {error}");
                Outcome::Failed
            }
            Ok(Value::Saved { profile, existed }) => {
                let action = if completion.kind == Kind::Export {
                    "Exported"
                } else {
                    "Saved"
                };
                self.status = format!(
                    "{action} {path}. {}",
                    if existed {
                        "Previous valid bytes were backed up."
                    } else {
                        "No previous file existed to back up."
                    }
                );
                if completion.kind == Kind::Export {
                    Outcome::Exported
                } else if current {
                    self.profile = profile;
                    self.advance_setup_revision();
                    Outcome::Saved
                } else {
                    self.status
                        .push_str(" Current setup or view changed; current memory was kept.");
                    Outcome::FinishedNotInstalled
                }
            }
            Ok(Value::MissingDefault) => {
                if current {
                    self.profile_file_ui.installed(completion.kind);
                    self.status =
                        "No saved startup profile was found. The current fresh setup is ready."
                            .into();
                    Outcome::Installed
                } else {
                    self.status = "No default profile was found, but setup changed; resolve with Load or Reset.".into();
                    Outcome::FinishedNotInstalled
                }
            }
            Ok(Value::Loaded(profile)) => {
                if current {
                    if matches!(
                        completion.kind,
                        Kind::InitialLoad | Kind::InitialLoadDefault
                    ) {
                        self.profile = profile;
                        self.reset_profile_editors();
                        self.selected = (!self.profile.seats.is_empty()).then_some(0);
                    } else {
                        self.install_profile(profile);
                    }
                    self.profile_file_ui.installed(completion.kind);
                    if matches!(
                        completion.kind,
                        Kind::InitialLoad | Kind::InitialLoadDefault
                    ) && self.initial_profile_size.take().is_some()
                    {
                        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(home_window_size(
                            self.profile.seats.len(),
                        )));
                    }
                    self.status = if completion.kind == Kind::Restore {
                        format!(
                            "Restored {path} on disk and installed it. The previous main file is now backed up."
                        )
                    } else {
                        format!("Loaded {path}. Verify target PIDs before attaching.")
                    };
                    Outcome::Installed
                } else {
                    self.status = if completion.kind == Kind::Restore {
                        format!(
                            "Restored {path} on disk, but current setup or view changed; not installed."
                        )
                    } else {
                        format!(
                            "Finished reading {path}, but current setup or view changed; not installed."
                        )
                    };
                    Outcome::FinishedNotInstalled
                }
            }
        };
        if matches!(
            completion.kind,
            Kind::InitialLoad | Kind::InitialLoadDefault
        ) {
            self.initial_profile_size = None;
        }
        self.profile_feedback = Some((self.status.clone(), outcome == Outcome::Failed));
        if self.profile_file_ui.quit_waiting
            && matches!(outcome, Outcome::Failed | Outcome::FinishedNotInstalled)
        {
            self.profile_exit.cancel();
            let error = if outcome == Outcome::FinishedNotInstalled {
                format!(
                    "{} Review the current setup before quitting again.",
                    self.status
                )
            } else {
                self.status.clone()
            };
            self.fail_profile_exit(ctx, error);
        }
        if self.profile_file_ui.finished(outcome) {
            self.request_routing_stop(true);
        }
        if let Some((ticket, request)) = self.setup_file_pending.take() {
            if ticket == completion.ticket {
                let value = serde_json::json!({
                    "ticket": ticket,
                    "kind": completion.kind,
                    "path": path,
                    "outcome": outcome,
                    "revision": self.setup_revision,
                    "message": self.status,
                });
                let error = (outcome == Outcome::Failed).then(|| self.status.clone());
                request.reply(value, error, None);
            } else {
                self.setup_file_pending = Some((ticket, request));
            }
        }
    }
    fn profiles(&mut self, ui: &mut egui::Ui) {
        let previous_status = self.status.clone();
        let mut feedback_failed = false;
        Heading::new("Profiles & backend").heading().show(ui);
        if self.profile_file_ui.initial_unresolved {
            Text::new(if self.profile_file.pending() {
                "Loading initial profile."
            } else {
                "Initial profile unavailable. Load a profile or reset before starting."
            })
            .wrap()
            .color(Theme::get(ui).error)
            .show(ui);
        }
        if self.profile_file.pending() {
            Text::new(if self.profile_file_ui.quit_waiting {
                "Finishing profile operation before quitting."
            } else {
                "Profile file operation in progress."
            })
            .wrap()
            .show(ui);
        }
        Text::new("Save keeps the current setup on disk. Load replaces the current setup and its unsaved editors.")
            .caption().wrap().show(ui);
        if self.session.is_some() {
            Text::new("Stop routing before changing profiles or target scope.")
                .caption()
                .wrap()
                .show(ui);
        }
        egui::ScrollArea::vertical().id_salt("profile-workflow").show(ui, |ui| {
        ui.label("Active profile file");
        Input::new(&mut self.profile_path)
            .id_source("profile_path")
            .show(ui);
        ui.horizontal_wrapped(|ui| {
            if self.prominent_button(ui, "Save profile", self.session.is_none() && !self.preferences_worker.retiring() && !self.preferences_quit_waiting && !self.profile_file.pending() && !self.profile_file_ui.quit_waiting && !self.profile_file_ui.initial_unresolved, Some(ButtonVariant::Default)) {
                self.submit_profile_file(crate::gui_profile_file::Kind::Save);
            }
            if self.home_button(ui, "Load profile", self.session.is_none() && !self.preferences_worker.retiring() && !self.preferences_quit_waiting && !self.profile_file.pending() && !self.profile_file_ui.quit_waiting, ButtonVariant::Secondary) {
                self.submit_profile_file(crate::gui_profile_file::Kind::Load);
            }
        });
        if let Some((message, failed)) = &self.profile_feedback {
            Text::new(message).wrap().color(if *failed { Theme::get(ui).error } else { Theme::get(ui).foreground }).show(ui);
        }
        Text::new("Apply in a feature page updates the current setup. Save profile makes those accepted changes durable; it does not save unsubmitted drafts.")
            .caption().wrap().show(ui);
        Divider::horizontal().show(ui);
        let transfer = Collapsible::new("Import / export").id_source("profiles-transfer").default_open(false).show(ui, |ui| {
        ui.label("Transfer file");
        Input::new(&mut self.profile_transfer_path)
            .id_source("profile_transfer_path")
            .show(ui);
        ui.horizontal_wrapped(|ui| {
            if self.home_button(ui, "Import profile", self.session.is_none() && !self.preferences_worker.retiring() && !self.preferences_quit_waiting && !self.profile_file.pending() && !self.profile_file_ui.quit_waiting && !self.profile_file_ui.initial_unresolved, ButtonVariant::Secondary) {
                self.submit_profile_file(crate::gui_profile_file::Kind::Import);
            }
            if self.home_button(ui, "Export profile", self.session.is_none() && !self.preferences_worker.retiring() && !self.preferences_quit_waiting && !self.profile_file.pending() && !self.profile_file_ui.quit_waiting && !self.profile_file_ui.initial_unresolved, ButtonVariant::Secondary) {
                self.submit_profile_file(crate::gui_profile_file::Kind::Export);
            }
        });
        Text::new("Import replaces the current setup. Export writes a separate validated snapshot.").caption().wrap().show(ui);
        });
        if let Some(script) = self.script.as_mut() { script.observe("Import / export", &transfer); }
        let recovery = Collapsible::new("Profile recovery").id_source("profiles-recovery").default_open(false).show(ui, |ui| {
        Text::new("Restore replaces the active file from its backup. Reset and Undo change memory only; Undo cannot reverse disk writes.").caption().wrap().show(ui);
        ui.horizontal_wrapped(|ui| {
            if self.home_button(ui, "Restore profile backup", self.session.is_none() && !self.preferences_worker.retiring() && !self.preferences_quit_waiting && !self.profile_file.pending() && !self.profile_file_ui.quit_waiting && !self.profile_file_ui.initial_unresolved, ButtonVariant::Secondary) {
                self.submit_profile_file(crate::gui_profile_file::Kind::Restore);
            }
            if self.home_button(ui, "Reset profile", self.session.is_none(), ButtonVariant::Secondary) {
                let mut empty = Profile::default();
                match empty.normalize_ids() {
                    Ok(()) => {
                        self.install_profile(empty);
                        self.profile_file_ui.reset();
                        self.initial_profile_size = None;
                        self.status = if self.profile_file.pending() { "Reset to an empty profile. The pending file operation will still finish." } else { "Reset to an empty profile. No file was changed." }.into();
                    }
                    Err(error) => { feedback_failed = true; self.status = format!("{error:#}"); },
                }
            }
            if self.button(
                ui,
                "Undo profile change",
                self.session.is_none() && self.profile_undo.is_some(),
            ) && let Some(previous) = self.profile_undo.take() {
                self.profile = previous;
                self.reset_profile_editors();
                self.status = "Restored the previous in-memory profile. Disk writes were not undone.".into();
            }
        });
        ui.weak(format!(
            "Automatic backup path: {}. Undo restores one prior in-memory profile only; it does not reverse file writes.",
            crate::profile_store::backup_path(Path::new(&self.profile_path)).display()
        ));
        });
        if let Some(script) = self.script.as_mut() { script.observe("Profile recovery", &recovery); }
        let scope = Collapsible::new("Automatic targeting scope").id_source("profiles-scope").default_open(false).show(ui, |ui| {
        ui.add_enabled_ui(self.session.is_none(), |ui| {
        if let Some(options) = &mut self.profile.desktop {
            ui.weak(
                "Leave every application unchecked to target any eligible desktop application.",
            );
            for target in &self.targets {
                let mut allowed = options.allowed_pids.contains(&target.pid);
                if Checkbox::new(&mut allowed)
                    .label(format!("{} · PID {}", target.title, target.pid))
                    .show(ui)
                    .changed()
                {
                    if allowed {
                        options.allowed_pids.push(target.pid);
                    } else {
                        options.allowed_pids.retain(|p| *p != target.pid);
                    }
                    options.allowed_pids.sort_unstable();
                    options.allowed_pids.dedup();
                }
            }
        } else {
            Text::new("This profile uses assigned applications. Automatic targeting is configured from Home.").caption().wrap().show(ui);
        }
        });
        });
        if let Some(script) = self.script.as_mut() { script.observe("Automatic targeting scope", &scope); }
        let backend = Collapsible::new("Backend runtime").id_source("profiles-backend").default_open(false).show(ui, |ui| {
        ui.label("Runtime folder");
        Input::new(&mut self.runtime)
            .id_source("runtime_folder")
            .show(ui);
        Text::new("Used for the next routing session. This folder must contain the built ProtoInput DLLs. Device discovery and preview work without them.").caption().wrap().show(ui);
        });
        if let Some(script) = self.script.as_mut() { script.observe("Backend runtime", &backend); }
        });
        if self.status != previous_status {
            self.profile_feedback = Some((self.status.clone(), feedback_failed));
        }
    }
}
impl eframe::App for Dashboard {
    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        if let Some(script) = &mut self.script {
            script.capture(ctx, input);
            script.input(input);
        }
        if crate::gui_profile_file::local_interaction(
            input,
            ctx.input(|state| state.pointer.any_down()),
        ) {
            self.profile_interaction_epoch = self.profile_interaction_epoch.saturating_add(1);
            self.advance_setup_revision();
        }
        if let Some(expected) = self.initial_profile_size
            && input
                .viewport()
                .inner_rect
                .is_some_and(|rect| (rect.size() - expected).length() > 2.0)
        {
            self.initial_profile_size = None;
        }
    }
    fn logic(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        // eframe skips `ui` while hidden. Keep tray actions, connected requests,
        // target cleanup, and device-disconnect handling alive independently of painting.
        self.staged_window_locks.prune(&self.profile);
        self.poll_session(ctx);
        self.poll_agent_broker();
        self.poll_sdk(ctx);
        self.poll_application_events(ctx);
        if matches!(
            self.agent_broker.status().phase,
            crate::gui_agent_broker::Phase::Starting | crate::gui_agent_broker::Phase::Stopping
        ) {
            ctx.request_repaint_after(Duration::from_millis(40));
        }
        if self.quit_after_stop
            && crate::gui_agent_broker::quit_ready(
                self.session.is_none(),
                &self.agent_broker.status().phase,
            )
        {
            self.close_ready = self.all_sdks_retired();
        }
        self.poll_preferences();
        self.poll_profile_file(ctx);
        self.poll_macro_file();
        self.poll_failed_cleanup();
        if !self.setup_handle.enabled()
            && self.app_request_handle.retirement_due()
            && self
                .agent_broker
                .request_automatic_stop(self.agent_session_epoch)
        {
            self.agent_pending_enable = None;
            self.agent_page.reset_session();
        }
        if self.restart_requested
            && (!self.discovery_current()
                || self.restart_discovery_generation != self.discovery_applied_generation
                || self.recovery_phase != crate::session_recovery::Phase::Stopped)
        {
            self.restart_requested = false;
            self.status = "Pending Start cancelled because readiness changed. Check targets, then start again.".into();
        }
        if crate::gui_agent_broker::restart_ready(
            self.restart_requested,
            self.session.is_none(),
            self.recovery_phase == crate::session_recovery::Phase::Stopped,
            &self.agent_broker.status().phase,
        ) && !self.quit_after_stop
            && self.all_sdks_retired()
        {
            self.restart_requested = false;
            self.start_routing();
        }
        self.poll_discovery();
        if !self.shortcut_status().active && self.session.is_some() && !self.session_stopping {
            self.request_routing_stop(false);
        }
        if let Some(script) = &self.script {
            script.macro_file_state(&serde_json::json!({
                "operation": self.macro_file.status(),
                "session_epoch": self.macro_file_session_epoch,
                "mutation_epoch": self.macro_file_mutation_epoch,
                "observed_macro_revision": self.macro_file_observed_revision,
                "result": self.macro_file_result,
                "error": self.macro_error,
                "macro_loaded_events": self.macro_loaded_events,
            }));
            script.recovery_state(&self.recovery.snapshot());
            let stages: Vec<_> = self.profile.seats.iter().enumerate().filter_map(|(user, _)| {
                self.staged_window_locks.get(&self.profile, user).map(|stage| serde_json::json!({"user": user, "selection": stage.selection, "title": stage.title}))
            }).collect();
            script.window_lock_state(&serde_json::json!({
                "staged": stages,
                "startup_selecting": self.startup_window_selection_active,
                "startup_queued": self.startup_window_selections.len(),
                "pending": if self.startup_window_selection_active { self.selection_pending.as_ref() } else { None },
                "startup_error": self.startup_window_selection_error,
                "ready": self.session_ready,
                "stopping": self.session_stopping,
            }));
            script.shortcut_status(&self.shortcut_status());
        }
        self.poll_lifecycle_actions(ctx);
        self.apply_script_fields();
        self.screen_page.poll(ctx);
        self.sync_screen_target();
        self.drain_app_requests();
        self.drain_setup_requests();
        let close_requested = ctx.input(|input| input.viewport().close_requested());
        if close_requested && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.recovery.snapshot().phase == crate::session_recovery::Phase::Quarantined {
                self.handle_lifecycle_action(crate::app_lifecycle::LifecycleAction::Restore, ctx);
                self.status =
                    "Cleanup failed. Restart the affected applications to recover.".into();
                self.recovery_dialog_open = true;
            } else if self.app_preferences.close_to_tray
                && !self.quit_after_stop
                && !self.preferences_quit_waiting
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            } else if !self.preferences_quit_waiting {
                self.handle_lifecycle_action(crate::app_lifecycle::LifecycleAction::Quit, ctx);
            }
        }
        self.poll_preferences_quit(ctx);
        if let Some(script) = &self.script {
            script.launch_state(
                self.session.is_some(),
                self.session
                    .as_ref()
                    .is_some_and(Controller::preflight_pending),
                self.session_ready,
                self.session_stopping,
                &serde_json::json!({
                    "broker_phase": broker_phase_label(&self.agent_broker.status().phase),
                    "broker_generation": self.agent_broker.status().generation,
                    "sdk": self.sdk_service.as_ref().map(|service| service.snapshot()).unwrap_or_default(),
                    "application_events": self.application_events.status(),
                    "enabled": self.setup_handle.enabled(),
                    "retiring": self.setup_retiring,
                    "start_order": self.setup_start_order,
                    "quit_trace": {
                        "received": self.lifecycle_trace.as_ref(),
                        "quit_after_stop": self.quit_after_stop,
                        "preferences_quit_waiting": self.preferences_quit_waiting,
                        "close_ready": self.close_ready,
                        "allow_close": self.allow_close,
                        "preferences_operation": self.preferences_worker.status(),
                    },
                }),
            );
        }
        self.publish_preferences_snapshot();
        let minimized = ctx.input(|input| input.viewport().minimized);
        if let Some(started) = self.restore_started_frame {
            // egui may repeat layout with stale viewport input. Keep Restore
            // authoritative until a later native frame reports it unminimized.
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            if ctx.cumulative_frame_nr() > started && minimized == Some(false) {
                self.restore_started_frame = None;
            }
        } else if minimized == Some(true) && self.app_preferences.minimize_to_tray {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        let mut changed = false;
        for event in self.capture.events.try_iter().take(4096) {
            match event {
                Event::Mouse {
                    handle,
                    x,
                    y,
                    absolute,
                    buttons,
                } => {
                    // Process button releases even while preview motion is paused.
                    let p = self.pointers.entry(handle).or_default();
                    if self.preview_paused {
                        p.update(0, 0, false, buttons);
                    } else {
                        p.update(x, y, absolute, buttons);
                    }
                }
                Event::DevicesChanged => changed = true,
                Event::Keyboard { handle } => {
                    *self.keyboard_packets.entry(handle).or_default() += 1;
                }
                Event::Error(e) => self.status = e,
            }
        }
        let discovery_busy = self.discovery.status();
        if changed
            || (!discovery_busy.in_flight
                && !discovery_busy.queued
                && !discovery_busy.worker_failed
                && self.last_refresh.elapsed() > Duration::from_secs(3))
        {
            self.refresh();
        }
        if let Some(fault) = self
            .script
            .as_ref()
            .and_then(crate::ui_script::Script::io_fault)
        {
            self.status = format!("UI script I/O stopped: {fault}");
        }
        ctx.request_repaint_after(Duration::from_millis(16));
    }
    fn ui(&mut self, root: &mut egui::Ui, _: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        if self.quit_after_stop || self.preferences_quit_waiting || self.allow_close {
            root.disable();
        }
        if self.page == Page::Apps && !self.app_preferences.show_app_launcher {
            self.go_to_page(&ctx, Page::Home);
        }
        self.sync_page_viewport(&ctx);
        let zoom_changed = self.sync_appearance(&ctx);
        if self.minimum_size_pending && !zoom_changed {
            // Zoom activates at the next egui pass. Reissue the logical minimum
            // once that scale is active, without resizing the current window.
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(egui::vec2(
                320.0, 360.0,
            )));
            self.minimum_size_pending = false;
        }
        if let Some(script) = &mut self.script {
            script.ready(
                !(self.profile_file_ui.initial_unresolved && self.profile_file.pending())
                    && (self.session.is_none() || self.session_ready && !self.session_busy),
            );
        }
        if let Some(script) = &self.script {
            script.workflow_state(
                &self.output_page.snapshot(),
                self.selected,
                &format!("{:?}", self.page),
                self.settings_category.label(),
            );
            let footer_status = self.attachment.message().map_or_else(
                || self.status.clone(),
                |notice| format!("{notice}\n{}", self.status),
            );
            script.profile_state(
                &self.profile,
                self.profile_undo.is_some(),
                &self.profile_path,
                &self.profile_transfer_path,
                &footer_status,
                &crate::ui_script::ProfileUiStatus {
                    cursor_settings_sync: self.cursor_settings.snapshot(
                        self.selected
                            .and_then(|index| self.profile.seats.get(index)),
                    ),
                    profile_file: self.profile_file_snapshot(),
                    exit_save: self.profile_exit.status(),
                },
            );
        }
        if let Some((user, point)) = self.script.as_ref().and_then(|s| s.position_edit()) {
            while self.lab_positions.len() <= user {
                self.lab_positions.push([0, 0]);
            }
            self.lab_positions[user] = point;
        }
        egui::Panel::top("header").show_inside(root, |ui| {
            let status_text = if self.session.is_none() {
                "Stopped"
            } else if self.session_stopping {
                "Stopping"
            } else if !self.session_ready {
                "Starting"
            } else if self.session_paused {
                "Paused"
            } else {
                "Routing"
            };
            ui.horizontal(|ui| {
                if self.page == Page::User {
                    if let Some(index) = self.selected.filter(|i| *i < self.profile.seats.len()) {
                        let full_name = &self.profile.seats[index].name;
                        let [r, g, b] = self.profile.seats[index].cursor.color;
                        let font = ui.style().text_styles[&egui::TextStyle::Body].clone();
                        let status_width = ui.fonts_mut(|fonts| {
                            fonts
                                .layout_no_wrap(
                                    status_text.to_owned(),
                                    font.clone(),
                                    Color32::WHITE,
                                )
                                .size()
                                .x
                        });
                        let identity_width =
                            (ui.available_width() - status_width - ui.spacing().item_spacing.x)
                                .max(0.0);
                        let display_name =
                            bounded_display_text(ui, full_name, font, identity_width);
                        let response = ui.label(
                            RichText::new(display_name)
                                .strong()
                                .color(Color32::from_rgb(r, g, b)),
                        );
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Label,
                                true,
                                full_name.to_owned(),
                            )
                        });
                        response.on_hover_text(full_name);
                    } else {
                        ui.strong(crate::APP_NAME);
                    }
                } else {
                    ui.strong(crate::APP_NAME);
                }
                if self.page != Page::Home {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.weak(status_text);
                    });
                }
            });
        });
        let home_default_status = matches!(self.page, Page::Home | Page::User)
            && self.status == "Ready to assign devices.";
        let attachment_message = self.attachment.message();
        if !home_default_status
            || attachment_message.is_some()
            || self.capture.dropped_packets() > 0
        {
            egui::Panel::bottom("status").show_inside(root, |ui| {
                if let Some(message) = &attachment_message {
                    let display = bounded_display_text(
                        ui,
                        message,
                        ui.style().text_styles[&egui::TextStyle::Small].clone(),
                        ui.available_width(),
                    );
                    let text = Text::new(display).caption();
                    let text = if self.attachment.failure.is_some() {
                        text.color(ui.visuals().error_fg_color)
                    } else {
                        text
                    };
                    text.show(ui).on_hover_text(message);
                }
                if !home_default_status {
                    let display_status = bounded_display_text(
                        ui,
                        &self.status,
                        ui.style().text_styles[&egui::TextStyle::Small].clone(),
                        ui.available_width(),
                    );
                    Text::new(display_status)
                        .caption()
                        .show(ui)
                        .on_hover_text(&self.status);
                }
                if self.capture.dropped_packets() > 0 {
                    ui.colored_label(
                        COLORS[1],
                        format!("{} preview packets dropped", self.capture.dropped_packets()),
                    );
                }
            });
        }
        egui::CentralPanel::default().show_inside(root, |ui| {
            if self.page == Page::Home {
                self.home(ui);
                return;
            }
            if self.page == Page::User {
                if self.home_button(ui, "Back to Home", true, ButtonVariant::Ghost) {
                    self.go_to_page(ui.ctx(), Page::Home);
                }
            } else if !matches!(self.page, Page::Home) {
                ui.horizontal_wrapped(|ui| {
                    if self.home_button(ui, "Back to Home", true, ButtonVariant::Ghost) {
                        self.go_to_page(ui.ctx(), Page::Home);
                    }
                    if let Some(category) = SettingsCategory::for_page(self.page) {
                        ui.weak(format!("Settings / {}", category.label()));
                    }
                });
                Divider::horizontal().show(ui);
            }
            if self.page == Page::Apps {
                self.apps_launcher(ui);
                return;
            }
            if self.page == Page::AppPreferences {
                self.app_preferences_controls(ui);
                return;
            }
            if self.page == Page::User {
                self.user_settings(ui);
                return;
            }
            if self.page == Page::Output {
                self.output_controls(ui);
                return;
            }
            if self.page == Page::Profiles {
                self.profiles(ui);
                return;
            }
            if self.page == Page::Agent {
                // Agent Access owns its bounded vertical scrolling, including
                // revealing cleanup details. Avoid competing nested scroll areas.
                self.agent_controls(ui);
                return;
            }
            if self.page == Page::InputMapper {
                // The mapper reserves actions above its own independently scrolling body.
                self.mapper_controls(ui);
                return;
            }
            page_scroll_area(self.page).show(ui, |ui| match self.page {
                Page::Home => self.home(ui),
                Page::Apps => self.apps_launcher(ui),
                Page::Settings => self.settings_landing(ui),
                Page::AppPreferences => self.app_preferences_controls(ui),
                Page::Users => self.roster(ui),
                Page::User => {
                    let editable = self.session.is_none()
                        || matches!(
                            self.user_tab,
                            UserTab::Keyboard
                                | UserTab::Motion
                                | UserTab::Buttons
                                | UserTab::Wheel
                                | UserTab::Theme
                                | UserTab::Overlay
                                | UserTab::Lock
                        );
                    // Tabs remain navigable while routing; assignments stay fixed until stopped.
                    if self.session.is_some() && !editable {
                        ui.weak("Stop routing before editing device assignments.");
                    }
                    self.user_settings(ui);
                }
                Page::Lab => {
                    self.emulated_controls(ui);
                    self.keyboard_text_controls(ui);
                }
                Page::Automation => self.automation_controls(ui),
                Page::Annotation => self.annotation_controls(ui, None),
                Page::Magnifier => self.magnifier_controls(ui, None),
                Page::MacroStudio => self.macro_studio_controls(ui, None),
                Page::Replication => self.replication_controls(ui),
                Page::Forwarding => self.forwarding_controls(ui),
                Page::InputMapper => self.mapper_controls(ui),
                Page::Screen => self.screen_controls(ui),
                Page::VirtualUsers => {
                    let mapper = self.mapper_page.snapshot();
                    let allowed_pids = self
                        .profile
                        .desktop
                        .as_ref()
                        .map(|desktop| desktop.allowed_pids.clone())
                        .unwrap_or_default();
                    if let Some(crate::virtual_users_ui::PageAction::Worker(command)) =
                        self.virtual_users_page.show(
                            ui,
                            &self.profile.seats,
                            &allowed_pids,
                            &self.targets,
                            self.profile.desktop.is_some(),
                            self.session.is_some(),
                            self.session_ready,
                            self.session_busy,
                            mapper.armed || mapper.cleanup_pending,
                            self.script.as_mut(),
                            &mut self.action_frame,
                            &mut self.frame_actions,
                        )
                        && let Some(session) = &self.session
                    {
                        session.request(Request::VirtualUsers(command));
                        self.session_busy = true;
                    }
                }
                Page::Agent => self.agent_controls(ui),
                Page::Output => self.output_controls(ui),
                Page::Preview => self.preview(ui),
                Page::Profiles => {
                    ui.add_enabled_ui(self.session.is_none(), |ui| self.profiles(ui));
                }
            });
        });
        if self.show_devices {
            self.inventory(&ctx);
        }
        self.recovery_dialog(&ctx);
        self.input_release_dialog(&ctx);
    }
}

#[cfg(test)]
mod home_navigation_tests {
    use super::*;

    #[test]
    fn access_keys_require_a_focused_exact_alt_press() {
        fn event(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers,
            }
        }
        fn frame(ctx: &egui::Context, focused: bool, event: egui::Event) -> Option<HomeAccess> {
            let mut result = None;
            let _ = ctx.run_ui(
                egui::RawInput {
                    focused,
                    events: vec![event],
                    ..Default::default()
                },
                |ui| result = home_access_key(ui.ctx()),
            );
            result
        }

        for (key, expected) in [
            (egui::Key::C, HomeAccess::Configure),
            (egui::Key::S, HomeAccess::Settings),
            (egui::Key::H, HomeAccess::Hide),
            (egui::Key::A, HomeAccess::Apps),
        ] {
            let ctx = egui::Context::default();
            assert_eq!(
                frame(&ctx, true, event(key, egui::Modifiers::ALT)),
                Some(expected)
            );
            assert_eq!(frame(&ctx, true, event(key, egui::Modifiers::ALT)), None);
            let ctx = egui::Context::default();
            assert_eq!(frame(&ctx, false, event(key, egui::Modifiers::ALT)), None);
            let ctx = egui::Context::default();
            assert_eq!(
                frame(
                    &ctx,
                    true,
                    event(key, egui::Modifiers::ALT | egui::Modifiers::CTRL)
                ),
                None
            );
        }
    }

    #[test]
    fn roster_number_recognizes_single_then_double_click_at_same_identity() {
        let ctx = egui::Context::default();
        Theme::install(&ctx, Mode::Dark);
        let mut selected = false;
        let mut rect = egui::Rect::NOTHING;
        let mut clicks = Vec::new();
        for (time, pressed) in [
            (0.0, None),
            (0.1, Some(true)),
            (0.11, Some(false)),
            (0.15, Some(true)),
            (0.16, Some(false)),
        ] {
            let pos = rect.center();
            let events = pressed.map_or_else(Vec::new, |pressed| {
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]
            });
            let _ = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |root| {
                    egui::CentralPanel::default().show_inside(root, |ui| {
                        let response = roster_number(ui, 1, "user-b", selected);
                        rect = response.rect;
                        if response.clicked() {
                            clicks.push((selected, response.double_clicked()));
                            selected = true;
                        }
                    });
                },
            );
        }
        assert_eq!(clicks, [(false, false), (true, true)]);
    }
}

#[cfg(test)]
mod session_event_drain_tests {
    use super::*;
    use std::{sync::mpsc, thread};

    #[test]
    fn bounded_drain_returns_with_live_producer_and_preserves_tail() {
        let (sender, receiver) = mpsc::channel();
        for pid in 0..SESSION_EVENTS_PER_FRAME as u32 {
            sender.send(SessionEvent::Attaching(pid)).unwrap();
        }
        let (offered_sender, offered_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let producer = thread::spawn(move || {
            sender
                .send(SessionEvent::Attaching(SESSION_EVENTS_PER_FRAME as u32))
                .unwrap();
            offered_sender.send(()).unwrap();
            let _ = release_receiver.recv();
        });
        offered_receiver.recv().unwrap();

        let first = drain_session_events(&receiver);
        assert_eq!(first.len(), SESSION_EVENTS_PER_FRAME);
        let pids: Vec<_> = first
            .into_iter()
            .map(|event| match event {
                SessionEvent::Attaching(pid) => pid,
                other => panic!("unexpected event: {other:?}"),
            })
            .collect();
        assert_eq!(
            pids,
            (0..SESSION_EVENTS_PER_FRAME as u32).collect::<Vec<_>>()
        );
        assert!(!producer.is_finished(), "drain waited for producer exit");
        assert!(matches!(
            receiver.try_recv(),
            Ok(SessionEvent::Attaching(pid)) if pid == SESSION_EVENTS_PER_FRAME as u32
        ));

        release_sender.send(()).unwrap();
        producer.join().unwrap();
    }

    #[test]
    fn acknowledgement_and_terminal_remain_fifo_across_frames() {
        let (sender, receiver) = mpsc::channel();
        for pid in 0..SESSION_EVENTS_PER_FRAME as u32 {
            sender.send(SessionEvent::Attaching(pid)).unwrap();
        }
        sender.send(SessionEvent::Paused).unwrap();
        sender.send(SessionEvent::Configured(2)).unwrap();
        sender.send(SessionEvent::Stopped).unwrap();

        let first = drain_session_events(&receiver);
        assert_eq!(first.len(), SESSION_EVENTS_PER_FRAME);
        assert!(
            first
                .iter()
                .all(|event| matches!(event, SessionEvent::Attaching(_)))
        );
        let second = drain_session_events(&receiver);
        assert!(matches!(
            second.as_slice(),
            [
                SessionEvent::Paused,
                SessionEvent::Configured(2),
                SessionEvent::Stopped
            ]
        ));
        assert!(drain_session_events(&receiver).is_empty());
    }
}

#[cfg(test)]
mod attachment_notice_tests {
    use super::*;

    #[test]
    fn attachment_notice_requires_matching_completion_and_clears_on_stop() {
        let mut notice = AttachmentNotice::default();
        notice.receive(&SessionEvent::Attaching(42));
        assert!(
            notice
                .message()
                .unwrap()
                .contains("Preparing application 42")
        );
        notice.receive(&SessionEvent::AttachmentFinished {
            pid: 7,
            error: None,
        });
        assert_eq!(notice.opening, Some(42));
        // Non-attachment acknowledgements do not finish an in-flight native open.
        notice.receive(&SessionEvent::Configured(2));
        assert_eq!(notice.opening, Some(42));
        notice.receive(&SessionEvent::AttachmentFinished {
            pid: 42,
            error: None,
        });
        assert!(notice.message().is_none());
        notice.receive(&SessionEvent::Attaching(7));
        notice.receive(&SessionEvent::AttachmentFinished {
            pid: 7,
            error: Some("fixture failed".into()),
        });
        assert!(notice.message().unwrap().contains("fixture failed"));
        assert!(notice.opening.is_none());
        notice.receive(&SessionEvent::Stopped);
        assert!(notice.message().is_none());
    }
}

#[cfg(test)]
mod font_fallback_tests {
    use super::*;

    #[test]
    fn missing_optional_font_leaves_definitions_unchanged() {
        let unique = format!(
            "protoinputmultiplex-missing-font-{}-{}.ttf",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after the Unix epoch")
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        assert!(!path.exists(), "test font path unexpectedly exists");

        let mut fonts = egui::FontDefinitions::default();
        let original_families = fonts.families.clone();
        let original_font_names: Vec<_> = fonts.font_data.keys().cloned().collect();

        assert!(!append_system_font_fallback(&mut fonts, &path));
        assert_eq!(fonts.families, original_families);
        assert_eq!(
            fonts.font_data.keys().cloned().collect::<Vec<_>>(),
            original_font_names
        );
    }
}

#[cfg(test)]
mod runtime_roster_tests {
    use super::*;

    fn profile() -> Profile {
        serde_json::from_value(serde_json::json!({"version":1,"concurrent":true,"seats":[
            {"id":"original","name":"Original","mouse":"virtual://mouse/1","keyboard":"virtual://keyboard/1","target_pid":0}
        ]})).unwrap()
    }
    fn roster(profile: &Profile) -> crate::virtual_users::State {
        crate::virtual_users::State {
            slots: profile
                .seats
                .iter()
                .enumerate()
                .map(|(user, seat)| crate::virtual_users::Slot {
                    user,
                    seat: seat.clone(),
                    active: true,
                    clone_of: None,
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn external_roster_appends_then_tombstones_without_overwriting_existing_drafts() {
        let mut profile = profile();
        let mut state = roster(&profile);
        let mut child = profile.seats[0].clone();
        child.id = "child".into();
        child.name = "Child".into();
        child.mouse = "virtual://mouse/2".into();
        state.slots.push(crate::virtual_users::Slot {
            user: 1,
            seat: child,
            active: true,
            clone_of: Some("original".into()),
        });
        merge_runtime_roster(&mut profile, &state, 1).unwrap();
        profile.seats[1].cursor.speed_x = 2.0;
        let graph = mux_core::replication::Definition::from_json(r#"{"version":1,"edges":[{"source":"original","target":"child","mouse":true,"keyboard":false,"text":false}]}"#).unwrap();
        profile.replication = graph;
        state.slots[1].active = false;
        state.revision = 2;
        merge_runtime_roster(&mut profile, &state, 1).unwrap();
        assert_eq!(profile.seats.len(), 2);
        assert_eq!(profile.seats[1].identity(), "child");
        assert_eq!(profile.seats[1].cursor.speed_x, 2.0);
        assert!(profile.replication.edges.is_empty());
    }

    #[test]
    fn mismatched_or_short_roster_cannot_partially_change_profile() {
        let mut profile = profile();
        let before = serde_json::to_value(&profile).unwrap();
        let mut state = roster(&profile);
        state.slots[0].seat.id = "replacement".into();
        assert!(merge_runtime_roster(&mut profile, &state, 1).is_err());
        state.slots.clear();
        assert!(merge_runtime_roster(&mut profile, &state, 1).is_err());
        assert_eq!(serde_json::to_value(&profile).unwrap(), before);
    }
}

#[cfg(test)]
mod window_selection_editor_tests {
    use super::*;
    use crate::window_selection::{Acknowledgement, Selection};

    fn candidate() -> Selection {
        Selection {
            request_id: "local:1".into(),
            user: 1,
            pid: 42,
            hwnd: 99,
            creation_token: "132456789012345678".into(),
        }
    }

    #[test]
    fn refreshed_inventory_requires_the_same_window_and_lossless_process_identity() {
        let original = candidate();
        let mut windows = vec![(
            Target {
                pid: original.pid,
                hwnd: original.hwnd,
                title: "App".into(),
            },
            targets::TargetEligibility {
                pid: original.pid,
                creation: Some(132456789012345678),
                creation_token: Some(original.creation_token.clone()),
                state: targets::EligibilityState::Fresh,
                reason: "Ready".into(),
            },
        )];
        assert_eq!(selection_snapshot_index(&original, &windows), Some(0));
        windows[0].1.creation_token = Some("132456789012345679".into());
        assert_eq!(selection_snapshot_index(&original, &windows), None);
        windows[0].1.creation_token = Some(original.creation_token.clone());
        windows[0].0.hwnd += 1;
        assert_eq!(selection_snapshot_index(&original, &windows), None);
        windows[0].0.hwnd = original.hwnd;
        windows[0].0.pid += 1;
        assert_eq!(selection_snapshot_index(&original, &windows), None);
    }

    #[test]
    fn acknowledgement_is_bound_to_issued_request_and_original_user() {
        let pending = candidate();
        let mut ack = Acknowledgement {
            selection: pending.clone(),
            error: None,
        };
        assert!(selection_ack_matches(Some(&pending), &ack));
        ack.selection.request_id = "connected:1".into();
        assert!(!selection_ack_matches(Some(&pending), &ack));
        ack.selection = pending.clone();
        ack.selection.user = 2;
        assert!(!selection_ack_matches(Some(&pending), &ack));
        assert!(!selection_ack_matches(None, &ack));
        let mut unissued = pending;
        unissued.request_id.clear();
        ack.selection = unissued.clone();
        assert!(!selection_ack_matches(Some(&unissued), &ack));
    }
}

#[cfg(test)]
mod recovery_feedback_tests {
    use super::{initialize_controller_agent_access, routing_failure_footer};
    use crate::session_recovery::{Phase, Snapshot};

    #[test]
    fn successful_new_controller_discards_old_applied_and_draft_access() {
        use crate::{agent_runtime, agent_ui::AgentPage, gui_bridge};
        use std::collections::BTreeMap;

        let mut page = AgentPage::default();
        page.receive(agent_runtime::State {
            acknowledged: true,
            policy: agent_runtime::Policy {
                enabled: true,
                user_ids: vec!["old-user".into()],
                allowed_pids: vec![42],
                capture: true,
                desktop_capture: true,
                session_control: true,
                app_preferences: true,
            },
            clients: vec![47],
            cleanup: vec![agent_runtime::ClientCleanupState {
                client: 47,
                pending_owners: 2,
                attempts: 1,
                last_error: "old release obligation".into(),
            }],
            ..Default::default()
        });
        page.apply_script_fields(&BTreeMap::from([("agent_pids".into(), "123".into())]), &[]);
        page.request_failed("old policy request failed");
        assert!(page.client_is_enabled(47));
        assert!(page.allows_app_preferences(47));
        let before = page.snapshot();
        assert!(before.policy_enabled && before.capture && before.desktop_capture);
        assert!(before.session_control && before.app_preferences);
        assert_eq!(before.selected_users, ["old-user"]);
        assert_eq!(before.allowed_pids, [123]);
        assert_eq!(before.cleanup.len(), 1);
        assert!(before.error.is_some());

        // The same production boundary is called only by Controller::start's
        // successful branch, after old broker cleanup and start validation.
        let (mut app, _requests) = gui_bridge::app_channel();
        initialize_controller_agent_access(&mut page, &mut app, 2);
        let fresh = page.snapshot();
        assert!(!fresh.policy_enabled && fresh.clients.is_empty() && fresh.cleanup.is_empty());
        assert!(fresh.selected_users.is_empty() && fresh.allowed_pids.is_empty());
        assert!(!fresh.capture && !fresh.desktop_capture);
        assert!(!fresh.session_control && !fresh.app_preferences);
        assert!(!fresh.pending && fresh.error.is_none());
        assert!(!page.client_is_enabled(47));
        assert!(!page.allows_app_preferences(47));
    }

    #[test]
    fn only_a_reachable_matching_diagnostic_may_leave_the_footer() {
        let diagnostic = "raw cleanup diagnostic with private path";
        let mut retained = Snapshot {
            phase: Phase::Quarantined,
            error: Some(diagnostic.into()),
            targets: vec![],
            can_start: false,
        };
        let concise = routing_failure_footer(diagnostic.into(), &retained);
        assert!(!concise.contains(diagnostic));
        assert_eq!(retained.error.as_deref(), Some(diagnostic));
        retained.phase = Phase::Stopped;
        assert_eq!(
            routing_failure_footer(diagnostic.into(), &retained),
            diagnostic
        );
        retained.phase = Phase::Quarantined;
        let startup = "startup failed before attachment";
        assert_eq!(routing_failure_footer(startup.into(), &retained), startup);
        let broker = "Agent Access cleanup is incomplete: descriptor error";
        assert_eq!(routing_failure_footer(broker.into(), &retained), broker);
    }
}

#[cfg(test)]
mod mapper_publication_tests {
    use super::*;

    fn draw(
        ctx: &egui::Context,
        page: &mut crate::mapper_ui::MapperPage,
        events: Vec<egui::Event>,
    ) -> Option<crate::mapper_ui::PageAction> {
        let seat: Seat = serde_json::from_value(
            serde_json::json!({"id":"mapper-user","name":"Actor","mouse":"virtual://mouse/1"}),
        )
        .unwrap();
        let mut action = None;
        let mut action_frame = 0;
        let mut actions = BTreeSet::new();
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(680.0, 620.0),
                )),
                events,
                ..Default::default()
            },
            |root| {
                egui::CentralPanel::default().show_inside(root, |ui| {
                    action = page.show(
                        ui,
                        std::slice::from_ref(&seat),
                        &[true],
                        true,
                        true,
                        false,
                        true,
                        None,
                        &mut action_frame,
                        &mut actions,
                    );
                });
            },
        );
        action
    }

    fn click(
        ctx: &egui::Context,
        page: &mut crate::mapper_ui::MapperPage,
        label: &str,
    ) -> Option<crate::mapper_ui::PageAction> {
        let (rect, _) = ctx
            .data(|data| {
                data.get_temp::<(egui::Rect, egui::Rect)>(egui::Id::new(("mapper-response", label)))
            })
            .unwrap();
        let mut result = None;
        for pressed in [true, false] {
            result = draw(
                ctx,
                page,
                vec![
                    egui::Event::PointerMoved(rect.center()),
                    egui::Event::PointerButton {
                        pos: rect.center(),
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        result
    }

    #[test]
    fn mapper_recovery_footer_preserves_unrelated_status_and_local_errors() {
        let old = "Mapper delay stopped: delay recipient changed";
        let mut status = format!("Input Mapper: {old}");
        update_mapper_status(&mut status, Some(old), None);
        assert_eq!(status, "Input Mapper state updated.");
        status = "Profile saved.".into();
        update_mapper_status(&mut status, Some(old), None);
        assert_eq!(status, "Profile saved.");
        update_mapper_status(&mut status, Some(old), Some(old));
        assert_eq!(status, format!("Input Mapper: {old}"));
        update_mapper_status(
            &mut status,
            Some(old),
            Some("Local acknowledgement remains uncertain"),
        );
        assert_eq!(
            status,
            "Input Mapper: Local acknowledgement remains uncertain"
        );
    }

    #[test]
    fn agent_publication_with_matching_id_cannot_complete_gui_apply() {
        let ctx = egui::Context::default();
        ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
        let mut page = crate::mapper_ui::MapperPage::default();
        let _ = draw(&ctx, &mut page, vec![]);
        let _ = click(&ctx, &mut page, "Browse mapper nodes");
        let _ = draw(&ctx, &mut page, vec![]);
        page.apply_script_fields(&std::collections::BTreeMap::from([
            ("mapper_node_search".into(), "pointer".into()),
            ("mapper_node_category".into(), "sources".into()),
        ]));
        let _ = click(&ctx, &mut page, "Search mapper nodes");
        let _ = draw(&ctx, &mut page, vec![]);
        let _ = click(&ctx, &mut page, "Choose mapper node pointer");
        let _ = draw(&ctx, &mut page, vec![]);
        let _ = click(&ctx, &mut page, "Add mapper node");
        let _ = draw(&ctx, &mut page, vec![]);
        let Some(crate::mapper_ui::PageAction::Apply {
            command_id,
            definition,
            expected_revision,
        }) = click(&ctx, &mut page, "Apply input mapper")
        else {
            panic!("the real Apply control did not create a pending command");
        };
        let publication = crate::mapper_runtime::State {
            acknowledged: true,
            reply_to: Some(command_id),
            definition: definition.clone(),
            revision: expected_revision + 1,
            sequence: 1,
            ..Default::default()
        };
        assert_eq!(mapper_reply_id(&publication, MapperPublisher::Agent), None);
        assert!(receive_mapper_page(&mut page, &publication, MapperPublisher::Agent).is_none());
        assert!(
            matches!(page.snapshot().pending, Some(crate::mapper_ui::Pending::Apply { command_id: pending, .. }) if pending == command_id)
        );
        assert_eq!(
            receive_mapper_page(&mut page, &publication, MapperPublisher::Direct),
            Some(definition)
        );
        assert!(page.snapshot().pending.is_none());
    }
}

#[cfg(test)]
mod scroll_context_tests {
    use super::*;

    #[derive(Clone, Copy)]
    enum Content {
        Page(Page),
        User {
            index: usize,
            seats: [&'static str; 2],
            tab: UserTab,
        },
        Tool {
            seat: &'static str,
            tab: UserTab,
        },
    }

    struct Sample {
        heading: egui::Rect,
        editor: egui::Rect,
        lower_button: egui::Rect,
        clip: egui::Rect,
        offset: f32,
        lower_clicked: bool,
    }

    struct Harness {
        ctx: egui::Context,
        size: egui::Vec2,
        time: f64,
        draft: String,
    }

    impl Harness {
        fn new(size: egui::Vec2) -> Self {
            let ctx = egui::Context::default();
            Theme::install(&ctx, Mode::Dark);
            Self {
                ctx,
                size,
                time: 0.0,
                draft: String::new(),
            }
        }

        fn frame(&mut self, content: Content, events: Vec<egui::Event>) -> Sample {
            self.time += 1.0 / 60.0;
            let mut sample = None;
            let _ = self.ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.size)),
                    time: Some(self.time),
                    events,
                    ..Default::default()
                },
                |root| {
                    egui::CentralPanel::default().show_inside(root, |ui| {
                        let mut show_body = |ui: &mut egui::Ui, scroll: egui::ScrollArea| {
                            let output = scroll.auto_shrink([false, false]).show(ui, |ui| {
                                let clip = ui.clip_rect();
                                let heading = Heading::new("Context heading").h2().show(ui).rect;
                                let editor = Input::new(&mut self.draft)
                                    .id_source("context-draft")
                                    .show(ui)
                                    .rect;
                                let mut lower_button = egui::Rect::NOTHING;
                                let mut lower_clicked = false;
                                for row in 0..40 {
                                    let response =
                                        Button::new(format!("Context row {row}")).show(ui);
                                    if row == 8 {
                                        lower_button = response.rect;
                                        lower_clicked = response.clicked();
                                    }
                                }
                                (heading, editor, lower_button, clip, lower_clicked)
                            });
                            let (heading, editor, lower_button, clip, lower_clicked) = output.inner;
                            sample = Some(Sample {
                                heading,
                                editor,
                                lower_button,
                                clip,
                                lower_clicked,
                                offset: output.state.offset.y,
                            });
                        };
                        match content {
                            Content::Page(page) => show_body(ui, page_scroll_area(page)),
                            Content::User { index, seats, tab } => {
                                show_body(ui, user_tab_scroll_area(seats[index], tab));
                            }
                            Content::Tool { seat, tab } => {
                                with_user_tab_content(ui, seat, tab, |ui| {
                                    show_body(
                                        ui,
                                        egui::ScrollArea::vertical().id_salt("owned-tool-body"),
                                    );
                                });
                            }
                        }
                    });
                },
            );
            sample.expect("scroll body rendered")
        }

        fn click(&mut self, content: Content, point: egui::Pos2) -> Sample {
            self.frame(
                content,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::default(),
                    },
                ],
            );
            self.frame(
                content,
                vec![egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                }],
            )
        }

        fn enter_draft(&mut self, content: Content) {
            let first = self.frame(content, vec![]);
            assert!(first.clip.contains_rect(first.editor));
            self.click(content, first.editor.center());
            self.frame(content, vec![egui::Event::Text("retained draft".into())]);
            assert_eq!(
                self.draft, "retained draft",
                "the real Ouroboros Input must receive text"
            );
        }

        fn scroll(&mut self, content: Content) -> Sample {
            let first = self.frame(content, vec![]);
            let mut last = self.frame(
                content,
                vec![
                    egui::Event::PointerMoved(first.clip.center()),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        phase: egui::TouchPhase::Move,
                        delta: egui::vec2(0.0, -200.0),
                        modifiers: egui::Modifiers::default(),
                    },
                ],
            );
            let mut settled = 0;
            for _ in 0..120 {
                let next = self.frame(content, vec![]);
                let quiet = self
                    .ctx
                    .input(|input| input.smooth_scroll_delta().length_sq() < 0.0001);
                settled = if quiet && (next.offset - last.offset).abs() < 0.01 {
                    settled + 1
                } else {
                    0
                };
                last = next;
                if settled == 3 {
                    break;
                }
            }
            assert_eq!(settled, 3, "wheel smoothing must settle before navigation");
            assert!(last.offset > 150.0 && !last.clip.contains_rect(last.heading));
            assert!(last.clip.contains_rect(last.lower_button));
            assert!(
                self.click(content, last.lower_button.center())
                    .lower_clicked,
                "a real lower-row control must become clickable after scrolling"
            );
            self.frame(content, vec![])
        }

        fn assert_fresh(&mut self, content: Content) {
            let sample = self.frame(content, vec![]);
            assert!(
                sample.clip.contains_rect(sample.heading),
                "a fresh context inherited a clipped heading: offset={} heading={:?} clip={:?}",
                sample.offset,
                sample.heading,
                sample.clip
            );
            assert!(sample.clip.contains_rect(sample.editor));
            assert!(
                sample.offset < 0.5,
                "fresh scroll context must start at the top"
            );
        }

        fn assert_restored(&mut self, content: Content, previous: &Sample) {
            let restored = self.frame(content, vec![]);
            assert!(
                (restored.heading.top() - previous.heading.top()).abs() < 0.5,
                "returning context must restore its visible content position: before={} after={}",
                previous.offset,
                restored.offset
            );
            assert!(restored.clip.contains_rect(restored.lower_button));
            assert!(
                self.click(content, restored.lower_button.center())
                    .lower_clicked
            );
            assert_eq!(
                self.draft, "retained draft",
                "navigation must retain the edited draft"
            );
        }
    }

    #[test]
    fn page_scroll_context_fresh_and_restored_with_real_wheel() {
        for size in [egui::vec2(320.0, 360.0), egui::vec2(680.0, 620.0)] {
            let mut harness = Harness::new(size);
            let first = Content::Page(Page::Lab);
            let second = Content::Page(Page::Screen);
            harness.enter_draft(first);
            let scrolled = harness.scroll(first);
            harness.assert_fresh(second);
            harness.scroll(second);
            harness.assert_restored(first, &scrolled);
        }
    }

    fn stable_user_context(tab: UserTab) {
        for size in [egui::vec2(320.0, 360.0), egui::vec2(680.0, 620.0)] {
            let mut harness = Harness::new(size);
            let first = Content::User {
                index: 0,
                seats: ["seat-a", "seat-b"],
                tab,
            };
            harness.enter_draft(first);
            let scrolled = harness.scroll(first);
            let other_tab = if tab == UserTab::Motion {
                UserTab::Wheel
            } else {
                UserTab::Motion
            };
            harness.assert_fresh(Content::User {
                index: 0,
                seats: ["seat-a", "seat-b"],
                tab: other_tab,
            });
            // Replacing the old row with another stable seat must not inherit its offset.
            harness.assert_fresh(Content::User {
                index: 0,
                seats: ["seat-b", "seat-a"],
                tab,
            });
            // Moving the original seat to another row must retain its own scroll context.
            harness.assert_restored(
                Content::User {
                    index: 1,
                    seats: ["seat-b", "seat-a"],
                    tab,
                },
                &scrolled,
            );
        }
    }

    #[test]
    fn user_scroll_context_uses_stable_seat_and_tab_with_real_wheel() {
        stable_user_context(UserTab::Motion);
    }

    #[test]
    fn keyboard_scroll_context_survives_reorder_and_index_reuse_with_real_wheel() {
        stable_user_context(UserTab::Keyboard);
    }
    #[test]
    fn early_tool_body_scroll_context_retains_control_input_and_position() {
        for size in [egui::vec2(320.0, 360.0), egui::vec2(680.0, 620.0)] {
            for tab in [
                UserTab::Target,
                UserTab::Draw,
                UserTab::Magnifier,
                UserTab::Macros,
            ] {
                let mut harness = Harness::new(size);
                let first = Content::Tool {
                    seat: "seat-a",
                    tab,
                };
                harness.enter_draft(first);
                let scrolled = harness.scroll(first);
                harness.assert_fresh(Content::Tool {
                    seat: "seat-b",
                    tab,
                });
                let other = if tab == UserTab::Target {
                    UserTab::Draw
                } else {
                    UserTab::Target
                };
                harness.assert_fresh(Content::Tool {
                    seat: "seat-a",
                    tab: other,
                });
                harness.assert_restored(first, &scrolled);
            }
        }
    }
}

pub(crate) struct GuiRunOptions {
    pub renderer: crate::GuiRenderer,
    pub script_screenshots: bool,
}

