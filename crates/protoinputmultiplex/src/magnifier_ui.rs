//! Ouroboros controls for the virtual-session magnifier.
use crate::{magnifier_runtime, ui_script::Script};
use eframe::egui::{self, Color32};
use mux_core::{
    Seat,
    desktop::Bounds,
    magnifier::{DockSide, Mode, Settings, Source},
};
use mux_windows::monitors::Monitor;
use ouroboros_ui::atoms::{Button, ButtonVariant, Checkbox, Divider, Heading, Input, Slider, Text};
use ouroboros_ui::cells::MenuItem;
use ouroboros_ui::molecules::Collapsible;
use ouroboros_ui::organisms::Select;
use std::collections::{BTreeMap, BTreeSet};

pub enum PageAction {
    SaveToProfile { user: usize, settings: Settings },
    Worker(magnifier_runtime::Command),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MagnifierAccessKey {
    Border,
    AutoRefresh,
}

fn magnifier_access_key(ctx: &egui::Context) -> Option<MagnifierAccessKey> {
    if egui::Popup::is_any_open(ctx) || ctx.any_popup_open() || ctx.text_edit_focused() {
        return None;
    }
    ctx.input(|input| {
        if !input.focused {
            return None;
        }
        input.events.iter().find_map(|event| {
            let egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } = event
            else {
                return None;
            };
            if *modifiers != egui::Modifiers::ALT {
                return None;
            }
            match key {
                egui::Key::B => Some(MagnifierAccessKey::Border),
                egui::Key::U => Some(MagnifierAccessKey::AutoRefresh),
                _ => None,
            }
        })
    })
}

#[derive(Clone)]
enum Pending {
    Configure { user: usize, settings: Settings },
    Refresh,
    Export,
}

#[derive(Default)]
pub struct MagnifierPage {
    selected_user: usize,
    committed: Vec<Settings>,
    drafts: Vec<Settings>,
    pending: Option<Pending>,
    state: Option<magnifier_runtime::State>,
    error: Option<String>,
    export_path: String,
    region_fields: [String; 4],
}

impl MagnifierPage {
    pub fn has_profile_draft(&self) -> bool {
        self.pending.is_some()
            || self.drafts != self.committed
            || self.drafts.get(self.selected_user).is_some_and(|draft| {
                let expected = draft.desktop_region.map_or_else(
                    || std::array::from_fn(|_| String::new()),
                    |region| {
                        [region.left, region.top, region.right, region.bottom]
                            .map(|value| value.to_string())
                    },
                );
                self.region_fields != expected
            })
    }
    pub fn reset_for_profile(&mut self) {
        *self = Self::default();
    }

    pub fn sync_profile(&mut self, seats: &[Seat]) {
        let next: Vec<_> = seats.iter().map(|seat| seat.magnifier.clone()).collect();
        let previous_selected = self.selected_user;
        let pending_user = match self.pending.as_ref() {
            Some(Pending::Configure { user, .. }) => Some(*user),
            _ => None,
        };
        self.committed.resize_with(next.len(), Settings::default);
        self.drafts.resize_with(next.len(), Settings::default);
        self.committed.truncate(next.len());
        self.drafts.truncate(next.len());
        let mut selected_draft_changed = false;
        for (index, settings) in next.iter().enumerate() {
            if self.committed[index] != *settings {
                self.committed[index] = settings.clone();
                if pending_user != Some(index) {
                    self.drafts[index] = settings.clone();
                    selected_draft_changed |= index == self.selected_user;
                }
            }
        }
        self.selected_user = self.selected_user.min(self.drafts.len().saturating_sub(1));
        if (selected_draft_changed || self.selected_user != previous_selected)
            && pending_user != Some(self.selected_user)
        {
            self.sync_region_fields();
        }
    }

    fn sync_region_fields(&mut self) {
        let Some(region) = self
            .drafts
            .get(self.selected_user)
            .and_then(|settings| settings.desktop_region)
        else {
            self.region_fields = std::array::from_fn(|_| String::new());
            return;
        };
        self.region_fields =
            [region.left, region.top, region.right, region.bottom].map(|value| value.to_string());
    }

    pub fn selected_user(&self) -> usize {
        self.selected_user
    }

    /// Select the user whose controls are shown by an embedded workspace tab.
    /// A pending command may continue for its original user but cannot be retargeted.
    pub fn focus_user(&mut self, seats: &[Seat], user: usize) -> bool {
        self.sync_profile(seats);
        if user >= seats.len() || (self.pending.is_some() && self.selected_user != user) {
            return false;
        }
        if self.selected_user != user {
            self.selected_user = user;
            self.sync_region_fields();
        }
        true
    }

    pub fn receive(&mut self, state: magnifier_runtime::State) -> Option<(usize, Settings)> {
        let acknowledged = state.acknowledged;
        let error = state.error.clone();
        let mut state = state;
        let invalidates_export = acknowledged
            && error.is_none()
            && matches!(
                self.pending.as_ref(),
                Some(Pending::Refresh | Pending::Configure { .. })
            );
        if state.export.is_none() && !invalidates_export {
            state.export = self
                .state
                .as_ref()
                .and_then(|previous| previous.export.clone());
        }
        self.state = Some(state);
        if !acknowledged {
            return None;
        }
        let pending = self.pending.take()?;
        self.error = error;
        let Pending::Configure { user, settings } = pending else {
            return None;
        };
        if self.error.is_some() {
            return None;
        }
        if let Some(committed) = self.committed.get_mut(user) {
            *committed = settings.clone();
        }
        if let Some(draft) = self.drafts.get_mut(user) {
            *draft = settings.clone();
        }
        if self.selected_user == user {
            self.sync_region_fields();
        }
        Some((user, settings))
    }

    /// Connected-agent command acknowledgements update telemetry and the profile,
    /// but cannot consume a local GUI command that is still pending.
    pub fn receive_external(&mut self, mut state: magnifier_runtime::State) {
        state.acknowledged = false;
        let _ = self.receive(state);
    }

    pub fn session_stopped(&mut self) {
        self.pending = None;
        self.state = None;
    }

