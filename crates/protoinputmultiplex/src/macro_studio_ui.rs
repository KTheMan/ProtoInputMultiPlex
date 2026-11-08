//! Bounded per-user macro authoring, playback, and generation controls.
use crate::{macro_studio, ui_script::Script};
use eframe::egui;
use mux_core::{
    Seat,
    automation::{Action, Clip, TimedAction},
    macro_authoring::{Generator, Pattern},
};
use ouroboros_ui::atoms::{Button, ButtonVariant, Checkbox, Heading, Input, Slider, Text};
use ouroboros_ui::molecules::Collapsible;
use ouroboros_ui::organisms::Select;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

const PAGE_EVENTS: usize = 20;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ActionKind {
    #[default]
    Move,
    RelativeMove,
    MouseButton,
    Key,
    Text,
    Wheel,
}

#[derive(Clone, Copy)]
enum Pending {
    Record,
    Load,
    Edit,
    Generate,
    Reset,
    Play,
    Cancel,
    Inspect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RecordingControls {
    stop_local: bool,
    discard_agent: bool,
}

fn recording_controls(
    state: Option<&macro_studio::UserState>,
    session_active: bool,
    session_ready: bool,
    session_busy: bool,
    pending: bool,
) -> RecordingControls {
    let recording = state.is_some_and(|state| state.recording);
    let agent_recording =
        state.is_some_and(|state| state.recording && state.recording_client.is_some());
    RecordingControls {
        stop_local: session_active && !session_busy && !pending && recording && !agent_recording,
        discard_agent: session_ready && !session_busy && !pending && agent_recording,
    }
}

#[derive(Clone)]
pub enum PageAction {
    Worker(macro_studio::Command),
}

pub struct MacroStudioPage {
    user: usize,
    state: Option<macro_studio::State>,
    pending: Option<Pending>,
    error: Option<String>,
    file_path: String,
    event_index: usize,
    timestamp_index: Option<usize>,
    at_micros: u64,
    action_kind: ActionKind,
    action_x: i32,
    action_y: i32,
    action_button: u8,
    action_pressed: bool,
    action_key: String,
    action_text: String,
    wheel_delta: i16,
    wheel_horizontal: bool,
    advanced_json: bool,
    event_json: String,
    insert_json: String,
    page_start: usize,
    range_start: usize,
    range_end: usize,
    offset_micros: u64,
    shift_micros: i64,
    remap_from: usize,
    remap_to: usize,
    pattern: Pattern,
    center_x: i32,
    center_y: i32,
    duration_ms: u64,
    radius: u32,
    points: u32,
    click: Option<u8>,
    seed: u64,
    repeat: bool,
    loops: u32,
    speed: f64,
    delay_ms: u64,
    stop_on_escape: bool,
    keep_awake: bool,
    status: String,
}

impl Default for MacroStudioPage {
    fn default() -> Self {
        Self {
            user: 0,
            state: None,
            pending: None,
            error: None,
            file_path: "macros/user-1.json".into(),
            event_index: 0,
            timestamp_index: None,
            at_micros: 0,
            action_kind: ActionKind::Move,
            action_x: 300,
            action_y: 300,
            action_button: 0,
            action_pressed: true,
            action_key: "Space".into(),
            action_text: String::new(),
            wheel_delta: 120,
            wheel_horizontal: false,
            advanced_json: false,
            event_json: String::new(),
            insert_json: String::new(),
            page_start: 0,
            range_start: 0,
            range_end: 1,
            offset_micros: 1_000,
            shift_micros: 0,
            remap_from: 0,
            remap_to: 1,
            pattern: Pattern::Circle,
            center_x: 300,
            center_y: 300,
            duration_ms: 2_000,
            radius: 100,
            points: 32,
            click: None,
            seed: 1,
            repeat: false,
            loops: 1,
            speed: 1.0,
            delay_ms: 0,
            stop_on_escape: true,
            keep_awake: false,
            status: "No macro studio state yet.".into(),
        }
    }
}

impl MacroStudioPage {
    pub fn selected_user(&self) -> usize {
        self.user
    }

    /// Select the user whose controls are shown by an embedded workspace tab.
    /// A pending command may continue for its original user but cannot be retargeted.
    pub fn focus_user(&mut self, seats: &[Seat], user: usize) -> bool {
        if user >= seats.len() || (self.pending.is_some() && self.user != user) {
            return false;
        }
        self.user = user;
        true
    }

    pub fn has_active_program(&self) -> bool {
        self.state.as_ref().is_some_and(|state| {
            state.users.iter().any(|user| {
                user.recording
                    || user
                        .playback
                        .as_ref()
                        .is_some_and(|playback| playback.state == "playing")
            })
        })
    }

    pub fn reset_session(&mut self) {
        self.state = None;
        self.pending = None;
        self.error = None;
        self.status = "Routing session ended.".into();
    }

    pub fn apply_script_fields(&mut self, fields: &BTreeMap<String, String>) {
        let previous_event_index = self.event_index;
        let timestamp_scripted = fields.contains_key("studio_at_micros");
        for (name, value) in fields {
            match name.as_str() {
                "studio_user" => set_usize(&mut self.user, value),
                "studio_file_path" => self.file_path = value.clone(),
                "studio_event_index" => set_usize(&mut self.event_index, value),
                "studio_at_micros" => set_u64(&mut self.at_micros, value),
                "studio_event_json" => {
                    self.event_json = value.clone();
                    self.advanced_json = true;
                }
                "studio_action_type" => {
                    self.action_kind = action_kind(value).unwrap_or(self.action_kind)
                }
                "studio_action_x" => set_i32(&mut self.action_x, value),
                "studio_action_y" => set_i32(&mut self.action_y, value),
                "studio_action_button" => {
                    self.action_button = parse_button(value).unwrap_or(self.action_button)
                }
                "studio_action_key" => self.action_key = value.clone(),
                "studio_action_pressed" => {
                    self.action_pressed = parse_bool(value, self.action_pressed)
                }
                "studio_action_text" => self.action_text = value.clone(),
                "studio_action_delta" => {
                    if let Ok(delta) = value.parse() {
                        self.wheel_delta = delta;
                    }
                }
                "studio_action_horizontal" => {
                    self.wheel_horizontal = parse_bool(value, self.wheel_horizontal)
                }
                "studio_use_advanced_json" => {
                    self.advanced_json = parse_bool(value, self.advanced_json)
                }
                "studio_insert_json" => {
                    self.insert_json = value.clone();
                    self.event_json = value.clone();
                    self.advanced_json = true;
                }
                "studio_page_start" => set_usize(&mut self.page_start, value),
                "studio_range_start" => set_usize(&mut self.range_start, value),
                "studio_range_end" => set_usize(&mut self.range_end, value),
                "studio_offset_micros" => set_u64(&mut self.offset_micros, value),
                "studio_shift_micros" => set_i64(&mut self.shift_micros, value),
                "studio_remap_from" => set_usize(&mut self.remap_from, value),
                "studio_remap_to" => set_usize(&mut self.remap_to, value),
                "studio_pattern" => {
                    self.pattern = match value.to_ascii_lowercase().as_str() {
                        "circle" => Pattern::Circle,
                        "square" => Pattern::Square,
                        "triangle" => Pattern::Triangle,
                        "line" => Pattern::Line,
                        "figure_eight" => Pattern::FigureEight,
                        "jiggle" => Pattern::Jiggle,
                        _ => self.pattern,
                    }
                }
                "studio_center_x" => set_i32(&mut self.center_x, value),
                "studio_center_y" => set_i32(&mut self.center_y, value),
                "studio_duration_ms" => set_u64(&mut self.duration_ms, value),
                "studio_radius" => set_u32(&mut self.radius, value),
                "studio_points" => set_u32(&mut self.points, value),
                "studio_click" => {
                    self.click = if value.eq_ignore_ascii_case("none") || value.is_empty() {
                        None
                    } else {
                        value.parse::<u8>().ok().filter(|button| *button <= 4)
                    }
                }
                "studio_seed" => set_u64(&mut self.seed, value),
                "studio_repeat" => self.repeat = parse_bool(value, self.repeat),
                "studio_loops" => set_u32(&mut self.loops, value),
                "studio_speed" => set_f64(&mut self.speed, value),
                "studio_delay_ms" => set_u64(&mut self.delay_ms, value),
                "studio_stop_on_escape" => {
                    self.stop_on_escape = parse_bool(value, self.stop_on_escape)
                }
                "studio_keep_awake" => self.keep_awake = parse_bool(value, self.keep_awake),
                _ => (),
            }
        }
        if self.pattern == Pattern::Jiggle {
            self.click = None;
        }
        if timestamp_scripted {
            self.timestamp_index = Some(self.event_index);
        } else if previous_event_index != self.event_index {
            self.timestamp_index = None;
        }
    }