    pub fn apply_script_fields(&mut self, fields: &BTreeMap<String, String>) {
        if let Some(value) = fields.get("magnifier_user")
            && let Ok(user) = value.parse::<usize>()
            && user < self.drafts.len()
        {
            self.selected_user = user;
            self.sync_region_fields();
        }
        let Some(settings) = self.drafts.get_mut(self.selected_user) else {
            return;
        };
        for (name, value) in fields {
            match name.as_str() {
                "magnifier_user" => (),
                "magnifier_mode" => {
                    settings.mode = match value.to_ascii_lowercase().as_str() {
                        "off" => Mode::Off,
                        "lens" => Mode::Lens,
                        "docked" => Mode::Docked,
                        "fullscreen" => Mode::Fullscreen,
                        _ => settings.mode,
                    };
                }
                "magnifier_source" => {
                    settings.source = match value.to_ascii_lowercase().as_str() {
                        "selected_target" | "target" => Source::SelectedTarget,
                        "desktop" => Source::Desktop,
                        _ => settings.source,
                    };
                }
                "magnifier_zoom" => set_f64(&mut settings.zoom, value, 1.0, 32.0),
                "magnifier_width" => set_u32(&mut settings.width, value, 32, 4096),
                "magnifier_height" => set_u32(&mut settings.height, value, 32, 4096),
                "magnifier_border" => settings.border = parse_bool(value, settings.border),
                "magnifier_border_red" => set_channel(&mut settings.border_color[0], value),
                "magnifier_border_green" => set_channel(&mut settings.border_color[1], value),
                "magnifier_border_blue" => set_channel(&mut settings.border_color[2], value),
                "magnifier_auto_refresh" => {
                    settings.auto_refresh = parse_bool(value, settings.auto_refresh)
                }
                "magnifier_refresh_hz" => set_u8(&mut settings.refresh_hz, value, 1, 30),
                "magnifier_monitor_id" => {
                    settings.monitor_id =
                        (!value.is_empty() && value != "none").then(|| value.clone())
                }
                "magnifier_work_area" => settings.work_area = parse_bool(value, settings.work_area),
                "magnifier_dock" => {
                    settings.dock = match value.to_ascii_lowercase().as_str() {
                        "right" => DockSide::Right,
                        "left" => DockSide::Left,
                        "top" => DockSide::Top,
                        "bottom" => DockSide::Bottom,
                        _ => settings.dock,
                    };
                }
                "magnifier_desktop_region" | "desktop_region" => {
                    if value.trim().is_empty() || value.eq_ignore_ascii_case("none") {
                        settings.desktop_region = None;
                        self.region_fields = std::array::from_fn(|_| String::new());
                    } else if let Ok(region) = serde_json::from_str::<Bounds>(value) {
                        settings.desktop_region = Some(region);
                        self.region_fields = [region.left, region.top, region.right, region.bottom]
                            .map(|coordinate| coordinate.to_string());
                    }
                }
                "magnifier_region_left" => self.region_fields[0] = value.clone(),
                "magnifier_region_top" => self.region_fields[1] = value.clone(),
                "magnifier_region_right" => self.region_fields[2] = value.clone(),
                "magnifier_region_bottom" => self.region_fields[3] = value.clone(),
                "magnifier_export_path" => self.export_path = value.clone(),
                _ => (),
            }
        }
        if let Some(region) = parse_region(&self.region_fields) {
            settings.desktop_region = Some(region);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        seats: &[Seat],
        show_user_picker: bool,
        monitors: &[Monitor],
        session_active: bool,
        session_ready: bool,
        routing_supported: bool,
        selected_target_available: bool,
        session_busy: bool,
        monitor_error: Option<&str>,
        script: Option<&mut Script>,
        action_frame: &mut u64,
        frame_actions: &mut BTreeSet<String>,
    ) -> Option<PageAction> {
        self.sync_profile(seats);
        let mut script = script;
        egui::ScrollArea::vertical()
            .id_salt("magnifier-workspace")
            .show(ui, |ui| {
                if show_user_picker {
                    Heading::new("Magnifier").heading().show(ui);
                    Text::new("Inspect a selected application or explicitly chosen desktop region with a virtual magnified view.")
                        .caption()
                        .wrap()
                        .show(ui);
                }
                if seats.is_empty() {
                    ui.weak("Add a user before configuring a magnifier.");
                    return None;
                }

                if show_user_picker {
                    ui.horizontal_wrapped(|ui| {
                        for (index, seat) in seats.iter().enumerate() {
                            let label = format!("Select magnifier user {}", index + 1);
                            if button(ui, &label, self.pending.is_none(), script.as_deref_mut(), action_frame, frame_actions) {
                                self.selected_user = index;
                                self.sync_region_fields();
                            }
                            if self.selected_user == index {
                                ui.label(seat.name.as_str());
                            }
                        }
                    });
                }

                let draft = self.drafts.get_mut(self.selected_user)?;
                if show_user_picker {
                    ui.label(format!("Selected user: {}", seats[self.selected_user].name));
                }
            let keyboard_claimed = egui::Popup::is_any_open(ui.ctx())
                || ui.ctx().any_popup_open()
                || ui.ctx().text_edit_focused();
            ui.horizontal(|ui| {
                ui.label("Mode");
                ui.push_id("magnifier-mode", |ui| {
                    let enabled = !session_busy && self.pending.is_none();
                    let current = match draft.mode {
                        Mode::Off => "Off",
                        Mode::Lens => "Lens",
                        Mode::Docked => "Docked",
                        Mode::Fullscreen => "Fullscreen",
                    };
                    let trigger = Button::new(current)
                        .variant(ButtonVariant::Outline)
                        .icon_right(ouroboros_ui::egui_phosphor::light::CARET_DOWN)
                        .enabled(enabled)
                        .id_source("mode-trigger")
                        .show(ui);
                    if let Some(script) = script.as_deref_mut() {
                        script.observe("Magnifier mode", &trigger);
                    }
                    let _ = egui::Popup::menu(&trigger).show(|ui| {
                        for (label, mode) in [
                            ("Off", Mode::Off),
                            ("Lens", Mode::Lens),
                            ("Docked", Mode::Docked),
                            ("Fullscreen", Mode::Fullscreen),
                        ] {
                            let response = MenuItem::new(label)
                                .checked(draft.mode == mode)
                                .enabled(enabled)
                                .id_source(("mode-option", label))
                                .show(ui);
                            let step_label = format!("Mode {label}");
                            let accepted = script
                                .as_deref_mut()
                                .is_none_or(|script| script.observe(&step_label, &response));
                            if response.clicked()
                                && accepted
                                && consume(action_frame, frame_actions, ui.ctx(), "Magnifier mode")
                            {
                                draft.mode = mode;
                                ui.close();
                            }
                        }
                    });
                });
            });

            ui.label("Capture source");
            ui.horizontal_wrapped(|ui| {
                for (label, source) in [
                    ("Source selected target", Source::SelectedTarget),
                    ("Source desktop pixels", Source::Desktop),
                ] {
                    let response = Button::new(label)
                        .variant(if draft.source == source { ButtonVariant::Secondary } else { ButtonVariant::Outline })
                        .enabled(!session_busy && self.pending.is_none())
                        .id_source(("magnifier", label))
                        .show(ui);
                    let accepted = script.as_deref_mut().is_none_or(|s| s.observe(label, &response));
                    if response.clicked() && accepted && consume(action_frame, frame_actions, ui.ctx(), label) {
                        draft.source = source;
                    }
                }
            });
            let mut region_input_error = None;
            if draft.source == Source::Desktop {
                ui.colored_label(
                    Color32::from_rgb(251, 191, 36),
                    "Desktop source captures screen pixels in the chosen rectangle, or the full virtual desktop when no rectangle is set. Choose only a region you are authorized to inspect.",
                );
                ui.label("Desktop region in physical pixels (blank uses the full virtual desktop)");
                for (i, label) in ["Left", "Top", "Right", "Bottom"].into_iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(label);
                        Input::new(&mut self.region_fields[i])
                            .id_source(("magnifier_region", label))
                            .show(ui);
                    });
                }
                if self.region_fields.iter().any(|v| !v.trim().is_empty()) {
                    match parse_region_checked(&self.region_fields) {
                        Ok(Some(region)) => draft.desktop_region = Some(region),
                        Ok(None) => draft.desktop_region = None,
                        Err(error) => {
                            region_input_error = Some(error.to_string());
                        }
                    }
                } else {
                    draft.desktop_region = None;
                }
                if let Some(region) = draft.desktop_region {
                    let desktop = mux_windows::targets::desktop_bounds();
                    if desktop.intersection(region) != Some(region) {
                        region_input_error = Some("Desktop capture rectangle must lie inside the physical virtual desktop.".into());
                    }
                }
            }

            if ui.available_width() >= 500.0 {
                ui.columns(2, |columns| {
                    zoom_control(&mut columns[0], draft);
                    width_control(&mut columns[1], draft);
                });
                ui.columns(2, |columns| {
                    height_control(&mut columns[0], draft);
                    refresh_control(&mut columns[1], draft);
                });
            } else {
                zoom_control(ui, draft);
                width_control(ui, draft);
                height_control(ui, draft);
                refresh_control(ui, draft);
            }

            ui.horizontal_wrapped(|ui| {
                let enabled = !session_busy && self.pending.is_none();
                let access_key = (!keyboard_claimed && enabled)
                    .then(|| magnifier_access_key(ui.ctx()))
                    .flatten();
                let border = Checkbox::new(&mut draft.border)
                    .label("Show lens border")
                    .enabled(enabled)
                    .id_source(("magnifier", "border"))
                    .show(ui);
                if let Some(script) = script.as_deref_mut() {
                    script.observe("Show lens border", &border);
                }
                if access_key == Some(MagnifierAccessKey::Border)
                    && enabled
                    && !border.changed()
                    && consume(action_frame, frame_actions, ui.ctx(), "Show lens border")
                {
                    draft.border = !draft.border;
                    if let Some(script) = script.as_deref_mut() {
                        script.magnifier_access_action("Show lens border");
                    }
                }

                let auto = Checkbox::new(&mut draft.auto_refresh)
                    .label("Refresh automatically")
                    .enabled(enabled)
                    .id_source(("magnifier", "auto-refresh"))
                    .show(ui);
                if let Some(script) = script.as_deref_mut() {
                    script.observe("Refresh automatically", &auto);
                }
                if access_key == Some(MagnifierAccessKey::AutoRefresh)
                    && enabled
                    && !auto.changed()
                    && consume(action_frame, frame_actions, ui.ctx(), "Refresh automatically")
                {
                    draft.auto_refresh = !draft.auto_refresh;
                    if let Some(script) = script.as_deref_mut() {
                        script.magnifier_access_action("Refresh automatically");
                    }
                }
            });
            let border_color = Collapsible::new("Border color (RGB)")
                .id_source(("magnifier", self.selected_user, "border-color"))
                .show(ui, |ui| {
                    for (channel, label) in draft.border_color.iter_mut().zip(["Red", "Green", "Blue"]) {
                        ui.label(format!("{label}: {channel}"));
                        let mut value = f32::from(*channel);
                        Slider::new(&mut value).range(0.0, 255.0).step(1.0).show(ui);
                        *channel = value.round().clamp(0.0, 255.0) as u8;
                    }
                });
            if let Some(script) = script.as_deref_mut() {
                script.observe("Border color (RGB)", &border_color);
            }
            let keyboard_help = Collapsible::new("Keyboard shortcuts")
                .id_source(("magnifier", self.selected_user, "keyboard-help"))
                .show(ui, |ui| {
                    ui.weak("Alt+B toggles border; Alt+U toggles auto update. Apply to activate.");
                });
            if let Some(script) = script.as_deref_mut() {
                script.observe("Keyboard shortcuts", &keyboard_help);
            }

            let stable_monitors: Vec<_> = monitors.iter().filter(|m| m.stable_id).collect();
            let mut selected_monitor = draft
                .monitor_id
                .as_ref()
                .and_then(|id| stable_monitors.iter().position(|m| m.id == *id))
                .map_or(0, |index| index + 1);
            let mut monitor_options = vec!["Automatic placement".to_string()];
            monitor_options.extend(stable_monitors.iter().map(|monitor| {
                format!("{}{}", if monitor.primary { "Primary · " } else { "" }, monitor.name)
            }));
            if Select::new(&mut selected_monitor)
                .options(monitor_options)
                .placeholder("Choose magnifier display")
                .show(ui)
                .changed()
            {
                draft.monitor_id = selected_monitor
                    .checked_sub(1)
                    .and_then(|index| stable_monitors.get(index))
                    .map(|monitor| monitor.id.clone());
            }
            if let Some(error) = monitor_error {
                ui.colored_label(Color32::LIGHT_RED, format!("Display inventory unavailable: {error}"));
            }
            if let Some(id) = draft.monitor_id.as_deref() {
                if !stable_monitors.iter().any(|monitor| monitor.id == id) {
                    ui.colored_label(Color32::LIGHT_RED, "The saved display is disconnected or has no stable identity.");
                }
                Checkbox::new(&mut draft.work_area).label("Place inside display work area").show(ui);
            }

            ui.label("Dock position");
            ui.horizontal_wrapped(|ui| {
                for (label, dock) in [
                    ("Dock right", DockSide::Right),
                    ("Dock left", DockSide::Left),
                    ("Dock top", DockSide::Top),
                    ("Dock bottom", DockSide::Bottom),
                ] {
                    let response = Button::new(label)
                        .variant(if draft.dock == dock { ButtonVariant::Secondary } else { ButtonVariant::Outline })
                        .enabled(!session_busy && self.pending.is_none())
                        .id_source(("magnifier", label))
                        .show(ui);
                    let accepted = script.as_deref_mut().is_none_or(|s| s.observe(label, &response));
                    if response.clicked() && accepted && consume(action_frame, frame_actions, ui.ctx(), label) {
                        draft.dock = dock;
                    }
                }
            });

            let validation = region_input_error
                .map_or_else(|| validate(draft, stable_monitors.iter().copied()), Err);
            if let Err(error) = &validation {
                ui.colored_label(Color32::LIGHT_RED, error);
            }
            let target_state = self.state.as_ref().and_then(|state| state.users.get(self.selected_user));
            if let Some(state) = target_state {
                let status = if state.in_flight && state.status == "Capturing" {
                    "Capturing".to_owned()
                } else if state.in_flight {
                    format!("Capturing · {}", state.status)
                } else {
                    state.status.clone()
                };
                ui.weak(match state.frame_age_ms {
                    Some(age) if state.frame_cursor.is_some() => {
                        format!("{status} · last frame {age} ms ago")
                    }
                    _ => status,
                });
            } else {
                ui.weak("Magnifier is not running.");
            }
            if let Some(error) = &self.error {
                ui.colored_label(Color32::LIGHT_RED, error);
            } else if let Some(error) = self.state.as_ref().and_then(|state| state.error.as_deref()) {
                ui.colored_label(Color32::LIGHT_RED, error);
            }
            if self.pending.is_some() {
                ui.weak("Waiting for the magnifier worker acknowledgement.");
            }

            Divider::horizontal().show(ui);
            let dirty = self.committed.get(self.selected_user) != Some(draft);
            if dirty {
                ui.weak("Changes staged. Apply to activate.");
            }
            if button(
                ui,
                "Apply magnifier settings",
                session_active && session_ready && (routing_supported || draft.mode == Mode::Off)
                    && !session_busy
                    && self.pending.is_none()
                    && dirty
                    && validation.is_ok(),
                script.as_deref_mut(),
                action_frame,
                frame_actions,
            ) {
                let settings = draft.clone();
                return Some(if session_active {
                    self.pending = Some(Pending::Configure { user: self.selected_user, settings: settings.clone() });
                    self.error = None;
                    PageAction::Worker(magnifier_runtime::Command::Configure { user: self.selected_user, settings })
                } else {
                    PageAction::SaveToProfile { user: self.selected_user, settings }
                });
            }
            if !session_active && dirty && validation.is_ok()
                && (routing_supported || draft.mode == Mode::Off)
                && button(ui, "Save magnifier settings to profile", true, script.as_deref_mut(), action_frame, frame_actions)
            {
                return Some(PageAction::SaveToProfile { user: self.selected_user, settings: draft.clone() });
            }

            let source_ready = draft.mode != Mode::Off
                && (draft.source == Source::Desktop || selected_target_available);
            let frame_ready = self
                .state
                .as_ref()
                .and_then(|state| state.users.get(self.selected_user))
                .is_some_and(|state| {
                    state.capture_sequence > 0
                        && state.status == "Ready"
                        && state.frame_cursor.is_some()
                });
            let mut command_action = None;
            ui.horizontal_wrapped(|ui| {
                if button(ui, "Refresh magnifier", session_ready && source_ready && !session_busy && self.pending.is_none(), script.as_deref_mut(), action_frame, frame_actions) {
                    self.pending = Some(Pending::Refresh);
                    self.error = None;
                    command_action = Some(magnifier_runtime::Command::Refresh { user: self.selected_user });
                }
            });
            ui.label("PNG export saves this user's current magnified frame.");
            Input::new(&mut self.export_path).id_source("magnifier_export_path").show(ui);
            if button(
                ui,
                "Save magnified PNG",
                session_ready && source_ready && frame_ready && !session_busy && self.pending.is_none() && self.export_path.to_ascii_lowercase().ends_with(".png"),
                script.as_deref_mut(),
                action_frame,
                frame_actions,
            ) {
                self.pending = Some(Pending::Export);
                self.error = None;
                command_action = Some(magnifier_runtime::Command::Export { user: self.selected_user, path: self.export_path.clone() });
            }
            if let Some(export) = self.state.as_ref().and_then(|state| state.export.as_ref()) {
                ui.label(format!("Exported {} × {} PNG to {}", export.width, export.height, export.path));
            }
            let details = Collapsible::new("Capture details")
                .id_source(("magnifier", self.selected_user, "capture-details"))
                .show(ui, |ui| {
                    if let Some(id) = draft.monitor_id.as_deref()
                        && let Some(monitor) = stable_monitors.iter().find(|monitor| monitor.id == id)
                    {
                        ui.label(format!("Stable display ID: {}", monitor.id));
                        ui.label(format!("Display bounds: {:?}; work area: {:?}", monitor.bounds, monitor.work_area));
                    }
                    if let Some(state) = target_state {
                        ui.label(format!("Status: {}", state.status));
                        ui.label(format!("Capture sequence: {}", state.capture_sequence));
                        ui.label(if state.in_flight { "Frame capture is in progress." } else { "No frame capture is in progress." });
                        if let Some(age) = state.frame_age_ms {
                            ui.label(format!("Frame age: {age} ms"));
                        }
                        if let Some(bounds) = state.source_bounds {
                            ui.label(format!("Source bounds: {bounds:?}"));
                        }
                        if let Some(pid) = state.source_pid {
                            ui.label(format!("Source PID: {pid}"));
                        }
                        if let Some(hwnd) = state.source_hwnd {
                            ui.label(format!("Source window: {hwnd:#x}"));
                        }
                        if let Some((x, y)) = state.cursor {
                            ui.label(format!("Virtual cursor: ({x}, {y})"));
                        }
                        if let Some(overlay) = &state.overlay {
                            ui.label(if overlay.visible {
                                "Magnifier layer is visible."
                            } else {
                                "Magnifier layer is hidden."
                            });
                            ui.label(format!(
                                "Click-through: {}; capture exclusion: {}",
                                overlay.click_through,
                                if overlay.excluded_from_capture {
                                    "confirmed"
                                } else {
                                    "unavailable"
                                }
                            ));
                        } else {
                            ui.label("Magnifier layer is not created.");
                        }
                    } else {
                        ui.weak("No running magnifier state.");
                    }
                });
            if let Some(script) = script {
                script.observe("Capture details", &details);
            }
            command_action.map(PageAction::Worker)
        })
        .inner
    }
}

fn zoom_control(ui: &mut egui::Ui, settings: &mut Settings) {
    ui.label(format!("Zoom: {:.2}×", settings.zoom));
    let mut zoom = settings.zoom as f32;
    Slider::new(&mut zoom).range(1.0, 32.0).step(0.05).show(ui);
    settings.zoom = f64::from(zoom.clamp(1.0, 32.0));
}

fn width_control(ui: &mut egui::Ui, settings: &mut Settings) {
    ui.label(format!("Lens width: {} px", settings.width));
    let mut width = settings.width as f32;
    Slider::new(&mut width)
        .range(32.0, 4096.0)
        .step(1.0)
        .show(ui);
    settings.width = width.round().clamp(32.0, 4096.0) as u32;
}

fn height_control(ui: &mut egui::Ui, settings: &mut Settings) {
    ui.label(format!("Lens height: {} px", settings.height));
    let mut height = settings.height as f32;
    Slider::new(&mut height)
        .range(32.0, 4096.0)
        .step(1.0)
        .show(ui);
    settings.height = height.round().clamp(32.0, 4096.0) as u32;
}

fn refresh_control(ui: &mut egui::Ui, settings: &mut Settings) {
    ui.label(format!("Refresh rate: {} Hz (1-30)", settings.refresh_hz));
    let mut refresh = f32::from(settings.refresh_hz);
    Slider::new(&mut refresh)
        .range(1.0, 30.0)
        .step(1.0)
        .show(ui);
    settings.refresh_hz = refresh.round().clamp(1.0, 30.0) as u8;
}