    pub fn receive(&mut self, mut state: macro_studio::State) {
        let error = state.error.clone();
        if let Some(previous) = &self.state {
            for incoming in &mut state.users {
                if let Some(old) = previous.users.iter().find(|old| old.user == incoming.user)
                    && incoming.clip.is_none()
                    && incoming.revision == old.revision
                    && incoming.loaded_events == old.loaded_events
                {
                    incoming.clip.clone_from(&old.clip);
                }
            }
            if !state.acknowledged && state.error.is_none() {
                state.error.clone_from(&previous.error);
            }
        }
        let acknowledged = state.acknowledged;
        self.state = Some(state);
        if !acknowledged {
            if let Some(error) = error {
                self.status.clone_from(&error);
                self.error = Some(error);
            }
            return;
        }
        self.pending = None;
        self.error = error;
        self.status = self
            .error
            .clone()
            .unwrap_or_else(|| "Macro Studio command acknowledged.".into());
        self.event_json.clear();
        self.insert_json.clear();
        self.timestamp_index = None;
    }

    /// Merge state delivered through Agent Access without treating its command
    /// acknowledgement as completion of this page's local request.
    pub fn receive_external(&mut self, mut state: macro_studio::State) {
        let successful_acknowledgement = state.acknowledged && state.error.is_none();
        let before = self.user_state().map(|user| {
            (
                user.revision,
                user.loaded_events,
                user.recorded_events,
                user.recording,
                user.recording_client,
                user.recording_error.clone(),
            )
        });
        let selected_user_changed = state
            .users
            .iter()
            .find(|user| user.user == self.user)
            .map(|user| {
                before.as_ref()
                    != Some(&(
                        user.revision,
                        user.loaded_events,
                        user.recorded_events,
                        user.recording,
                        user.recording_client,
                        user.recording_error.clone(),
                    ))
            })
            .unwrap_or(false);
        state.acknowledged = false;
        self.receive(state);
        if successful_acknowledgement && selected_user_changed && self.pending.is_none() {
            self.status = "Macro Studio updated by a connected agent.".into();
        }
    }

    fn user_state(&self) -> Option<&macro_studio::UserState> {
        self.user_state_for(self.user)
    }

    fn user_state_for(&self, user: usize) -> Option<&macro_studio::UserState> {
        self.state
            .as_ref()
            .and_then(|state| state.users.iter().find(|state| state.user == user))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        seats: &[Seat],
        show_user_picker: bool,
        session_active: bool,
        session_ready: bool,
        session_busy: bool,
        composite_active: bool,
        script: Option<&mut Script>,
        action_frame: &mut u64,
        frame_actions: &mut BTreeSet<String>,
    ) -> Option<PageAction> {
        let mut script = script;
        egui::ScrollArea::vertical()
            .id_salt("macro-studio-workspace")
            .show(ui, |ui| {
                if show_user_picker {
                    Heading::new("Macro Studio").heading().show(ui);
                }
                if seats.is_empty() {
                    ui.weak("Add users before creating a macro.");
                    return None;
                }
                self.user = self.user.min(seats.len() - 1);
                if show_user_picker {
                    ui.add_enabled_ui(self.pending.is_none(), |ui| {
                        ui.horizontal_wrapped(|ui| {
                            let options: Vec<_> = seats.iter().enumerate().map(|(index, seat)| format!("User {}: {}", index + 1, seat.name)).collect();
                            Select::new(&mut self.user).options(options).placeholder("Choose macro user").show(ui);
                        });
                    });
                }

            if let Some(state) = self.user_state() {
                if state.recording && state.recording_client.is_some() {
                    ui.strong(format!(
                        "Agent recording · client {} · {} events",
                        state.recording_client.unwrap_or_default(),
                        state.recorded_events
                    ));
                } else {
                    ui.strong(format!(
                        "{} · {} recorded · {} loaded · rev {}",
                        if state.recording {
                            "Recording"
                        } else {
                            state.playback.as_ref().map_or("Idle", |p| p.state.as_str())
                        },
                        state.recorded_events,
                        state.loaded_events,
                        state.revision
                    ));
                }
                if let Some(error) = &state.recording_error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                if state.agent_client.is_some() && state.recording_client.is_none() {
                    ui.weak("Agent-owned lane.");
                }
                if !state.recording && let Some(playback) = &state.playback {
                        ui.label(format!(
                            "Playback {} · {} events · {} loops · {:.2}x · {} ms delay",
                            playback.state, playback.events_emitted, playback.loops_completed,
                            playback.speed, playback.remaining_delay_ms
                        ));
                }
                if let Some(power) = state.power.as_ref().filter(|power| power.active) {
                    ui.label(format!(
                        "Keep-awake request active in process {} (display and system)",
                        power.process_id
                    ));
                }
            } else {
                ui.weak("No worker state for this user yet.");
            }
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            if self.pending.is_some() {
                ui.weak("Waiting for the Macro Studio worker acknowledgement.");
            }

            let recording_controls = recording_controls(
                self.user_state(),
                session_active,
                session_ready,
                session_busy,
                self.pending.is_some(),
            );
            let recording = self.user_state().is_some_and(|state| state.recording);
            let agent_recording = self.user_state().is_some_and(|state| state.recording && state.recording_client.is_some());
            let playing = self.user_state().and_then(|state| state.playback.as_ref()).is_some_and(|p| p.state == "playing");
            let state_available = self.user_state().is_some();
            let mut command = None;
            if recording && !agent_recording {
                if primary_button(ui, "Stop user recording", recording_controls.stop_local, script.as_deref_mut(), action_frame, frame_actions) {
                    self.pending = Some(Pending::Record);
                    command = Some(PageAction::Worker(macro_studio::Command::Record { user: self.user, active: false }));
                }
            } else if agent_recording {
                ui.horizontal_wrapped(|ui| {
                    if destructive_button(ui, "Discard agent recording", recording_controls.discard_agent, script.as_deref_mut(), action_frame, frame_actions) {
                        self.pending = Some(Pending::Cancel);
                        command = Some(PageAction::Worker(macro_studio::Command::Cancel { user: self.user }));
                    } else if quiet_button(ui, "Inspect user macro", session_ready && !session_busy && self.pending.is_none(), script.as_deref_mut(), action_frame, frame_actions) {
                        self.pending = Some(Pending::Inspect);
                        command = Some(PageAction::Worker(macro_studio::Command::Inspect { user: Some(self.user) }));
                    }
                });
            } else if playing {
                if primary_button(ui, "Cancel user playback", session_ready && !session_busy && self.pending.is_none() && playing, script.as_deref_mut(), action_frame, frame_actions) {
                    self.pending = Some(Pending::Cancel);
                    command = Some(PageAction::Worker(macro_studio::Command::Cancel { user: self.user }));
                }
            } else {
                ui.horizontal_wrapped(|ui| {
                    if primary_button(ui, "Start user recording", session_ready && !session_busy && !composite_active && self.pending.is_none() && !recording && !playing, script.as_deref_mut(), action_frame, frame_actions) {
                        self.pending = Some(Pending::Record);
                        command = Some(PageAction::Worker(macro_studio::Command::Record { user: self.user, active: true }));
                    }
                    if button(ui, "Play user macro", session_ready && !session_busy && !composite_active && self.pending.is_none() && !recording && !playing && self.user_state().is_some_and(|state| state.loaded_events > 0), script.as_deref_mut(), action_frame, frame_actions) {
                        self.pending = Some(Pending::Play);
                        command = Some(PageAction::Worker(macro_studio::Command::Play { user: self.user, options: macro_studio::PlaybackOptions { loops: (!self.repeat).then_some(self.loops.max(1)), speed: self.speed, delay_ms: self.delay_ms, stop_on_escape: self.stop_on_escape, keep_awake: self.keep_awake } }));
                    }
                    if quiet_button(ui, "Inspect user macro", session_ready && !session_busy && self.pending.is_none(), script.as_deref_mut(), action_frame, frame_actions) {
                        self.pending = Some(Pending::Inspect);
                        command = Some(PageAction::Worker(macro_studio::Command::Inspect { user: Some(self.user) }));
                    }
                    if quiet_button(ui, "Reset user macro", session_ready && !session_busy && self.pending.is_none() && !recording && !playing, script.as_deref_mut(), action_frame, frame_actions) {
                        self.pending = Some(Pending::Reset);
                        command = Some(PageAction::Worker(macro_studio::Command::Reset { user: self.user }));
                    }
                });
            }
            if command.is_some() { return command; }

            let mut file_action = None;
            let clip_files = Collapsible::new("Clip files")
                .id_source("macro-clip-files")
                .show(ui, |ui| {
            Input::new(&mut self.file_path).id_source("studio_file_path").show(ui);
            ui.horizontal_wrapped(|ui| {
                if quiet_button(ui, "Save user clip JSON", session_active && state_available && !recording && !playing && self.pending.is_none(), script.as_deref_mut(), action_frame, frame_actions) {
                    match self.user_state().and_then(|state| state.clip.as_ref()) {
                        Some(clip) => match save_clip(&self.file_path, clip) {
                            Ok(()) => self.status = format!("Saved {}", self.file_path),
                            Err(error) => self.error = Some(format!("{error:#}")),
                        },
                        None => self.error = Some("No acknowledged clip is available to save.".into()),
                    }
                }
                if quiet_button(ui, "Load user clip JSON", session_ready && !session_busy && !recording && !playing && self.pending.is_none(), script.as_deref_mut(), action_frame, frame_actions) {
                    let result = load_clip(&self.file_path);
                    match result {
                        Ok(clip) => {
                            self.pending = Some(Pending::Load);
                            self.error = None;
                            file_action = Some(PageAction::Worker(macro_studio::Command::Load { user: self.user, clip }));
                        }
                        Err(error) => self.error = Some(format!("{error:#}")),
                    }
                }
            });
                });
            let _ = script.as_deref_mut().is_none_or(|s| s.observe("Clip files", &clip_files));
            if file_action.is_some() { return file_action; }

            let mut timeline_action = None;
            let timeline = Collapsible::new("Timeline and edits")
                .id_source("macro-timeline-edits")
                .show(ui, |ui| {
            Heading::new("Timeline editor").heading().show(ui);
            let editor_clip = self.user_state().and_then(|state| state.clip.clone());
            let event_count = editor_clip.as_ref().map_or(0, |clip| clip.events.len());
            if let Some(clip) = editor_clip {
                ui.label(format!("{} events · timestamps in microseconds · pages of {}", event_count, PAGE_EVENTS));
                ui.horizontal_wrapped(|ui| {
                    ui.label("Page start");
                    ui.add(egui::DragValue::new(&mut self.page_start).range(0..=event_count));
                    if quiet_button(ui, "Previous timeline page", self.page_start >= PAGE_EVENTS && self.pending.is_none(), script.as_deref_mut(), action_frame, frame_actions) {
                        self.page_start = self.page_start.saturating_sub(PAGE_EVENTS);
                    }
                    if quiet_button(ui, "Next timeline page", self.page_start + PAGE_EVENTS < event_count && self.pending.is_none(), script.as_deref_mut(), action_frame, frame_actions) {
                        self.page_start = (self.page_start + PAGE_EVENTS).min(event_count.saturating_sub(1));
                    }
                });
                let page_end = self.page_start.saturating_add(PAGE_EVENTS).min(event_count);
                egui::Grid::new("macro_studio_timeline").striped(true).show(ui, |ui| {
                    ui.strong("Index"); ui.strong("Time µs"); ui.strong("Action"); ui.end_row();
                    for (index, event) in clip.events.iter().enumerate().skip(self.page_start).take(page_end.saturating_sub(self.page_start)) {
                        ui.label(index.to_string());
                        ui.label(event.at_micros.to_string());
                        ui.label(action_summary(&event.action));
                        ui.end_row();
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Selected event");
                    ui.add(egui::DragValue::new(&mut self.event_index).range(0..=event_count.saturating_sub(1)));
                    if self.timestamp_index != Some(self.event_index) {
                        self.at_micros = clip
                            .events
                            .get(self.event_index)
                            .map_or(0, |event| event.at_micros);
                        self.timestamp_index = Some(self.event_index);
                    }
                    if quiet_button(ui, "Use selected event", self.event_index < event_count, script.as_deref_mut(), action_frame, frame_actions)
                        && let Some(event) = clip.events.get(self.event_index)
                    {
                        self.load_typed_event(event);
                    }
                });
                let mut command = None;
                ui.horizontal_wrapped(|ui| {
                    if quiet_button(ui, "Delete selected event", session_ready && !session_busy && self.pending.is_none() && !recording && !playing && self.event_index < event_count, script.as_deref_mut(), action_frame, frame_actions) {
                        self.pending = Some(Pending::Edit);
                        command = Some(PageAction::Worker(macro_studio::Command::Edit { user: self.user, edit: mux_core::macro_authoring::Edit::Delete { start: self.event_index, end: self.event_index + 1 } }));
                    } else if quiet_button(ui, "Duplicate selected event", session_ready && !session_busy && self.pending.is_none() && !recording && !playing && self.event_index < event_count, script.as_deref_mut(), action_frame, frame_actions) {
                        self.pending = Some(Pending::Edit);
                        command = Some(PageAction::Worker(macro_studio::Command::Edit { user: self.user, edit: mux_core::macro_authoring::Edit::Duplicate { start: self.event_index, end: self.event_index + 1, offset_micros: self.offset_micros } }));
                    }
                });
                if command.is_some() { timeline_action = command; return; }
                egui::Grid::new("macro_studio_range_controls")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Range start (inclusive)");
                        ui.add(egui::DragValue::new(&mut self.range_start).range(0..=event_count));
                        ui.end_row();
                        ui.label("Range end (exclusive)");
                        ui.add(egui::DragValue::new(&mut self.range_end).range(0..=event_count));
                        ui.end_row();
                        ui.label("Duplicate offset (µs)");
                        ui.add(egui::DragValue::new(&mut self.offset_micros));
                        ui.end_row();
                        ui.label("Shift range by (µs)");
                        ui.add(egui::DragValue::new(&mut self.shift_micros));
                        ui.end_row();
                        ui.label("Copy from user");
                        ui.add(egui::DragValue::new(&mut self.remap_from).range(0..=seats.len().saturating_sub(1)));
                        ui.end_row();
                        ui.label("Copy to user");
                        ui.add(egui::DragValue::new(&mut self.remap_to).range(0..=seats.len().saturating_sub(1)));
                        ui.end_row();
                    });
                let remap_source_ready = self.user_state_for(self.remap_from).is_some_and(|state| {
                    state.loaded_events > 0
                        && state.clip.is_some()
                        && !state.recording
                        && state.playback.as_ref().is_none_or(|playback| playback.state != "playing")
                });
                let remap_target_idle = self.user_state_for(self.remap_to).is_some_and(|state| {
                    !state.recording
                        && state.playback.as_ref().is_none_or(|playback| playback.state != "playing")
                });
                let mut command = None;
                ui.horizontal_wrapped(|ui| {
                    if quiet_button(ui, "Delete event range", session_ready && !session_busy && self.pending.is_none() && !recording && !playing && self.range_start < self.range_end && self.range_end <= event_count, script.as_deref_mut(), action_frame, frame_actions) {
                        self.pending = Some(Pending::Edit);
                        command = Some(PageAction::Worker(macro_studio::Command::Edit { user: self.user, edit: mux_core::macro_authoring::Edit::Delete { start: self.range_start, end: self.range_end } }));
                    } else if quiet_button(ui, "Duplicate event range", session_ready && !session_busy && self.pending.is_none() && !recording && !playing && self.range_start < self.range_end && self.range_end <= event_count, script.as_deref_mut(), action_frame, frame_actions) {
                        self.pending = Some(Pending::Edit);
                        command = Some(PageAction::Worker(macro_studio::Command::Edit { user: self.user, edit: mux_core::macro_authoring::Edit::Duplicate { start: self.range_start, end: self.range_end, offset_micros: self.offset_micros } }));
                    } else if quiet_button(ui, "Shift event range", session_ready && !session_busy && self.pending.is_none() && !recording && !playing && self.range_start < self.range_end && self.range_end <= event_count, script.as_deref_mut(), action_frame, frame_actions) {
                        self.pending = Some(Pending::Edit);
                        command = Some(PageAction::Worker(macro_studio::Command::Edit { user: self.user, edit: mux_core::macro_authoring::Edit::Shift { start: self.range_start, end: self.range_end, delta_micros: self.shift_micros } }));
                    } else if quiet_button(ui, "Copy source macro to destination user", session_ready && !session_busy && self.pending.is_none() && remap_source_ready && remap_target_idle && self.remap_from < seats.len() && self.remap_to < seats.len() && self.remap_from != self.remap_to, script.as_deref_mut(), action_frame, frame_actions) {
                        self.pending = Some(Pending::Edit);
                        command = Some(PageAction::Worker(macro_studio::Command::Edit { user: self.remap_from, edit: mux_core::macro_authoring::Edit::Remap { from: self.remap_from, to: self.remap_to } }));
                    } else if quiet_button(ui, "Clear selected user clip", session_ready && !session_busy && self.pending.is_none() && !recording && !playing, script.as_deref_mut(), action_frame, frame_actions) {
                        self.pending = Some(Pending::Edit);
                        command = Some(PageAction::Worker(macro_studio::Command::Edit { user: self.user, edit: mux_core::macro_authoring::Edit::Clear }));
                    }
                });
                ui.weak("Copy keeps the source clip unchanged. Select the destination lane after acknowledgement to inspect or play its copy.");
                if command.is_some() { timeline_action = command; return; }
            } else {
                ui.weak("No clip yet. Author an action below, then insert it as the first event.");
            }

            if let Some(action) = self.action_editor(
                ui,
                &seats[self.user],
                event_count,
                session_ready,
                session_busy,
                recording,
                playing,
                script.as_deref_mut(),
                action_frame,
                frame_actions,
            ) {
                timeline_action = Some(action);
            }
                });
            let _ = script.as_deref_mut().is_none_or(|s| s.observe("Timeline and edits", &timeline));
            if timeline_action.is_some() { return timeline_action; }

            let mut generate_action = None;
            let generator = Collapsible::new("Pattern and jiggle")
                .id_source("macro-pattern-jiggle")
                .show(ui, |ui| {
            Heading::new("Pattern and jiggle generator").heading().show(ui);
            ui.horizontal_wrapped(|ui| {
                let patterns = ["Circle", "Square", "Triangle", "Line", "Figure eight", "Jiggle"];
                let mut selected = pattern_index(self.pattern);
                if Select::new(&mut selected).options(patterns.map(str::to_string)).placeholder("Choose generator").show(ui).changed() {
                    self.pattern = pattern_from_index(selected);
                    if self.pattern == Pattern::Jiggle {
                        self.click = None;
                    }
                }
            });
            let paired_columns = ui.available_width() >= 480.0;
            egui::Grid::new("macro_generator_fields")
                .num_columns(if paired_columns { 4 } else { 2 })
                .show(ui, |ui| {
                    ui.label("Center X"); ui.add(egui::DragValue::new(&mut self.center_x));
                    if !paired_columns { ui.end_row(); }
                    ui.label("Center Y"); ui.add(egui::DragValue::new(&mut self.center_y));
                    ui.end_row();
                    ui.label("Duration ms"); ui.add(egui::DragValue::new(&mut self.duration_ms).range(1..=86_400_000));
                    if !paired_columns { ui.end_row(); }
                    ui.label("Radius px"); ui.add(egui::DragValue::new(&mut self.radius).range(1..=100_000));
                    ui.end_row();
                    ui.label("Points"); ui.add(egui::DragValue::new(&mut self.points).range(3..=100_000));
                    if !paired_columns { ui.end_row(); }
                    ui.label("Seed"); ui.add(egui::DragValue::new(&mut self.seed));
                    ui.end_row();
                });
            ui.horizontal_wrapped(|ui| {
                ui.label("Click at each point");
                let click_names = ["None", "Primary", "Secondary", "Middle", "Back", "Forward"];
                let mut click_index = self.click.map_or(0, |button| usize::from(button) + 1);
                if Select::new(&mut click_index).options(click_names.map(str::to_string)).placeholder("No click").show(ui).changed() {
                    self.click = (self.pattern != Pattern::Jiggle)
                        .then(|| click_index.checked_sub(1).and_then(|button| u8::try_from(button).ok()))
                        .flatten();
                }
                if self.pattern == Pattern::Jiggle {
                    ui.weak("Jiggle generates movement only; click generation is unavailable for this pattern.");
                }
            });
            Text::new("Jiggle produces virtual movement only. Keep-awake uses a reversible OS power request while playback runs; it never injects global input.").caption().wrap().show(ui);
            if primary_button(ui, "Generate user macro", session_ready && !session_busy && self.pending.is_none() && !recording && !playing, script.as_deref_mut(), action_frame, frame_actions) {
                let settings = Generator { shape: self.pattern, user: self.user, center: (self.center_x, self.center_y), duration_micros: self.duration_ms.saturating_mul(1_000), radius: self.radius, points: self.points, click: self.click, seed: self.seed };
                self.pending = Some(Pending::Generate);
                generate_action = Some(PageAction::Worker(macro_studio::Command::Generate { user: self.user, settings }));
            }
                });
            let _ = script.as_deref_mut().is_none_or(|s| s.observe("Pattern and jiggle", &generator));
            if generate_action.is_some() { return generate_action; }

            let playback_options = Collapsible::new("Playback options")
                .id_source("macro-playback-options")
                .show(ui, |ui| {
            Heading::new("Playback options").heading().show(ui);
            Checkbox::new(&mut self.repeat).label("Repeat until cancelled").enabled(!playing && !recording && self.pending.is_none()).show(ui);
            if !self.repeat {
                ui.label("Playback loops");
                ui.add_enabled(!playing && !recording && self.pending.is_none(), egui::DragValue::new(&mut self.loops).range(1..=1_000_000));
            }
            ui.label(format!("Speed: {:.2}x (0.01–100x)", self.speed));
            let mut speed = self.speed as f32;
            Slider::new(&mut speed).range(0.01, 100.0).step(0.01).show(ui);
            self.speed = f64::from(speed);
            ui.horizontal_wrapped(|ui| {
                ui.label("Start delay ms"); ui.add(egui::DragValue::new(&mut self.delay_ms).range(0..=3_600_000));
                Checkbox::new(&mut self.stop_on_escape).label("Stop on Escape in this window").show(ui);
                Checkbox::new(&mut self.keep_awake).label("Request awake during playback").show(ui);
            });
                });
            let _ = script.is_none_or(|s| s.observe("Playback options", &playback_options));
            if self.user_state().is_none() || self.status != "No macro studio state yet." {
                ui.label(&self.status);
            }
            None
        }).inner
    }