fn validate<'a>(
    settings: &Settings,
    monitors: impl Iterator<Item = &'a Monitor>,
) -> Result<(), String> {
    let monitors: Vec<_> = monitors.collect();
    settings.validate().map_err(|error| format!("{error:#}"))?;
    if settings.mode != Mode::Off {
        let area = if let Some(id) = &settings.monitor_id {
            let monitor = monitors
                .iter()
                .copied()
                .find(|monitor| monitor.stable_id && monitor.id == *id)
                .ok_or_else(|| {
                    "Choose a connected display with a stable interface identity.".to_string()
                })?;
            if settings.work_area {
                monitor.work_area
            } else {
                monitor.bounds
            }
        } else {
            mux_windows::targets::desktop_bounds()
        };
        mux_core::magnifier::placement(settings, area, (0, 0))
            .map_err(|error| format!("{error:#}"))?;
    }
    if settings.source == Source::Desktop
        && let Some(region) = settings.desktop_region
    {
        if !region.valid_region() {
            return Err("Desktop capture rectangle must have positive width and height and use physical desktop coordinates.".into());
        }
        if mux_windows::targets::desktop_bounds().intersection(region) != Some(region) {
            return Err(
                "Desktop capture rectangle must lie inside the physical virtual desktop.".into(),
            );
        }
    }
    if let Some(id) = &settings.monitor_id
        && !monitors
            .iter()
            .copied()
            .any(|monitor| monitor.stable_id && monitor.id == *id)
    {
        return Err("Choose a connected display with a stable interface identity.".into());
    }
    Ok(())
}