    #[allow(clippy::too_many_arguments)]
    fn action_editor(
        &mut self,
        ui: &mut egui::Ui,
        seat: &Seat,
        event_count: usize,
        session_ready: bool,
        session_busy: bool,
        recording: bool,
        playing: bool,
        script: Option<&mut Script>,
        action_frame: &mut u64,
        frame_actions: &mut BTreeSet<String>,
    ) -> Option<PageAction> {
        let mut script = script;
        Heading::new("Author action").heading().show(ui);
        ui.label("Write a friendly virtual action into the selected user's clip.");
        ui.horizontal_wrapped(|ui| {
            ui.label("Timestamp (microseconds)");
            ui.add(egui::DragValue::new(&mut self.at_micros));
            let names = action_kind_names();
            let mut selected = action_kind_index(self.action_kind);
            if Select::new(&mut selected)
                .options(names.iter().map(|name| (*name).to_owned()))
                .placeholder("Action type")
                .show(ui)
                .changed()
            {
                self.action_kind = action_kind_at(selected);
            }
        });
        match self.action_kind {
            ActionKind::Move => {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Desktop X");
                    ui.add(egui::DragValue::new(&mut self.action_x));
                    ui.label("Desktop Y");
                    ui.add(egui::DragValue::new(&mut self.action_y));
                });
            }
            ActionKind::RelativeMove => {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Delta X");
                    ui.add(egui::DragValue::new(&mut self.action_x).range(-32767..=32767));
                    ui.label("Delta Y");
                    ui.add(egui::DragValue::new(&mut self.action_y).range(-32767..=32767));
                });
            }
            ActionKind::MouseButton => {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Mouse button");
                    let names = ["Primary", "Secondary", "Middle", "Back", "Forward"];
                    let mut selected = usize::from(self.action_button.min(4));
                    if Select::new(&mut selected)
                        .options(names.map(str::to_owned))
                        .placeholder("Choose button")
                        .show(ui)
                        .changed()
                    {
                        self.action_button = selected.min(4) as u8;
                    }
                    Checkbox::new(&mut self.action_pressed)
                        .label("Pressed")
                        .show(ui);
                });
            }
            ActionKind::Key => {
                if seat.keyboard.is_none() {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        "This user has no assigned virtual keyboard.",
                    );
                }
                ui.horizontal_wrapped(|ui| {
                    ui.label("Named key");
                    Input::new(&mut self.action_key)
                        .id_source("studio_action_key")
                        .show(ui);
                    Checkbox::new(&mut self.action_pressed)
                        .label("Pressed")
                        .show(ui);
                });
                ui.weak("Examples: A, Space, Left, F5, LeftShift, or Numpad2.");
            }
            ActionKind::Text => {
                if seat.keyboard.is_none() {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        "This user has no assigned virtual keyboard.",
                    );
                }
                ui.label("Unicode text");
                Input::new(&mut self.action_text)
                    .id_source("studio_action_text")
                    .show(ui);
            }
            ActionKind::Wheel => {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Wheel delta");
                    ui.add(egui::DragValue::new(&mut self.wheel_delta));
                    Checkbox::new(&mut self.wheel_horizontal)
                        .label("Horizontal axis")
                        .show(ui);
                });
            }
        }
        Checkbox::new(&mut self.advanced_json)
            .label("Use advanced TimedAction JSON instead")
            .show(ui);
        if self.advanced_json {
            Text::new("Advanced format is the core version-1 TimedAction schema. The timestamp above replaces its at_micros field; the action user must match this lane.")
                .caption()
                .wrap()
                .show(ui);
            Input::new(&mut self.event_json)
                .id_source("studio_event_json")
                .show(ui);
        }
        let can_edit =
            session_ready && !session_busy && self.pending.is_none() && !recording && !playing;
        let can_retime = can_edit && event_count > 0 && self.event_index < event_count;
        let mut command = None;
        ui.horizontal_wrapped(|ui| {
            if button(
                ui,
                "Insert event",
                can_edit,
                script.as_deref_mut(),
                action_frame,
                frame_actions,
            ) {
                command = self.make_event(seat).map(|event| {
                    self.pending = Some(Pending::Edit);
                    PageAction::Worker(macro_studio::Command::Edit {
                        user: self.user,
                        edit: mux_core::macro_authoring::Edit::Insert { event },
                    })
                });
            } else if button(
                ui,
                "Replace event",
                can_retime,
                script.as_deref_mut(),
                action_frame,
                frame_actions,
            ) {
                command = self.make_event(seat).map(|event| {
                    self.pending = Some(Pending::Edit);
                    PageAction::Worker(macro_studio::Command::Edit {
                        user: self.user,
                        edit: mux_core::macro_authoring::Edit::Replace {
                            index: self.event_index,
                            event,
                        },
                    })
                });
            } else if button(
                ui,
                "Retime selected event",
                can_retime,
                script,
                action_frame,
                frame_actions,
            ) {
                self.pending = Some(Pending::Edit);
                command = Some(PageAction::Worker(macro_studio::Command::Edit {
                    user: self.user,
                    edit: mux_core::macro_authoring::Edit::Retime {
                        index: self.event_index,
                        at_micros: self.at_micros,
                    },
                }));
            }
        });
        command
    }

    fn make_event(&mut self, seat: &Seat) -> Option<TimedAction> {
        let action = if self.advanced_json {
            match serde_json::from_str::<TimedAction>(&self.event_json) {
                Ok(event) if event.action.user() == self.user => event.action,
                Ok(_) => {
                    self.error = Some("Event user must match the selected clip lane.".into());
                    return None;
                }
                Err(error) => {
                    self.error = Some(format!("Invalid TimedAction JSON: {error}"));
                    return None;
                }
            }
        } else {
            match self.action_kind {
                ActionKind::Move => Action::MoveTo {
                    user: self.user,
                    x: self.action_x,
                    y: self.action_y,
                },
                ActionKind::RelativeMove => {
                    let device = placeholder_device(mux_core::DeviceKind::Mouse, self.user);
                    match device {
                        Ok(device) => Action::Packet {
                            user: self.user,
                            packet: mux_core::emulated::Packet::motion(
                                device,
                                self.action_x,
                                self.action_y,
                            ),
                        },
                        Err(error) => {
                            self.error = Some(format!("{error:#}"));
                            return None;
                        }
                    }
                }
                ActionKind::MouseButton => {
                    match placeholder_device(mux_core::DeviceKind::Mouse, self.user) {
                        Ok(device) => Action::Packet {
                            user: self.user,
                            packet: mux_core::emulated::Packet::button(
                                device,
                                u32::from(self.action_button.min(4)),
                                !self.action_pressed,
                            ),
                        },
                        Err(error) => {
                            self.error = Some(format!("{error:#}"));
                            return None;
                        }
                    }
                }
                ActionKind::Key => {
                    if seat.keyboard.is_none() {
                        self.error =
                            Some("The selected user has no assigned virtual keyboard.".into());
                        return None;
                    }
                    let key = match mux_windows::keyboard::code(&self.action_key) {
                        Ok(key) => key,
                        Err(error) => {
                            self.error = Some(format!("{error:#}"));
                            return None;
                        }
                    };
                    match placeholder_device(mux_core::DeviceKind::Keyboard, self.user).and_then(
                        |device| {
                            mux_windows::keyboard::packet_code(device, key, !self.action_pressed)
                        },
                    ) {
                        Ok(packet) => Action::Packet {
                            user: self.user,
                            packet,
                        },
                        Err(error) => {
                            self.error = Some(format!("{error:#}"));
                            return None;
                        }
                    }
                }
                ActionKind::Text => {
                    if seat.keyboard.is_none() {
                        self.error =
                            Some("The selected user has no assigned virtual keyboard.".into());
                        return None;
                    }
                    Action::Text {
                        user: self.user,
                        text: self.action_text.clone(),
                    }
                }
                ActionKind::Wheel => {
                    match placeholder_device(mux_core::DeviceKind::Mouse, self.user) {
                        Ok(device) => Action::Packet {
                            user: self.user,
                            packet: if self.wheel_horizontal {
                                mux_core::emulated::Packet::horizontal_wheel(
                                    device,
                                    self.wheel_delta,
                                )
                            } else {
                                mux_core::emulated::Packet::wheel(device, self.wheel_delta)
                            },
                        },
                        Err(error) => {
                            self.error = Some(format!("{error:#}"));
                            return None;
                        }
                    }
                }
            }
        };
        let event = TimedAction {
            at_micros: self.at_micros,
            action,
        };
        match Clip::new(vec![event.clone()]) {
            Ok(_) => Some(event),
            Err(error) => {
                self.error = Some(format!("Invalid action: {error:#}"));
                None
            }
        }
    }

    fn load_typed_event(&mut self, event: &TimedAction) {
        self.at_micros = event.at_micros;
        self.timestamp_index = Some(self.event_index);
        self.advanced_json = false;
        let supported = match &event.action {
            Action::MoveTo { x, y, .. } => {
                self.action_kind = ActionKind::Move;
                self.action_x = *x;
                self.action_y = *y;
                true
            }
            Action::Text { text, .. } => {
                self.action_kind = ActionKind::Text;
                self.action_text.clone_from(text);
                true
            }
            Action::Packet { packet, .. } if packet.kind == 1 => {
                if let Some(name) = key_name(packet.data) {
                    self.action_kind = ActionKind::Key;
                    self.action_key = name;
                    self.action_pressed = packet.flags & 1 == 0;
                    true
                } else {
                    false
                }
            }
            Action::Packet { packet, .. } if packet.kind == 0 && packet.flags == 0 => {
                self.action_kind = ActionKind::RelativeMove;
                self.action_x = packet.x;
                self.action_y = packet.y;
                true
            }
            Action::Packet { packet, .. }
                if packet.kind == 0 && matches!(packet.flags, 0x400 | 0x800) =>
            {
                self.action_kind = ActionKind::Wheel;
                self.wheel_delta = packet.data as u16 as i16;
                self.wheel_horizontal = packet.flags == 0x800;
                true
            }
            Action::Packet { packet, .. } if packet.kind == 0 => {
                if let Some((button, pressed)) = parse_button_packet(packet.flags) {
                    self.action_kind = ActionKind::MouseButton;
                    self.action_button = button;
                    self.action_pressed = pressed;
                    true
                } else {
                    false
                }
            }
            _ => false,
        };
        self.advanced_json = !supported;
        if !supported {
            self.event_json = serde_json::to_string(event).unwrap_or_default();
            self.error = Some("This event uses a packet form not represented by the friendly editor; advanced JSON is shown.".into());
        } else {
            self.error = None;
        }
    }
}

fn save_clip(path: &str, clip: &Clip) -> anyhow::Result<()> {
    let json = clip.to_json()?;
    let path = Path::new(path);
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, json)?;
    Ok(())
}

fn load_clip(path: &str) -> anyhow::Result<Clip> {
    let length = std::fs::metadata(path)?.len();
    anyhow::ensure!(
        length <= mux_core::automation::MAX_CLIP_JSON_BYTES as u64,
        "macro file is too large"
    );
    let bytes = std::fs::read(path)?;
    anyhow::ensure!(
        bytes.len() <= mux_core::automation::MAX_CLIP_JSON_BYTES,
        "macro file is too large"
    );
    Clip::from_json(std::str::from_utf8(&bytes)?)
}

fn pattern_index(pattern: Pattern) -> usize {
    match pattern {
        Pattern::Circle => 0,
        Pattern::Square => 1,
        Pattern::Triangle => 2,
        Pattern::Line => 3,
        Pattern::FigureEight => 4,
        Pattern::Jiggle => 5,
    }
}
fn pattern_from_index(index: usize) -> Pattern {
    match index {
        0 => Pattern::Circle,
        1 => Pattern::Square,
        2 => Pattern::Triangle,
        3 => Pattern::Line,
        4 => Pattern::FigureEight,
        _ => Pattern::Jiggle,
    }
}
fn action_kind_names() -> [&'static str; 6] {
    [
        "Move",
        "Relative move",
        "Mouse button",
        "Key",
        "Text",
        "Wheel",
    ]
}
fn action_kind_index(kind: ActionKind) -> usize {
    match kind {
        ActionKind::Move => 0,
        ActionKind::RelativeMove => 1,
        ActionKind::MouseButton => 2,
        ActionKind::Key => 3,
        ActionKind::Text => 4,
        ActionKind::Wheel => 5,
    }
}
fn action_kind_at(index: usize) -> ActionKind {
    match index {
        1 => ActionKind::RelativeMove,
        2 => ActionKind::MouseButton,
        3 => ActionKind::Key,
        4 => ActionKind::Text,
        5 => ActionKind::Wheel,
        _ => ActionKind::Move,
    }
}
fn action_kind(value: &str) -> Option<ActionKind> {
    Some(
        match value.to_ascii_lowercase().replace([' ', '-'], "_").as_str() {
            "move" => ActionKind::Move,
            "relative_move" => ActionKind::RelativeMove,
            "mouse_button" | "button" => ActionKind::MouseButton,
            "key" => ActionKind::Key,
            "text" => ActionKind::Text,
            "wheel" => ActionKind::Wheel,
            _ => return None,
        },
    )
}
fn parse_button(value: &str) -> Option<u8> {
    match value.to_ascii_lowercase().as_str() {
        "0" | "primary" | "left" => Some(0),
        "1" | "secondary" | "right" => Some(1),
        "2" | "middle" => Some(2),
        "3" | "back" | "x1" => Some(3),
        "4" | "forward" | "x2" => Some(4),
        _ => None,
    }
}
fn parse_button_packet(flags: u32) -> Option<(u8, bool)> {
    (0..5).find_map(|button| {
        let down = 1 << (button * 2);
        let up = down << 1;
        if flags == down {
            Some((button as u8, true))
        } else if flags == up {
            Some((button as u8, false))
        } else {
            None
        }
    })
}
fn placeholder_device(kind: mux_core::DeviceKind, user: usize) -> anyhow::Result<u32> {
    let id = u32::try_from(
        user.checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("user index overflow"))?,
    )?;
    Ok(mux_core::emulated::device(kind, id)?.handle as u32)
}
fn key_name(code: u32) -> Option<String> {
    if (b'A' as u32..=b'Z' as u32).contains(&code) || (b'0' as u32..=b'9' as u32).contains(&code) {
        return char::from_u32(code).map(|key| key.to_string());
    }
    if (0x70..=0x87).contains(&code) {
        return Some(format!("F{}", code - 0x6F));
    }
    if (0x60..=0x69).contains(&code) {
        return Some(format!("Numpad{}", code - 0x60));
    }
    Some(
        match code {
            8 => "Backspace",
            9 => "Tab",
            13 => "Enter",
            16 => "Shift",
            17 => "Control",
            18 => "Alt",
            19 => "Pause",
            20 => "CapsLock",
            27 => "Escape",
            32 => "Space",
            33 => "PageUp",
            34 => "PageDown",
            35 => "End",
            36 => "Home",
            37 => "Left",
            38 => "Up",
            39 => "Right",
            40 => "Down",
            44 => "PrintScreen",
            45 => "Insert",
            46 => "Delete",
            91 => "LeftWin",
            92 => "RightWin",
            93 => "Menu",
            106 => "Multiply",
            107 => "Add",
            109 => "Subtract",
            110 => "Decimal",
            111 => "Divide",
            144 => "NumLock",
            145 => "ScrollLock",
            160 => "LeftShift",
            161 => "RightShift",
            162 => "LeftCtrl",
            163 => "RightCtrl",
            164 => "LeftAlt",
            165 => "RightAlt",
            186 => "Semicolon",
            187 => "Equals",
            188 => "Comma",
            189 => "Minus",
            190 => "Period",
            191 => "Slash",
            192 => "Backtick",
            219 => "LeftBracket",
            220 => "Backslash",
            221 => "RightBracket",
            222 => "Quote",
            _ => return None,
        }
        .to_owned(),
    )
}
fn action_summary(action: &Action) -> String {
    match action {
        Action::MoveTo { x, y, .. } => format!("Move to ({x}, {y})"),
        Action::Text { text, .. } => format!("Type text ({} characters)", text.chars().count()),
        Action::Packet { packet, .. } if packet.kind == 1 => {
            let state = if packet.flags & 1 == 0 {
                "Press"
            } else {
                "Release"
            };
            format!(
                "{state} {}",
                key_name(packet.data).unwrap_or_else(|| "other key".into())
            )
        }
        Action::Packet { packet, .. } if packet.kind == 0 && packet.flags == 0 => {
            format!("Move by ({}, {})", packet.x, packet.y)
        }
        Action::Packet { packet, .. }
            if packet.kind == 0 && matches!(packet.flags, 0x400 | 0x800) =>
        {
            let axis = if packet.flags == 0x800 {
                "horizontal"
            } else {
                "vertical"
            };
            format!("Scroll {axis} ({})", packet.data as u16 as i16)
        }
        Action::Packet { packet, .. } if packet.kind == 0 => {
            if let Some((button, pressed)) = parse_button_packet(packet.flags) {
                let names = ["primary", "secondary", "middle", "back", "forward"];
                format!(
                    "{} {} button",
                    if pressed { "Press" } else { "Release" },
                    names[button as usize]
                )
            } else {
                "Mouse action (advanced)".into()
            }
        }
        _ => "Virtual action (advanced)".into(),
    }
}
fn set_usize(slot: &mut usize, value: &str) {
    if let Ok(parsed) = value.parse() {
        *slot = parsed;
    }
}
fn set_u32(slot: &mut u32, value: &str) {
    if let Ok(parsed) = value.parse() {
        *slot = parsed;
    }
}
fn set_u64(slot: &mut u64, value: &str) {
    if let Ok(parsed) = value.parse() {
        *slot = parsed;
    }
}
fn set_i32(slot: &mut i32, value: &str) {
    if let Ok(parsed) = value.parse() {
        *slot = parsed;
    }
}
fn set_i64(slot: &mut i64, value: &str) {
    if let Ok(parsed) = value.parse() {
        *slot = parsed;
    }
}
fn set_f64(slot: &mut f64, value: &str) {
    if let Ok(parsed) = value.parse::<f64>()
        && parsed.is_finite()
    {
        *slot = parsed.clamp(0.01, 100.0);
    }
}
fn parse_bool(value: &str, old: bool) -> bool {
    value.parse().unwrap_or(old)
}
fn button(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    script: Option<&mut Script>,
    action_frame: &mut u64,
    frame_actions: &mut BTreeSet<String>,
) -> bool {
    let response = Button::new(label)
        .variant(ButtonVariant::Outline)
        .enabled(enabled)
        .id_source(("macro_studio", label))
        .show(ui);
    let accepted = script.is_none_or(|script| script.observe(label, &response));
    if !accepted || !response.clicked() {
        return false;
    }
    let frame = ui.ctx().cumulative_frame_nr();
    if *action_frame != frame {
        *action_frame = frame;
        frame_actions.clear();
    }
    frame_actions.insert(label.to_owned())
}