fn parse_region(fields: &[String; 4]) -> Option<Bounds> {
    parse_region_checked(fields).ok().flatten()
}

fn parse_region_checked(fields: &[String; 4]) -> Result<Option<Bounds>, &'static str> {
    if fields.iter().all(|value| value.trim().is_empty()) {
        return Ok(None);
    }
    let mut values = [0_i32; 4];
    for (destination, value) in values.iter_mut().zip(fields) {
        *destination = value
            .trim()
            .parse()
            .map_err(|_| "Enter integer physical-pixel coordinates for all four region edges.")?;
    }
    let region = Bounds {
        left: values[0],
        top: values[1],
        right: values[2],
        bottom: values[3],
    };
    if !region.valid_region() {
        return Err(
            "Desktop region must have positive width and height within the supported coordinate range.",
        );
    }
    Ok(Some(region))
}

fn set_channel(channel: &mut u8, text: &str) {
    if let Ok(value) = text.parse::<u16>() {
        *channel = value.min(255) as u8;
    }
}
fn set_f64(slot: &mut f64, text: &str, min: f64, max: f64) {
    if let Ok(value) = text.parse::<f64>()
        && value.is_finite()
    {
        *slot = value.clamp(min, max);
    }
}
fn set_u32(slot: &mut u32, text: &str, min: u32, max: u32) {
    if let Ok(value) = text.parse::<u32>() {
        *slot = value.clamp(min, max);
    }
}
fn set_u8(slot: &mut u8, text: &str, min: u8, max: u8) {
    if let Ok(value) = text.parse::<u8>() {
        *slot = value.clamp(min, max);
    }
}
fn parse_bool(text: &str, old: bool) -> bool {
    text.parse().unwrap_or(old)
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
        .enabled(enabled)
        .id_source(("magnifier", label))
        .show(ui);
    let accepted = script.is_none_or(|script| script.observe(label, &response));
    response.clicked() && accepted && consume(action_frame, frame_actions, ui.ctx(), label)
}
fn consume(
    frame: &mut u64,
    actions: &mut BTreeSet<String>,
    ctx: &egui::Context,
    label: &str,
) -> bool {
    let current = ctx.cumulative_frame_nr();
    if *frame != current {
        *frame = current;
        actions.clear();
    }
    actions.insert(label.to_owned())
}