fn quiet_button(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    script: Option<&mut Script>,
    action_frame: &mut u64,
    frame_actions: &mut BTreeSet<String>,
) -> bool {
    button(ui, label, enabled, script, action_frame, frame_actions)
}

fn destructive_button(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    script: Option<&mut Script>,
    action_frame: &mut u64,
    frame_actions: &mut BTreeSet<String>,
) -> bool {
    let response = Button::new(label)
        .variant(ButtonVariant::Destructive)
        .enabled(enabled)
        .id_source(("macro_studio_destructive", label))
        .show(ui);
    let accepted = script.is_none_or(|script| script.observe(label, &response));
    if !accepted || !response.clicked() {
        return false;
    }
    let frame = ui.ctx().cumulative_frame_nr();
    if *action_frame != frame {
        *action_frame = frame;
        frame_actions.clear();
    }
    frame_actions.insert(label.to_owned())
}

fn primary_button(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    script: Option<&mut Script>,
    action_frame: &mut u64,
    frame_actions: &mut BTreeSet<String>,
) -> bool {
    let response = Button::new(label)
        .variant(ButtonVariant::Default)
        .enabled(enabled)
        .id_source(("macro_studio_primary", label))
        .show(ui);
    let accepted = script.is_none_or(|script| script.observe(label, &response));
    if !accepted || !response.clicked() {
        return false;
    }
    let frame = ui.ctx().cumulative_frame_nr();
    if *action_frame != frame {
        *action_frame = frame;
        frame_actions.clear();
    }
    frame_actions.insert(label.to_owned())
}