#[cfg(test)]
mod focus_user_tests {
    use super::*;

    fn seats() -> Vec<Seat> {
        let mut first = Seat {
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
        };
        let mut second = Seat {
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
        };
        first.magnifier.desktop_region = Some(Bounds {
            left: 1,
            top: 2,
            right: 3,
            bottom: 4,
        });
        second.magnifier.desktop_region = Some(Bounds {
            left: 10,
            top: 20,
            right: 30,
            bottom: 40,
        });
        vec![first, second]
    }

    #[test]
    fn focus_keeps_pending_recipient_and_refreshes_region_for_new_user() {
        let seats = seats();
        let mut page = MagnifierPage::default();
        assert!(page.focus_user(&seats, 0));
        page.drafts[0].zoom = 3.0;
        page.pending = Some(Pending::Configure {
            user: 0,
            settings: page.drafts[0].clone(),
        });

        assert!(!page.focus_user(&seats, 1));
        assert!(!page.focus_user(&seats, seats.len()));
        assert_eq!(page.selected_user(), 0);
        assert_eq!(
            page.pending.as_ref().map(|pending| match pending {
                Pending::Configure { user, .. } => *user,
                Pending::Refresh => usize::MAX,
                Pending::Export => usize::MAX - 1,
            }),
            Some(0)
        );

        page.pending = None;
        assert!(page.focus_user(&seats, 1));
        assert_eq!(page.region_fields, ["10", "20", "30", "40"]);
        assert_eq!(page.drafts[0].zoom, 3.0);
    }

    #[test]
    fn external_agent_state_does_not_consume_pending_gui_settings() {
        let mut seats = seats();
        let mut page = MagnifierPage::default();
        page.sync_profile(&seats);
        page.drafts[0].zoom = 3.0;
        page.region_fields[0] = "99".into();
        page.pending = Some(Pending::Configure {
            user: 0,
            settings: page.drafts[0].clone(),
        });

        let external_settings = Settings {
            zoom: 4.0,
            desktop_region: Some(Bounds {
                left: 5,
                top: 6,
                right: 7,
                bottom: 8,
            }),
            ..Default::default()
        };
        let external = magnifier_runtime::State {
            acknowledged: true,
            users: vec![magnifier_runtime::UserState {
                user: 0,
                settings: external_settings.clone(),
                status: "Waiting for source".into(),
                source_bounds: None,
                source_hwnd: None,
                source_pid: None,
                cursor: None,
                frame_cursor: None,
                capture_sequence: 0,
                frame_age_ms: None,
                in_flight: false,
                overlay: None,
            }],
            export: None,
            error: None,
        };
        page.receive_external(external.clone());
        assert!(page.pending.is_some());
        assert!(!page.state.as_ref().unwrap().acknowledged);

        seats[0].magnifier = external_settings;
        page.sync_profile(&seats);
        assert_eq!(page.committed[0].zoom, 4.0);
        assert_eq!(page.drafts[0].zoom, 3.0);
        assert_eq!(page.region_fields[0], "99");

        let (user, accepted) = page.receive(external).unwrap();
        assert_eq!(user, 0);
        assert_eq!(accepted.zoom, 3.0);
        assert_eq!(page.region_fields, ["1", "2", "3", "4"]);
        assert!(page.focus_user(&seats, 1));
        assert_eq!(page.region_fields, ["10", "20", "30", "40"]);
    }
}

#[cfg(test)]
mod access_key_tests {
    use super::*;

    fn context() -> egui::Context {
        let context = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        ouroboros_ui::theme::typography::register(&mut fonts);
        context.set_fonts(fonts);
        ouroboros_ui::Theme::install(&context, ouroboros_ui::Mode::Dark);
        context
    }