#[cfg(test)]
mod focus_user_tests {
    use super::*;

    fn seats() -> Vec<Seat> {
        vec![
            Seat {
                id: "seat-a".into(),
                name: "A".into(),
                mouse: "mouse-a".into(),
                keyboard: Some("keyboard-a".into()),
                target_pid: 1,
                desktop_start: None,
                cursor: Default::default(),
                annotation: Default::default(),
                magnifier: Default::default(),
                output: Default::default(),
            },
            Seat {
                id: "seat-b".into(),
                name: "B".into(),
                mouse: "mouse-b".into(),
                keyboard: Some("keyboard-b".into()),
                target_pid: 2,
                desktop_start: None,
                cursor: Default::default(),
                annotation: Default::default(),
                magnifier: Default::default(),
                output: Default::default(),
            },
        ]
    }

    #[test]
    fn agent_recording_has_discard_control_but_never_local_stop() {
        let state = macro_studio::UserState {
            user: 0,
            recording: true,
            recorded_events: 2,
            loaded_events: 0,
            revision: 1,
            clip: None,
            playback: None,
            keep_awake: false,
            power: None,
            agent_client: None,
            recording_client: Some(17),
            recording_error: None,
        };

        assert_eq!(
            recording_controls(Some(&state), true, true, false, false),
            RecordingControls {
                stop_local: false,
                discard_agent: true,
            }
        );
        assert_eq!(
            recording_controls(Some(&state), true, true, false, true),
            RecordingControls {
                stop_local: false,
                discard_agent: false,
            }
        );
    }