    struct Fixture {
        ctx: egui::Context,
        page: MagnifierPage,
        seats: Vec<Seat>,
        action_frame: u64,
        frame_actions: BTreeSet<String>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                ctx: context(),
                page: MagnifierPage::default(),
                seats: serde_json::from_value(serde_json::json!([
                    {"id":"a","name":"A","mouse":"virtual://mouse/1","keyboard":"virtual://keyboard/1","target_pid":0},
                    {"id":"b","name":"B","mouse":"virtual://mouse/2","keyboard":"virtual://keyboard/2","target_pid":0}
                ])).unwrap(),
                action_frame: 0,
                frame_actions: BTreeSet::new(),
            }
        }

        fn frame(
            &mut self,
            key: Option<(egui::Key, egui::Modifiers, bool)>,
            focused: bool,
            busy: bool,
            active: bool,
            width: f32,
        ) -> Option<PageAction> {
            let mut input = egui::RawInput {
                focused,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 1000.0),
                )),
                ..Default::default()
            };
            if let Some((key, modifiers, repeat)) = key {
                input.modifiers = modifiers;
                for pressed in [true, false] {
                    input.events.push(egui::Event::Key {
                        key,
                        physical_key: Some(key),
                        pressed,
                        repeat: repeat && pressed,
                        modifiers,
                    });
                }
            }
            let mut action = None;
            let _ = self.ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    action = self.page.show(
                        ui,
                        &self.seats,
                        false,
                        &[],
                        active,
                        active,
                        true,
                        true,
                        busy,
                        None,
                        None,
                        &mut self.action_frame,
                        &mut self.frame_actions,
                    );
                });
            });
            action
        }

        fn alt(&mut self, key: egui::Key, width: f32) -> Option<PageAction> {
            self.frame(
                Some((key, egui::Modifiers::ALT, false)),
                true,
                false,
                true,
                width,
            )
        }

        fn held_b(&mut self, pressed: bool, repeat: bool, busy: bool) {
            let modifiers = egui::Modifiers::ALT;
            let mut input = egui::RawInput {
                focused: true,
                modifiers,
                ..Default::default()
            };
            input.events.push(egui::Event::Key {
                key: egui::Key::B,
                physical_key: Some(egui::Key::B),
                pressed,
                repeat,
                modifiers,
            });
            let _ = self.ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    let _ = self.page.show(
                        ui,
                        &self.seats,
                        false,
                        &[],
                        true,
                        true,
                        true,
                        true,
                        busy,
                        None,
                        None,
                        &mut self.action_frame,
                        &mut self.frame_actions,
                    );
                });
            });
        }
    }

    #[test]
    fn alt_border_and_update_stage_only_selected_user_at_normal_and_narrow_width() {
        let mut fixture = Fixture::new();
        fixture.page.sync_profile(&fixture.seats);
        let committed = fixture.page.committed.clone();
        assert!(fixture.alt(egui::Key::B, 840.0).is_none());
        assert_eq!(fixture.page.drafts[0].border, !committed[0].border);
        assert_eq!(fixture.page.drafts[1], committed[1]);
        assert!(fixture.page.pending.is_none());
        assert!(fixture.page.focus_user(&fixture.seats, 1));
        assert!(fixture.alt(egui::Key::U, 360.0).is_none());
        assert_eq!(
            fixture.page.drafts[1].auto_refresh,
            !committed[1].auto_refresh
        );
        assert_eq!(fixture.page.drafts[1].border, committed[1].border);
        assert_eq!(fixture.page.drafts[0].border, !committed[0].border);
        assert_eq!(fixture.page.committed, committed);
        assert!(fixture.page.pending.is_none());

        // Stopped sessions still permit local profile drafts and the existing Save path.
        assert!(
            fixture
                .frame(
                    Some((egui::Key::B, egui::Modifiers::ALT, false)),
                    true,
                    false,
                    false,
                    360.0,
                )
                .is_none()
        );
        assert_eq!(fixture.page.drafts[1].border, !committed[1].border);
    }

    #[test]
    fn exact_alt_requires_focus_and_ignores_busy_pending_text_popup_and_real_repeat() {
        let mut fixture = Fixture::new();
        fixture.page.sync_profile(&fixture.seats);
        let original = fixture.page.drafts[0].clone();
        for modifiers in [
            egui::Modifiers::ALT | egui::Modifiers::CTRL,
            egui::Modifiers::ALT | egui::Modifiers::COMMAND,
            egui::Modifiers::ALT | egui::Modifiers::MAC_CMD,
            egui::Modifiers::ALT | egui::Modifiers::SHIFT,
        ] {
            assert!(
                fixture
                    .frame(
                        Some((egui::Key::B, modifiers, false)),
                        true,
                        false,
                        true,
                        840.0
                    )
                    .is_none()
            );
        }
        assert!(
            fixture
                .frame(
                    Some((egui::Key::U, egui::Modifiers::ALT, false)),
                    false,
                    false,
                    true,
                    840.0
                )
                .is_none()
        );
        assert!(
            fixture
                .frame(
                    Some((egui::Key::B, egui::Modifiers::ALT, false)),
                    true,
                    true,
                    true,
                    840.0
                )
                .is_none()
        );
        fixture.page.pending = Some(Pending::Refresh);
        assert!(fixture.alt(egui::Key::U, 840.0).is_none());
        fixture.page.pending = None;
        assert_eq!(fixture.page.drafts[0], original);

        fixture.held_b(true, false, true);
        fixture.held_b(true, true, false);
        fixture.held_b(false, false, false);
        assert_eq!(fixture.page.drafts[0], original);

        let mut text = String::new();
        let _ = fixture.ctx.run_ui(
            egui::RawInput {
                focused: true,
                ..Default::default()
            },
            |ui| {
                let response = ui.text_edit_singleline(&mut text);
                response.request_focus();
            },
        );
        assert!(fixture.ctx.text_edit_focused());
        assert!(fixture.alt(egui::Key::B, 840.0).is_none());
        assert_eq!(fixture.page.drafts[0], original);

        fixture.seats.clear();
        assert!(fixture.alt(egui::Key::B, 840.0).is_none());
        assert!(fixture.page.drafts.is_empty());

        let ctx = context();
        let mut popup_input = egui::RawInput {
            focused: true,
            modifiers: egui::Modifiers::ALT,
            ..Default::default()
        };
        popup_input.events.push(egui::Event::Key {
            key: egui::Key::U,
            physical_key: Some(egui::Key::U),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::ALT,
        });
        let _ = ctx.run_ui(popup_input, |ui| {
            egui::Popup::open_id(ui.ctx(), egui::Id::new("unrelated-popup"));
            assert_eq!(magnifier_access_key(ui.ctx()), None);
        });
    }

    fn script_fixture(steps: serde_json::Value) -> (std::path::PathBuf, Script) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let directory = std::env::temp_dir().join(format!(
            "protoinputmultiplex-magnifier-control-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("ui.json");
        std::fs::write(&path, steps.to_string()).unwrap();
        (directory, Script::load(path).unwrap())
    }

    fn scripted_frame(fixture: &mut Fixture, script: &mut Script, input: egui::RawInput) {
        let _ = fixture.ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show_inside(ui, |ui| {
                let action = fixture.page.show(
                    ui,
                    &fixture.seats,
                    false,
                    &[],
                    true,
                    true,
                    true,
                    true,
                    false,
                    None,
                    Some(script),
                    &mut fixture.action_frame,
                    &mut fixture.frame_actions,
                );
                assert!(action.is_none());
            });
        });
        script.wait_for_io();
    }

    fn screen_input() -> egui::RawInput {
        egui::RawInput {
            focused: true,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(840.0, 1000.0),
            )),
            ..Default::default()
        }
    }

    fn observed_center(script: &mut Script, directory: &std::path::Path) -> egui::Pos2 {
        script.ready(true);
        script.wait_for_io();
        let progress: serde_json::Value =
            serde_json::from_slice(&std::fs::read(directory.join("ui.progress.json")).unwrap())
                .unwrap();
        let rect = progress["response"]["rect"].as_array().unwrap();
        egui::pos2(
            ((rect[0].as_f64().unwrap() + rect[2].as_f64().unwrap()) / 2.0) as f32,
            ((rect[1].as_f64().unwrap() + rect[3].as_f64().unwrap()) / 2.0) as f32,
        )
    }

    fn pointer_event(point: egui::Pos2, pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers,
        }
    }

    fn alt_b_pair(input: &mut egui::RawInput) {
        input.modifiers = egui::Modifiers::ALT;
        for pressed in [true, false] {
            input.events.push(egui::Event::Key {
                key: egui::Key::B,
                physical_key: Some(egui::Key::B),
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::ALT,
            });
        }
    }

    #[test]
    fn compact_mode_opens_with_enter_and_stages_actual_menu_choice() {
        let (directory, mut script) = script_fixture(serde_json::json!([
            {"button":"Magnifier mode","key":"Tab","wait_ms":0},
            {"button":"Magnifier mode","key":"Enter","wait_ms":0},
            {"button":"Mode Lens","wait_ms":0}
        ]));
        let mut fixture = Fixture::new();
        fixture.page.sync_profile(&fixture.seats);
        let committed = fixture.page.committed.clone();
        let mut keys = Vec::new();
        for _ in 0..120 {
            let mut input = screen_input();
            script.input(&mut input);
            keys.extend(input.events.iter().filter_map(|event| match event {
                egui::Event::Key { key, pressed, .. } => Some((*key, *pressed)),
                _ => None,
            }));
            scripted_frame(&mut fixture, &mut script, input);
            if fixture.page.drafts[0].mode == Mode::Lens {
                break;
            }
        }
        assert_eq!(fixture.page.drafts[0].mode, Mode::Lens);
        assert_eq!(fixture.page.committed, committed);
        assert_eq!(fixture.page.drafts[1], committed[1]);
        assert!(fixture.page.pending.is_none());
        assert_eq!(
            keys,
            [
                (egui::Key::Tab, true),
                (egui::Key::Tab, false),
                (egui::Key::Enter, true),
                (egui::Key::Enter, false)
            ]
        );
        let events = std::fs::read_to_string(directory.join("ui.events.jsonl")).unwrap();
        let rows: Vec<serde_json::Value> = events
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["button"], "Magnifier mode");
        assert_eq!(rows[0]["focused"], true);
        assert_eq!(rows[1]["button"], "Magnifier mode");
        assert_eq!(rows[1]["clicked"], true);
        assert_eq!(rows[2]["button"], "Mode Lens");
        assert_eq!(rows[2]["clicked"], true);
        drop(script);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn same_frame_pointer_click_and_alt_toggle_checkbox_only_once() {
        let (directory, mut script) = script_fixture(serde_json::json!([
            {"button":"Show lens border","key":"AltB","wait_ms":0}
        ]));
        let mut fixture = Fixture::new();
        fixture.page.sync_profile(&fixture.seats);
        let original = fixture.page.drafts[0].border;
        scripted_frame(&mut fixture, &mut script, screen_input());
        let point = observed_center(&mut script, &directory);
        let mut down = screen_input();
        down.events.push(egui::Event::PointerMoved(point));
        down.events
            .push(pointer_event(point, true, egui::Modifiers::NONE));
        scripted_frame(&mut fixture, &mut script, down);
        let mut release = screen_input();
        script.input(&mut release);
        assert!(release.events.iter().any(|event| matches!(
            event,
            egui::Event::Key {
                key: egui::Key::B,
                pressed: true,
                modifiers: egui::Modifiers::ALT,
                ..
            }
        )));
        release.events.push(egui::Event::PointerMoved(point));
        release
            .events
            .push(pointer_event(point, false, egui::Modifiers::ALT));
        scripted_frame(&mut fixture, &mut script, release);
        assert_eq!(fixture.page.drafts[0].border, !original);
        assert_eq!(
            fixture.page.drafts[1].border,
            fixture.page.committed[1].border
        );
        assert!(
            std::fs::read(directory.join("ui.events.jsonl"))
                .unwrap()
                .is_empty(),
            "the Alt action must not report success for the pointer's checkbox change"
        );
        drop(script);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn mode_popup_opened_in_alt_frame_claims_keyboard_before_checkbox() {
        let (directory, mut script) = script_fixture(serde_json::json!([
            {"button":"Magnifier mode","wait_ms":100000}
        ]));
        let mut fixture = Fixture::new();
        fixture.page.sync_profile(&fixture.seats);
        let original = fixture.page.drafts[0].border;
        scripted_frame(&mut fixture, &mut script, screen_input());
        let point = observed_center(&mut script, &directory);
        let mut down = screen_input();
        down.events.push(egui::Event::PointerMoved(point));
        down.events
            .push(pointer_event(point, true, egui::Modifiers::NONE));
        scripted_frame(&mut fixture, &mut script, down);
        let mut release = screen_input();
        alt_b_pair(&mut release);
        release.events.push(egui::Event::PointerMoved(point));
        release
            .events
            .push(pointer_event(point, false, egui::Modifiers::ALT));
        scripted_frame(&mut fixture, &mut script, release);
        assert!(egui::Popup::is_any_open(&fixture.ctx));
        assert_eq!(fixture.page.drafts[0].border, original);
        drop(script);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