    #[test]
    fn focus_keeps_pending_playback_user_and_preserves_each_users_clip_state() {
        let seats = seats();
        let mut page = MacroStudioPage {
            state: Some(macro_studio::State {
                acknowledged: false,
                users: vec![
                    macro_studio::UserState {
                        user: 0,
                        recording: false,
                        recorded_events: 2,
                        loaded_events: 3,
                        revision: 1,
                        clip: None,
                        playback: None,
                        keep_awake: false,
                        power: None,
                        agent_client: None,
                        recording_client: None,
                        recording_error: None,
                    },
                    macro_studio::UserState {
                        user: 1,
                        recording: false,
                        recorded_events: 5,
                        loaded_events: 7,
                        revision: 2,
                        clip: None,
                        playback: None,
                        keep_awake: false,
                        power: None,
                        agent_client: None,
                        recording_client: None,
                        recording_error: None,
                    },
                ],
                error: None,
            }),
            pending: Some(Pending::Play),
            ..Default::default()
        };

        assert!(!page.focus_user(&seats, 1));
        assert!(!page.focus_user(&seats, seats.len()));
        assert_eq!(page.selected_user(), 0);
        assert_eq!(page.user_state().map(|state| state.loaded_events), Some(3));
        assert!(matches!(page.pending, Some(Pending::Play)));

        page.pending = None;
        assert!(page.focus_user(&seats, 1));
        assert_eq!(page.user_state().map(|state| state.loaded_events), Some(7));
        assert_eq!(
            page.user_state_for(0).map(|state| state.loaded_events),
            Some(3)
        );
    }

    #[test]
    fn external_agent_ack_refreshes_lane_without_consuming_local_edit() {
        let clip = |count| {
            Clip::new(
                (0..count)
                    .map(|x| TimedAction {
                        at_micros: x as u64,
                        action: Action::MoveTo { user: 0, x, y: 0 },
                    })
                    .collect(),
            )
            .unwrap()
        };
        let mut page = MacroStudioPage {
            user: 0,
            event_json: "local unsubmitted action".into(),
            pending: Some(Pending::Edit),
            state: Some(macro_studio::State {
                acknowledged: false,
                users: vec![macro_studio::UserState {
                    user: 0,
                    recording: false,
                    recorded_events: 0,
                    loaded_events: 1,
                    revision: 3,
                    clip: Some(clip(1)),
                    playback: None,
                    keep_awake: false,
                    power: None,
                    agent_client: None,
                    recording_client: None,
                    recording_error: None,
                }],
                error: None,
            }),
            ..Default::default()
        };

        page.receive_external(macro_studio::State {
            acknowledged: true,
            users: vec![macro_studio::UserState {
                user: 0,
                recording: true,
                recorded_events: 2,
                loaded_events: 2,
                revision: 4,
                clip: Some(clip(2)),
                playback: None,
                keep_awake: false,
                power: None,
                agent_client: None,
                recording_client: Some(17),
                recording_error: None,
            }],
            error: None,
        });

        assert!(matches!(page.pending, Some(Pending::Edit)));
        assert_eq!(page.event_json, "local unsubmitted action");
        assert_eq!(page.user_state().map(|state| state.revision), Some(4));
        assert_eq!(page.user_state().map(|state| state.loaded_events), Some(2));
        assert_eq!(
            page.status, "No macro studio state yet.",
            "external ACK must not consume a pending local edit"
        );

        page.pending = None;
        page.receive_external(macro_studio::State {
            acknowledged: true,
            users: vec![macro_studio::UserState {
                user: 0,
                recording: false,
                recorded_events: 0,
                loaded_events: 3,
                revision: 5,
                clip: Some(clip(3)),
                playback: None,
                keep_awake: false,
                power: None,
                agent_client: None,
                recording_client: None,
                recording_error: None,
            }],
            error: None,
        });
        assert_eq!(page.user_state().map(|state| state.revision), Some(5));
        assert_eq!(
            page.user_state()
                .and_then(|state| state.clip.as_ref())
                .map(|clip| clip.events.len()),
            Some(3)
        );
        assert_eq!(page.event_json, "local unsubmitted action");
        assert_eq!(page.status, "Macro Studio updated by a connected agent.");
    }

    #[test]
    fn external_ack_for_another_lane_does_not_change_selected_lane_status() {
        let lane = |user, revision, recorded_events, recording_client: Option<u64>| {
            macro_studio::UserState {
                user,
                recording: recording_client.is_some(),
                recorded_events,
                loaded_events: 0,
                revision,
                clip: None,
                playback: None,
                keep_awake: false,
                power: None,
                agent_client: None,
                recording_client,
                recording_error: None,
            }
        };
        let mut page = MacroStudioPage {
            user: 0,
            state: Some(macro_studio::State {
                acknowledged: false,
                users: vec![lane(0, 1, 0, None), lane(1, 2, 0, None)],
                error: None,
            }),
            status: "Waiting for a local edit acknowledgement.".into(),
            ..Default::default()
        };

        page.receive_external(macro_studio::State {
            acknowledged: true,
            users: vec![lane(0, 1, 0, None), lane(1, 3, 1, Some(42))],
            error: None,
        });

        assert_eq!(page.user_state().map(|state| state.revision), Some(1));
        assert_eq!(page.user_state_for(1).map(|state| state.revision), Some(3));
        assert_eq!(page.status, "Waiting for a local edit acknowledgement.");
    }
}
