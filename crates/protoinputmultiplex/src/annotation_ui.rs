//! Ouroboros annotation controls, kept separate while the page is integrated.
use crate::{annotation_runtime, ui_script::Script};
use eframe::egui::{self, Color32};
use mux_core::{
    Seat,
    annotation::{Activator, Settings, Tool},
    desktop::Bounds,
};
use ouroboros_ui::atoms::{Button, ButtonVariant, Checkbox, Divider, Heading, Input, Slider, Text};
use ouroboros_ui::{cells::MenuItem, molecules::Collapsible};
use std::collections::{BTreeMap, BTreeSet};

/// Action returned to the dashboard. Profile changes are committed locally;
/// worker commands become committed only after an acknowledged success event.
pub enum PageAction {
    SaveToProfile { user: usize, settings: Settings },
    Worker(annotation_runtime::Command),
}

#[derive(Clone)]
enum Pending {
    Configure { user: usize, settings: Settings },
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrawAccessKey {
    Draw,
    Eraser,
    Clear,
    Color,
    Activator,
    ClearShortcut,
    Save,
}

impl DrawAccessKey {
    fn focus_only(self) -> bool {
        matches!(self, Self::Color | Self::Activator | Self::ClearShortcut)
    }
}

fn draw_access_key(ctx: &egui::Context) -> Option<DrawAccessKey> {
    if egui::Popup::is_any_open(ctx) || ctx.any_popup_open() {
        return None;
    }
    let editing_text = ctx.text_edit_focused();
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
            if *modifiers
                != (egui::Modifiers {
                    alt: true,
                    ..egui::Modifiers::NONE
                })
            {
                return None;
            }
            let shortcut = match key {
                egui::Key::D => DrawAccessKey::Draw,
                egui::Key::E => DrawAccessKey::Eraser,
                egui::Key::C => DrawAccessKey::Clear,
                egui::Key::O => DrawAccessKey::Color,
                egui::Key::B => DrawAccessKey::Activator,
                egui::Key::S => DrawAccessKey::ClearShortcut,
                egui::Key::V => DrawAccessKey::Save,
                _ => return None,
            };
            (!editing_text || shortcut.focus_only()).then_some(shortcut)
        })
    })
}

/// Staged annotation page state. `committed` comes from the profile/runtime;
/// edits stay in `drafts` until the dashboard reports a successful commit.
#[derive(Default)]
pub struct AnnotationPage {
    selected_user: usize,
    committed: Vec<Settings>,
    drafts: Vec<Settings>,
    pending: Option<Pending>,
    state: Option<annotation_runtime::State>,
    error: Option<String>,
    export_path: String,
    export_kind: usize,
    export_ink_scope: annotation_runtime::InkScope,
    export_region_fields: [String; 4],
    clear_key_text: String,
    color_hex_text: String,
    color_popup_open: bool,
}

impl AnnotationPage {
    pub fn has_profile_draft(&self) -> bool {
        self.pending.is_some()
            || self.drafts != self.committed
            || self.drafts.get(self.selected_user).is_some_and(|draft| {
                self.clear_key_text != draft.clear_key.map_or_else(String::new, display_clear_key)
                    || self.color_hex_text != display_rgb_hex(draft.color)
            })
    }
    /// Synchronize committed profile values without discarding unsubmitted edits.
    pub fn sync_profile(&mut self, seats: &[Seat]) {
        let next: Vec<_> = seats.iter().map(|seat| seat.annotation.clone()).collect();
        if next.len() != self.committed.len() {
            self.drafts = next.clone();
            self.committed = next;
            self.selected_user = self.selected_user.min(self.drafts.len().saturating_sub(1));
            self.clear_key_text = self
                .drafts
                .get(self.selected_user)
                .and_then(|settings| settings.clear_key)
                .map_or_else(String::new, display_clear_key);
            self.color_hex_text = self
                .drafts
                .get(self.selected_user)
                .map_or_else(String::new, |settings| display_rgb_hex(settings.color));
            self.color_popup_open = false;
        } else {
            for (index, settings) in next.iter().enumerate() {
                if self.committed.get(index) != Some(settings) {
                    self.committed[index] = settings.clone();
                    self.drafts[index] = settings.clone();
                    if index == self.selected_user {
                        self.clear_key_text = settings
                            .clear_key
                            .map_or_else(String::new, display_clear_key);
                        self.color_hex_text = display_rgb_hex(settings.color);
                        self.color_popup_open = false;
                    }
                }
            }
        }
    }

    /// Consume the worker event. Non-acknowledged telemetry updates counts only;
    /// it can never commit draft settings or erase a previous command error.
    pub fn receive(&mut self, state: annotation_runtime::State) -> Option<(usize, Settings)> {
        let acknowledged = state.acknowledged;
        let error = state.error.clone();
        let mut state = state;
        if state.export.is_none() {
            state.export = self
                .state
                .as_ref()
                .and_then(|previous| previous.export.clone());
        }
        self.state = Some(state.clone());
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
        if user == self.selected_user {
            self.clear_key_text = settings
                .clear_key
                .map_or_else(String::new, display_clear_key);
            self.color_hex_text = display_rgb_hex(settings.color);
            self.color_popup_open = false;
        }
        Some((user, settings))
    }

    /// A connected agent's ACK is telemetry here, never the pending GUI command's ACK.
    pub fn receive_external(&mut self, mut state: annotation_runtime::State) {
        state.acknowledged = false;
        let _ = self.receive(state);
    }

    pub fn session_stopped(&mut self) {
        self.pending = None;
        self.state = None;
        self.color_popup_open = false;
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
            self.clear_key_text = self.drafts[user]
                .clear_key
                .map_or_else(String::new, display_clear_key);
            self.color_hex_text = display_rgb_hex(self.drafts[user].color);
            self.color_popup_open = false;
        }
        true
    }

    /// Apply acceptance-script field edits to the staged controls, then allow
    /// the ordinary Ouroboros button interaction to issue the real command.
    pub fn apply_script_fields(&mut self, fields: &BTreeMap<String, String>) {
        if let Some(value) = fields.get("annotation_user")
            && let Ok(user) = value.parse::<usize>()
            && let Some(settings) = self.drafts.get(user)
        {
            self.selected_user = user;
            self.clear_key_text = settings
                .clear_key
                .map_or_else(String::new, display_clear_key);
            self.color_hex_text = display_rgb_hex(settings.color);
            self.color_popup_open = false;
        }
        let Some(settings) = self.drafts.get_mut(self.selected_user) else {
            return;
        };
        let mut color_edited = false;
        for (name, value) in fields {
            match name.as_str() {
                "annotation_user" => (),
                "annotation_tool" => {
                    settings.tool = match value.to_ascii_lowercase().as_str() {
                        "off" => Tool::Off,
                        "brush" => Tool::Brush,
                        "eraser" => Tool::Eraser,
                        _ => settings.tool,
                    };
                }
                "annotation_activator" => {
                    settings.activator = match value.to_ascii_lowercase().as_str() {
                        "primary" => Activator::Primary,
                        "secondary" => Activator::Secondary,
                        "middle" => Activator::Middle,
                        "back" | "x1" => Activator::X1,
                        "forward" | "x2" => Activator::X2,
                        _ => settings.activator,
                    };
                }
                "annotation_width" => {
                    if let Ok(width) = value.parse::<u16>() {
                        settings.width = width.clamp(1, 128);
                    }
                }
                "annotation_red" => {
                    set_channel(&mut settings.color[0], value);
                    color_edited = true;
                }
                "annotation_green" => {
                    set_channel(&mut settings.color[1], value);
                    color_edited = true;
                }
                "annotation_blue" => {
                    set_channel(&mut settings.color[2], value);
                    color_edited = true;
                }
                "annotation_alpha" => set_channel(&mut settings.alpha, value),
                "annotation_clear_key" => {
                    let value = value.trim();
                    if value.is_empty() || value.eq_ignore_ascii_case("none") {
                        settings.clear_key = None;
                        self.clear_key_text.clear();
                    } else {
                        self.clear_key_text = value.to_string();
                        if let Ok(key) = parse_clear_key(value) {
                            settings.clear_key = Some(key);
                        }
                    }
                }
                "annotation_clear_all" => {
                    settings.clear_all =
                        matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes");
                }
                "annotation_export_path" => self.export_path = value.clone(),
                "annotation_export_kind" => match value.to_ascii_lowercase().as_str() {
                    "ink_only" | "transparent" => self.export_kind = 0,
                    "selected_target" | "composition" => self.export_kind = 1,
                    "full_desktop" => self.export_kind = 2,
                    "desktop_region" => self.export_kind = 3,
                    _ => self.export_kind = 0,
                },
                "annotation_export_region_left" => self.export_region_fields[0] = value.clone(),
                "annotation_export_region_top" => self.export_region_fields[1] = value.clone(),
                "annotation_export_region_right" => self.export_region_fields[2] = value.clone(),
                "annotation_export_region_bottom" => self.export_region_fields[3] = value.clone(),
                "annotation_export_ink_scope" => {
                    self.export_ink_scope = match value.to_ascii_lowercase().as_str() {
                        "selected_user" | "selected" => annotation_runtime::InkScope::SelectedUser,
                        _ => annotation_runtime::InkScope::AllUsers,
                    };
                }
                _ => (),
            }
        }
        if color_edited {
            self.color_hex_text = display_rgb_hex(settings.color);
        }
    }

    /// Render with real Ouroboros widgets. Pass dashboard fields to preserve
    /// the existing once-per-frame script click guard used by other pages.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        seats: &[Seat],
        show_user_picker: bool,
        session_active: bool,
        session_ready: bool,
        routing_supported: bool,
        selected_target_available: bool,
        session_busy: bool,
        script: Option<&mut Script>,
        action_frame: &mut u64,
        frame_actions: &mut BTreeSet<String>,
    ) -> Option<PageAction> {
        self.sync_profile(seats);
        let access_key = if self.color_popup_open {
            None
        } else {
            draw_access_key(ui.ctx())
        };
        let mut script = script;
        egui::ScrollArea::vertical()
            .id_salt("annotation-workspace")
            .show(ui, |ui| {
                if show_user_picker {
                    Heading::new("Annotation").heading().show(ui);
                    Text::new("Draw and erase a session-local layer with each user's virtual mouse.")
                        .caption()
                        .show(ui);
                }
                Collapsible::new("Keyboard shortcuts")
                    .id_source("annotation-keyboard-shortcuts")
                    .default_open(false)
                    .show(ui, |ui| {
                        Text::new("Alt+D draw; Alt+E eraser; Alt+C clear using the applied scope; Alt+V save PNG. Alt+B focuses the activator, Alt+O opens the color editor, and Alt+S focuses the clear shortcut. Settings need Apply.")
                            .caption()
                            .wrap()
                            .show(ui);
                    });
                if seats.is_empty() {
                    ui.weak("Add a user before configuring annotation.");
                    return None;
                }

                if show_user_picker {
                    ui.horizontal_wrapped(|ui| {
                        for (index, seat) in seats.iter().enumerate() {
                            let label = format!("Select annotation user {}", index + 1);
                            if button(
                                ui,
                                &label,
                                self.pending.is_none(),
                                script.as_deref_mut(),
                                action_frame,
                                frame_actions,
                            ) {
                                self.selected_user = index;
                                self.clear_key_text = self.drafts[index]
                                    .clear_key
                                    .map_or_else(String::new, display_clear_key);
                                self.color_hex_text = display_rgb_hex(self.drafts[index].color);
                                self.color_popup_open = false;
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
            let draw_target = match draft.tool {
                Tool::Off => Tool::Brush,
                _ => Tool::Off,
            };
            ui.horizontal_wrapped(|ui| {
                for (label, tool) in [
                    ("Annotation Off", Tool::Off),
                    ("Annotation Brush", Tool::Brush),
                    ("Annotation Eraser", Tool::Eraser),
                ] {
                    let shortcut = !session_busy
                        && match access_key {
                            Some(DrawAccessKey::Draw) => tool == draw_target,
                            Some(DrawAccessKey::Eraser) => tool == Tool::Eraser,
                            _ => false,
                        };
                    let clicked = button(
                        ui,
                        label,
                        !session_busy,
                        script.as_deref_mut(),
                        action_frame,
                        frame_actions,
                    );
                    let access_activated =
                        shortcut && consume(action_frame, frame_actions, ui.ctx(), label);
                    if clicked || access_activated {
                        draft.tool = tool;
                        if access_activated
                            && let Some(script) = script.as_deref_mut()
                        {
                            script.access_action(label);
                        }
                    }
                }
            });

            ui.label("Activator button (virtual mouse)");
            ui.horizontal_wrapped(|ui| {
                for (label, activator) in [
                    ("Activator Primary", Activator::Primary),
                    ("Activator Secondary", Activator::Secondary),
                    ("Activator Middle", Activator::Middle),
                    ("Activator Back", Activator::X1),
                    ("Activator Forward", Activator::X2),
                ] {
                    let selected = draft.activator == activator;
                    let response = Button::new(label)
                        .variant(if selected { ButtonVariant::Secondary } else { ButtonVariant::Outline })
                        .enabled(!session_busy)
                        .id_source(("annotation", label))
                        .show(ui);
                    let accepted = script
                        .as_deref_mut()
                        .is_none_or(|script| script.observe(label, &response));
                    if access_key == Some(DrawAccessKey::Activator)
                        && selected
                        && response.enabled()
                        && consume(action_frame, frame_actions, ui.ctx(), "Focus brush activator")
                    {
                        response.request_focus();
                    }
                    if response.clicked()
                        && accepted
                        && consume(action_frame, frame_actions, ui.ctx(), label)
                    {
                        draft.activator = activator;
                    }
                }
            });

            ui.label(format!("Brush width: {} px (1–128)", draft.width));
            let mut width = f32::from(draft.width);
            Slider::new(&mut width).range(1.0, 128.0).step(1.0).show(ui);
            draft.width = width.round().clamp(1.0, 128.0) as u16;

            let color_enabled = !session_busy && self.pending.is_none();
            if !color_enabled {
                self.color_popup_open = false;
            }
            let color_button = Button::new(format!("Brush color: {}", display_rgb_hex(draft.color)))
                .variant(ButtonVariant::Outline)
                .enabled(color_enabled)
                .id_source("annotation-brush-color")
                .show(ui);
            let color_accepted = script
                .as_deref_mut()
                .is_none_or(|script| script.observe("Brush color", &color_button));
            let color_clicked = color_button.clicked()
                && color_accepted
                && consume(action_frame, frame_actions, ui.ctx(), "Brush color");
            let color_access = access_key == Some(DrawAccessKey::Color)
                && color_enabled
                && consume(action_frame, frame_actions, ui.ctx(), "Open brush color");
            if color_clicked || color_access {
                self.color_popup_open = if color_access { true } else { !self.color_popup_open };
                if self.color_popup_open {
                    self.color_hex_text = display_rgb_hex(draft.color);
                }
            }
            egui::Popup::menu(&color_button)
                .open_bool(&mut self.color_popup_open)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .show(|popup| {
                    popup.label("Brush color · #RRGGBB");
                    let hex_input = Input::new(&mut self.color_hex_text)
                        .id_source("annotation-color-hex")
                        .show(popup);
                    if color_access {
                        hex_input.request_focus();
                    }
                    if let Some(script) = script.as_deref_mut() {
                        script.access_focus("Brush color", &hex_input);
                    }
                    if let Some(rgb) = parse_rgb_hex(&self.color_hex_text) {
                        draft.color = rgb;
                    } else {
                        popup.weak("Enter six hexadecimal digits after #.");
                    }
                    popup.weak("Color changes need Apply.");
                });
            ui.label(format!("Alpha: {} (0–255)", draft.alpha));
            let mut alpha = f32::from(draft.alpha);
            Slider::new(&mut alpha).range(0.0, 255.0).step(1.0).show(ui);
            draft.alpha = alpha.round().clamp(0.0, 255.0) as u8;

            ui.label("Clear shortcut: a key such as F6 or Delete (blank disables)");
            let clear_key_input = Input::new(&mut self.clear_key_text)
                .id_source("annotation_clear_key")
                .show(ui);
            if let Some(script) = script.as_deref_mut() {
                script.observe("Clear shortcut", &clear_key_input);
            }
            if access_key == Some(DrawAccessKey::ClearShortcut)
                && consume(action_frame, frame_actions, ui.ctx(), "Focus clear shortcut")
            {
                clear_key_input.request_focus();
            }
            let mut clear_key_error = None;
            if self.clear_key_text.trim().is_empty() {
                draft.clear_key = None;
            } else {
                match parse_clear_key(self.clear_key_text.trim()) {
                    Ok(value) => draft.clear_key = Some(value),
                    _ => clear_key_error = Some("Enter a supported key name, such as F6 or Delete, or leave the field blank.".to_string()),
                }
            }
            if draft.clear_key.is_some() && seats[self.selected_user].keyboard.is_none() {
                ui.colored_label(Color32::LIGHT_RED, "This user has no keyboard assigned; the clear key is unavailable.");
            }
            Checkbox::new(&mut draft.clear_all)
                .label("Clear shortcut and Alt+C: all users after Apply")
                .show(ui);

            let settings_error = clear_key_error
                .or_else(|| draft.validate().err().map(|error| format!("{error:#}")));
            if let Some(error) = settings_error.as_deref() {
                ui.colored_label(Color32::LIGHT_RED, error);
            }
            let user_state = self
                .state
                .as_ref()
                .and_then(|state| state.users.get(self.selected_user));
            ui.weak(match user_state {
                Some(state) if state.open => "Drawing is in progress.",
                Some(_) => "Drawing layer is ready.",
                None => "No running annotation state.",
            });
            if let Some(error) = &self.error {
                ui.colored_label(Color32::LIGHT_RED, error);
            } else if let Some(error) = self.state.as_ref().and_then(|state| state.error.as_deref()) {
                ui.colored_label(Color32::LIGHT_RED, error);
            }
            if self.pending.is_some() {
                ui.weak("Waiting for the session worker acknowledgement.");
            }

            Divider::horizontal().show(ui);
            let dirty = self.committed.get(self.selected_user) != Some(draft);
            if button(
                ui,
                "Apply annotation settings",
                session_ready && (routing_supported || draft.tool == Tool::Off)
                    && !session_busy && self.pending.is_none() && dirty && settings_error.is_none()
                    && (draft.clear_key.is_none() || seats[self.selected_user].keyboard.is_some()),
                script.as_deref_mut(),
                action_frame,
                frame_actions,
            ) {
                return Some(if session_active {
                    let settings = draft.clone();
                    self.pending = Some(Pending::Configure { user: self.selected_user, settings: settings.clone() });
                    self.error = None;
                    PageAction::Worker(annotation_runtime::Command::Configure { user: self.selected_user, settings })
                } else {
                    PageAction::SaveToProfile { user: self.selected_user, settings: draft.clone() }
                });
            }
            if !session_active && dirty && self.pending.is_none() && settings_error.is_none()
                && button(
                    ui,
                    "Save annotation settings to profile",
                    (routing_supported || draft.tool == Tool::Off)
                        && (draft.clear_key.is_none() || seats[self.selected_user].keyboard.is_some()),
                    script.as_deref_mut(),
                    action_frame,
                    frame_actions,
                ) {
                    return Some(PageAction::SaveToProfile { user: self.selected_user, settings: draft.clone() });
            }

            let mut pending_action = None;
            let clear_all_applied = (if session_active {
                self.state.as_ref().and_then(|state| {
                    state
                        .users
                        .iter()
                        .find(|user| user.user == self.selected_user)
                        .map(|user| user.settings.clear_all)
                })
            } else {
                None
            })
            .unwrap_or_else(|| {
                self.committed
                    .get(self.selected_user)
                    .is_some_and(|settings| settings.clear_all)
            });
            ui.weak(if clear_all_applied {
                "Alt+C uses applied scope: all users."
            } else {
                "Alt+C uses applied scope: selected user."
            });
            ui.horizontal_wrapped(|ui| {
                let clear_enabled = session_ready && !session_busy && self.pending.is_none();
                let label = "Clear selected user's ink";
                let clicked = button(
                    ui,
                    label,
                    clear_enabled,
                    script.as_deref_mut(),
                    action_frame,
                    frame_actions,
                );
                let access_activated = access_key == Some(DrawAccessKey::Clear)
                    && !clear_all_applied
                    && clear_enabled
                    && consume(action_frame, frame_actions, ui.ctx(), label);
                if clicked || access_activated {
                    self.pending = Some(Pending::Other);
                    self.error = None;
                    pending_action = Some(PageAction::Worker(annotation_runtime::Command::ClearUser { user: self.selected_user }));
                    if access_activated
                        && let Some(script) = script.as_deref_mut()
                    {
                        script.access_action(label);
                    }
                    return;
                }
                let label = "Clear all users' ink";
                let clicked = button(
                    ui,
                    label,
                    session_ready && !session_busy && self.pending.is_none(),
                    script.as_deref_mut(),
                    action_frame,
                    frame_actions,
                );
                let access_activated = access_key == Some(DrawAccessKey::Clear)
                    && clear_all_applied
                    && clear_enabled
                    && consume(action_frame, frame_actions, ui.ctx(), label);
                if clicked || access_activated {
                    self.pending = Some(Pending::Other);
                    self.error = None;
                    pending_action = Some(PageAction::Worker(annotation_runtime::Command::ClearAll));
                    if access_activated
                        && let Some(script) = script.as_deref_mut()
                    {
                        script.access_action(label);
                    }
                }
            });
            if pending_action.is_some() {
                return pending_action;
            }

            Divider::horizontal().show(ui);
            Heading::new("Export PNG").heading().show(ui);
            let export_available = session_ready
                && !session_busy
                && self.pending.is_none()
                && self.state.as_ref().is_none_or(|state| state.export_pending.is_none());
            ui.horizontal_wrapped(|ui| {
                if button(ui, "Export selected user ink", export_available, script.as_deref_mut(), action_frame, frame_actions) {
                    self.export_ink_scope = annotation_runtime::InkScope::SelectedUser;
                }
                if button(ui, "Export all users ink", export_available, script.as_deref_mut(), action_frame, frame_actions) {
                    self.export_ink_scope = annotation_runtime::InkScope::AllUsers;
                }
            });
            let source_ready = session_ready
                && !session_busy
                && self.pending.is_none()
                && self.state.as_ref().is_none_or(|state| state.export_pending.is_none());
            let source_labels = [
                ("Transparent ink", "Export transparent ink PNG", true),
                (
                    "Selected application",
                    "Export selected target composition PNG",
                    selected_target_available,
                ),
                ("Full desktop", "Export full desktop screenshot PNG", true),
                ("Desktop region", "Export desktop region screenshot PNG", true),
            ];
            if self.export_kind == 1 && !selected_target_available {
                ui.colored_label(Color32::LIGHT_RED, "Select an eligible target window for this user before exporting a composition.");
                self.export_kind = 0;
            }
            let selected_source = source_labels[self.export_kind.min(3)].0;
            let trigger_label = "PNG source";
            let trigger = Button::new(format!("{trigger_label}: {selected_source}"))
                .sm()
                .variant(ButtonVariant::Outline)
                .enabled(source_ready)
                .id_source("annotation-export-source")
                .show(ui);
            let trigger_accepted = script
                .as_deref_mut()
                .is_none_or(|script| script.observe(trigger_label, &trigger));
            let _trigger_action = trigger.clicked()
                && trigger_accepted
                && consume(action_frame, frame_actions, ui.ctx(), trigger_label);
            let popup_context = ui.ctx().clone();
            let mut selected_kind = None;
            egui::Popup::menu(&trigger).show(|menu| {
                for (kind, (display, observation, available)) in source_labels.iter().enumerate() {
                    let visible = if *available {
                        (*display).to_owned()
                    } else {
                        format!("{display} · unavailable")
                    };
                    let option = MenuItem::new(visible)
                        .checked(kind == self.export_kind)
                        .enabled(source_ready && *available)
                        .id_source(("annotation-export-source-option", kind))
                        .show(menu);
                    option.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Button,
                            source_ready && *available,
                            (*observation).to_owned(),
                        )
                    });
                    let accepted = script
                        .as_deref_mut()
                        .is_none_or(|script| script.observe(observation, &option));
                    if option.clicked()
                        && accepted
                        && source_ready
                        && *available
                        && consume(action_frame, frame_actions, &popup_context, observation)
                    {
                        selected_kind = Some(kind);
                        menu.close();
                    }
                }
            });
            if let Some(kind) = selected_kind {
                self.export_kind = kind;
            }
            let desktop_bounds = (self.export_kind == 2 || self.export_kind == 3)
                .then(mux_windows::targets::desktop_bounds);
            let desktop_export_bounds = match self.export_kind {
                2 => desktop_bounds,
                3 => {
                    ui.label("Desktop region in physical pixels");
                    for (index, label) in ["Left", "Top", "Right", "Bottom"].into_iter().enumerate() {
                        let row = ui.horizontal(|ui| {
                            let label_response = ui.add_sized(
                                [64.0, ui.spacing().interact_size.y],
                                egui::Label::new(label),
                            );
                            let _ = Input::new(&mut self.export_region_fields[index])
                                .id_source(("annotation_export_region", label))
                                .show(ui)
                                .labelled_by(label_response.id);
                        }).response;
                        if let Some(script) = script.as_deref_mut() {
                            script.observe(&format!("Annotation export region {label}"), &row);
                        }
                    }
                    match desktop_bounds {
                        Some(desktop) => match validate_desktop_region(
                            &self.export_region_fields,
                            desktop,
                        ) {
                            Ok(bounds) => Some(bounds),
                            Err(error) => {
                                ui.colored_label(Color32::LIGHT_RED, error);
                                None
                            }
                        },
                        None => {
                            ui.colored_label(Color32::LIGHT_RED, "Desktop bounds are unavailable.");
                            None
                        }
                    }
                }
                _ => None,
            };
            let desktop_export_error = match (self.export_kind, desktop_bounds) {
                (2, Some(bounds)) => validate_desktop_bounds(bounds).err(),
                (2, None) => Some("Desktop bounds are unavailable.".to_owned()),
                _ => None,
            };
            if let Some(error) = desktop_export_error.as_deref() {
                ui.colored_label(Color32::LIGHT_RED, error);
            }
            let ink_source = match self.export_ink_scope {
                annotation_runtime::InkScope::SelectedUser => format!("{}'s ink", seats[self.selected_user].name),
                annotation_runtime::InkScope::AllUsers => "all active users' ink".to_owned(),
            };
            let export_description = match self.export_kind {
                0 => format!("Transparent canvas · {ink_source}."),
                1 => format!("Selected user's authorized target client · {ink_source}."),
                2 => format!("Full virtual desktop screenshot · {ink_source}; other visible apps may appear."),
                3 => format!("Selected physical desktop rectangle · {ink_source}; only the validated rectangle is captured."),
                _ => String::new(),
            };
            ui.label(export_description);
            if let Some(bounds) = desktop_export_bounds {
                ui.weak(format!("Physical bounds: {}", format_bounds(bounds)));
            }
            Input::new(&mut self.export_path)
                .id_source("annotation_export_path")
                .show(ui);
            let export_path_error = validate_export_path(&self.export_path).err();
            if let Some(error) = export_path_error {
                ui.colored_label(Color32::LIGHT_RED, error);
            }
            let export_pending = self.state.as_ref().and_then(|state| state.export_pending.as_ref());
            let save_enabled = session_ready
                && !session_busy
                && self.pending.is_none()
                && export_pending.is_none()
                && export_path_error.is_none()
                && desktop_export_error.is_none()
                && (self.export_kind != 3 || desktop_export_bounds.is_some());
            let save = Button::new("Save annotation PNG")
                .variant(ButtonVariant::Secondary)
                .enabled(save_enabled)
                .id_source("annotation-save-png")
                .show(ui);
            let accepted = script
                .as_deref_mut()
                .is_none_or(|script| script.observe("Save annotation PNG", &save));
            let access_activated = access_key == Some(DrawAccessKey::Save)
                && save_enabled
                && consume(action_frame, frame_actions, ui.ctx(), "Save annotation PNG");
            if (save.clicked() && accepted
                && consume(action_frame, frame_actions, ui.ctx(), "Save annotation PNG"))
                || access_activated
            {
                self.pending = Some(Pending::Other);
                self.error = None;
                if access_activated
                    && let Some(script) = script.as_deref_mut()
                {
                    script.access_action("Save annotation PNG");
                }
                return Some(PageAction::Worker(annotation_runtime::Command::Export {
                    user: self.selected_user,
                    path: self.export_path.clone(),
                    kind: match self.export_kind {
                        1 => annotation_runtime::ExportKind::SelectedTarget,
                        2 => annotation_runtime::ExportKind::FullDesktop,
                        3 => annotation_runtime::ExportKind::DesktopRegion,
                        _ => annotation_runtime::ExportKind::InkOnly,
                    },
                    ink_scope: self.export_ink_scope,
                    desktop_region: if self.export_kind == 3 { desktop_export_bounds } else { None },
                }));
            }
            if let Some(progress) = export_pending {
                let user_name = seats.get(progress.user).map_or("Selected user", |seat| seat.name.as_str());
                ui.weak(format!("PNG export for {user_name} is {}…", progress.phase));
            }
            if let Some(export) = self.state.as_ref().and_then(|state| state.export.as_ref()) {
                let user_name = seats.get(export.user).map_or("Selected user", |seat| seat.name.as_str());
                let ink_scope = match export.ink_scope {
                    annotation_runtime::InkScope::SelectedUser => "selected user's ink",
                    annotation_runtime::InkScope::AllUsers => "all users' ink",
                };
                let kind = match export.kind {
                    annotation_runtime::ExportKind::InkOnly => "transparent ink",
                    annotation_runtime::ExportKind::SelectedTarget => "target composition",
                    annotation_runtime::ExportKind::FullDesktop => "full desktop screenshot",
                    annotation_runtime::ExportKind::DesktopRegion => "desktop region screenshot",
                };
                let bounds = export.bounds.map_or_else(String::new, |bounds| format!(" · {}", format_bounds(bounds)));
                let filename = std::path::Path::new(&export.path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("PNG file");
                ui.label(format!("Saved {kind} · {user_name} · {ink_scope} · {} × {} px{bounds} · rev {} · {filename}", export.width, export.height, export.revision))
                    .on_hover_text(&export.path);
            }
            let details = Collapsible::new("Layer details")
                .id_source(("annotation", self.selected_user, "layer-details"))
                .show(ui, |ui| {
                    if let Some(state) = user_state {
                        ui.label(format!("{} strokes, {} segments{}", state.strokes, state.segments, if state.open { " (drawing)" } else { "" }));
                    } else {
                        ui.weak("No running annotation state.");
                    }
                    if let Some(overlay) = self.state.as_ref().and_then(|state| state.overlay.as_ref()) {
                        ui.label(format!("Layer bounds: {} × {} px", overlay.bounds.right - overlay.bounds.left, overlay.bounds.bottom - overlay.bounds.top));
                    }
                });
            if let Some(script) = script {
                script.observe("Layer details", &details);
            }
            None
        })
        .inner
    }
}

fn set_channel(channel: &mut u8, text: &str) {
    if let Ok(value) = text.parse::<u16>() {
        *channel = value.min(255) as u8;
    }
}

fn display_rgb_hex(color: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", color[0], color[1], color[2])
}

fn parse_rgb_hex(text: &str) -> Option<[u8; 3]> {
    let hex = text.trim().strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    Some([(value >> 16) as u8, (value >> 8) as u8, value as u8])
}

fn validate_export_path(path: &str) -> Result<(), &'static str> {
    if path.trim().is_empty() {
        return Err("Enter a PNG output path.");
    }
    if path.contains('\0') {
        return Err("The PNG output path cannot contain a NUL character.");
    }
    if path.len() > 32768 {
        return Err("The PNG output path must be 32,768 bytes or shorter.");
    }
    if !path.to_ascii_lowercase().ends_with(".png") {
        return Err("Choose a path ending in .png.");
    }
    Ok(())
}

fn validate_desktop_region(fields: &[String; 4], desktop: Bounds) -> Result<Bounds, String> {
    let names = ["Left", "Top", "Right", "Bottom"];
    let mut coordinates = [0_i32; 4];
    for (index, (name, field)) in names.into_iter().zip(fields).enumerate() {
        coordinates[index] = field
            .trim()
            .parse::<i32>()
            .map_err(|_| format!("Enter a valid physical-pixel {name} coordinate."))?;
    }
    let region = Bounds {
        left: coordinates[0],
        top: coordinates[1],
        right: coordinates[2],
        bottom: coordinates[3],
    };
    validate_desktop_bounds(region)?;
    if desktop.intersection(region) != Some(region) {
        return Err("Desktop region must lie inside the physical virtual desktop.".into());
    }
    Ok(region)
}

fn validate_desktop_bounds(bounds: Bounds) -> Result<(), String> {
    if !bounds.valid_region() {
        return Err("Desktop capture bounds must have positive width and height.".into());
    }
    let width = u64::try_from(i64::from(bounds.right) - i64::from(bounds.left))
        .map_err(|_| "Desktop capture bounds are invalid.".to_owned())?;
    let height = u64::try_from(i64::from(bounds.bottom) - i64::from(bounds.top))
        .map_err(|_| "Desktop capture bounds are invalid.".to_owned())?;
    let pixels = width
        .checked_mul(height)
        .ok_or_else(|| "Desktop capture size is too large.".to_owned())?;
    if pixels > mux_core::magnifier::MAX_PIXELS {
        return Err(format!(
            "Desktop capture exceeds the {} pixel limit; choose a smaller region.",
            mux_core::magnifier::MAX_PIXELS
        ));
    }
    Ok(())
}

fn format_bounds(bounds: Bounds) -> String {
    let width = i64::from(bounds.right) - i64::from(bounds.left);
    let height = i64::from(bounds.bottom) - i64::from(bounds.top);
    format!(
        "left {}, top {}, right {}, bottom {} ({} × {} px)",
        bounds.left, bounds.top, bounds.right, bounds.bottom, width, height
    )
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
        .id_source(("annotation", label))
        .show(ui);
    let accepted = script.is_none_or(|script| script.observe(label, &response));
    response.clicked() && accepted && consume(action_frame, frame_actions, ui.ctx(), label)
}

fn consume(
    action_frame: &mut u64,
    frame_actions: &mut BTreeSet<String>,
    context: &egui::Context,
    label: &str,
) -> bool {
    let frame = context.cumulative_frame_nr();
    if *action_frame != frame {
        *action_frame = frame;
        frame_actions.clear();
    }
    frame_actions.insert(label.to_owned())
}

fn display_clear_key(key: u16) -> String {
    if (0x70..=0x87).contains(&key) {
        format!("F{}", key - 0x6F)
    } else {
        format!("0x{key:02X}")
    }
}
fn parse_clear_key(text: &str) -> anyhow::Result<u16> {
    let value = if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16)?
    } else if let Ok(decimal) = text.parse::<u32>() {
        decimal
    } else {
        mux_windows::keyboard::code(text)?
    };
    anyhow::ensure!(
        (1..=255).contains(&value),
        "Clear shortcut key is outside the supported range"
    );
    Ok(value as u16)
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
        first.annotation.clear_key = Some(0x70);
        second.annotation.clear_key = Some(0x71);
        vec![first, second]
    }

    #[test]
    fn focus_keeps_pending_recipient_and_restores_each_users_staged_fields() {
        let seats = seats();
        let mut page = AnnotationPage::default();
        assert!(page.focus_user(&seats, 0));
        page.drafts[0].width = 23;
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
                Pending::Other => usize::MAX,
            }),
            Some(0)
        );

        page.pending = None;
        assert!(page.focus_user(&seats, 1));
        assert_eq!(page.clear_key_text, "F2");
        assert_eq!(page.drafts[0].width, 23);
    }

    #[test]
    fn external_ack_cannot_consume_gui_pending_and_authoritative_key_refreshes() {
        let mut seats = seats();
        let mut page = AnnotationPage::default();
        assert!(page.focus_user(&seats, 0));
        let mut own = page.drafts[0].clone();
        own.width = 33;
        page.pending = Some(Pending::Configure {
            user: 0,
            settings: own.clone(),
        });
        let ack = annotation_runtime::State {
            acknowledged: true,
            users: vec![],
            revision: 1,
            overlay: None,
            export: None,
            export_pending: None,
            error: None,
        };
        page.receive_external(ack.clone());
        assert!(page.pending.is_some());
        assert_eq!(page.committed[0], seats[0].annotation);

        let mut progress = ack.clone();
        progress.export_pending = Some(annotation_runtime::ExportProgress {
            user: 1,
            kind: annotation_runtime::ExportKind::InkOnly,
            ink_scope: annotation_runtime::InkScope::SelectedUser,
            phase: "preparing".into(),
        });
        page.receive_external(progress);
        assert!(page.state.as_ref().unwrap().export_pending.is_some());
        let mut completed = ack.clone();
        completed.export = Some(annotation_runtime::ExportInfo {
            user: 1,
            kind: annotation_runtime::ExportKind::InkOnly,
            ink_scope: annotation_runtime::InkScope::SelectedUser,
            revision: 1,
            path: "unwritten.png".into(),
            width: 2,
            height: 2,
            pid: None,
            hwnd: None,
            bounds: None,
        });
        page.receive_external(completed);
        assert!(page.state.as_ref().unwrap().export_pending.is_none());
        assert_eq!(
            page.state.as_ref().unwrap().export.as_ref().unwrap().user,
            1
        );
        assert!(
            page.pending.is_some(),
            "Export completion is not the GUI Configure ACK"
        );

        seats[0].annotation.width = 45;
        seats[0].annotation.clear_key = Some(0x73);
        page.sync_profile(&seats);
        assert_eq!(page.drafts[0], seats[0].annotation);
        assert_eq!(page.clear_key_text, "F4");
        assert!(page.pending.is_some());

        assert_eq!(page.receive(ack), Some((0, own.clone())));
        assert!(page.pending.is_none());
        assert_eq!(page.drafts[0], own);
        assert_eq!(page.clear_key_text, "F1");
    }
}

#[cfg(test)]
mod desktop_export_tests {
    use super::*;

    fn desktop() -> Bounds {
        Bounds {
            left: -1280,
            top: 0,
            right: 1920,
            bottom: 1080,
        }
    }

    fn fields(values: [&str; 4]) -> [String; 4] {
        values.map(str::to_owned)
    }

    #[test]
    fn desktop_region_is_physical_bounded_and_pixel_limited() {
        let region = fields(["-1280", "0", "0", "720"]);
        assert_eq!(
            validate_desktop_region(&region, desktop()).unwrap(),
            Bounds {
                left: -1280,
                top: 0,
                right: 0,
                bottom: 720,
            }
        );
        assert!(validate_desktop_region(&fields(["", "0", "0", "720"]), desktop()).is_err());
        assert!(validate_desktop_region(&fields(["0", "0", "-1", "720"]), desktop()).is_err());
        assert!(validate_desktop_region(&fields(["-1281", "0", "0", "720"]), desktop()).is_err());
        assert!(
            validate_desktop_region(&fields(["-1280", "0", "1920", "1080"]), desktop()).is_ok()
        );
        assert!(
            validate_desktop_region(
                &fields(["-1280", "0", "1920", "1080"]),
                Bounds {
                    left: 0,
                    top: 0,
                    right: 1920,
                    bottom: 1080,
                }
            )
            .is_err()
        );
        assert!(
            validate_desktop_bounds(Bounds {
                left: 0,
                top: 0,
                right: 5000,
                bottom: 5000,
            })
            .is_err()
        );
    }

    #[test]
    fn export_scope_script_fields_are_transient_and_keep_legacy_names() {
        let mut page = AnnotationPage::default();
        let seats: Vec<Seat> = serde_json::from_value(serde_json::json!([{
            "id": "a", "name": "A", "mouse": "virtual://mouse/1",
            "keyboard": "virtual://keyboard/1", "target_pid": 0
        }]))
        .unwrap();
        page.sync_profile(&seats);
        page.apply_script_fields(&BTreeMap::from([
            (
                "annotation_export_kind".to_owned(),
                "desktop_region".to_owned(),
            ),
            (
                "annotation_export_region_left".to_owned(),
                "-1280".to_owned(),
            ),
            ("annotation_export_region_top".to_owned(), "0".to_owned()),
            ("annotation_export_region_right".to_owned(), "0".to_owned()),
            (
                "annotation_export_region_bottom".to_owned(),
                "720".to_owned(),
            ),
        ]));
        assert_eq!(page.export_kind, 3);
        assert_eq!(
            page.export_region_fields,
            ["-1280", "0", "0", "720"].map(str::to_owned)
        );
        page.apply_script_fields(&BTreeMap::from([(
            "annotation_export_kind".to_owned(),
            "composition".to_owned(),
        )]));
        assert_eq!(page.export_kind, 1);
        assert_eq!(
            page.export_region_fields,
            ["-1280", "0", "0", "720"].map(str::to_owned)
        );
        assert_eq!(AnnotationPage::default().export_kind, 0);
    }
}

#[cfg(test)]
mod access_key_tests {
    use super::*;

    fn draw_test_context() -> egui::Context {
        let context = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        ouroboros_ui::theme::typography::register(&mut fonts);
        context.set_fonts(fonts);
        ouroboros_ui::Theme::install(&context, ouroboros_ui::Mode::Dark);
        context
    }

    struct Fixture {
        ctx: egui::Context,
        page: AnnotationPage,
        seats: Vec<Seat>,
        action_frame: u64,
        frame_actions: BTreeSet<String>,
    }

    impl Fixture {
        fn new() -> Self {
            let seats = serde_json::from_value(serde_json::json!([
                {"id":"a","name":"A","mouse":"virtual://mouse/1","keyboard":"virtual://keyboard/1","target_pid":0},
                {"id":"b","name":"B","mouse":"virtual://mouse/2","keyboard":"virtual://keyboard/2","target_pid":0}
            ]))
            .unwrap();
            Self {
                ctx: draw_test_context(),
                page: AnnotationPage::default(),
                seats,
                action_frame: 0,
                frame_actions: BTreeSet::new(),
            }
        }

        fn frame(
            &mut self,
            key: Option<(egui::Key, egui::Modifiers, bool)>,
            focused: bool,
            ready: bool,
            busy: bool,
        ) -> Option<PageAction> {
            let mut input = egui::RawInput {
                focused,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 1200.0),
                )),
                ..Default::default()
            };
            if let Some((key, modifiers, repeat)) = key {
                input.modifiers = modifiers;
                input.events.push(egui::Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed: true,
                    repeat,
                    modifiers,
                });
                input.events.push(egui::Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed: false,
                    repeat: false,
                    modifiers,
                });
            }
            self.render(input, ready, busy)
        }

        fn held_alt_d(&mut self, pressed: bool, repeat: bool, busy: bool) -> Option<PageAction> {
            let modifiers = egui::Modifiers::ALT;
            let mut input = egui::RawInput {
                focused: true,
                modifiers,
                ..Default::default()
            };
            input.events.push(egui::Event::Key {
                key: egui::Key::D,
                physical_key: Some(egui::Key::D),
                pressed,
                repeat,
                modifiers,
            });
            self.render(input, true, busy)
        }

        fn render(&mut self, input: egui::RawInput, ready: bool, busy: bool) -> Option<PageAction> {
            let mut action = None;
            let _ = self.ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    action = self.page.show(
                        ui,
                        &self.seats,
                        false,
                        true,
                        ready,
                        true,
                        false,
                        busy,
                        None,
                        &mut self.action_frame,
                        &mut self.frame_actions,
                    );
                });
            });
            action
        }

        fn alt(&mut self, key: egui::Key) -> Option<PageAction> {
            self.frame(
                Some((
                    key,
                    egui::Modifiers {
                        alt: true,
                        ..egui::Modifiers::NONE
                    },
                    false,
                )),
                true,
                true,
                false,
            )
        }
    }

    #[test]
    fn draw_and_eraser_keys_only_stage_the_selected_users_tool() {
        let mut fixture = Fixture::new();
        assert!(fixture.alt(egui::Key::D).is_none());
        assert_eq!(fixture.page.drafts[0].tool, Tool::Brush);
        assert_eq!(fixture.page.committed[0].tool, Tool::Off);
        assert!(fixture.alt(egui::Key::E).is_none());
        assert_eq!(fixture.page.drafts[0].tool, Tool::Eraser);
        assert!(fixture.alt(egui::Key::D).is_none());
        assert_eq!(fixture.page.drafts[0].tool, Tool::Off);
        assert!(fixture.page.focus_user(&fixture.seats, 1));
        assert!(fixture.alt(egui::Key::D).is_none());
        assert_eq!(fixture.page.drafts[1].tool, Tool::Brush);
        assert_eq!(fixture.page.committed[1].tool, Tool::Off);
        assert_eq!(fixture.page.drafts[0].tool, Tool::Off);
    }

    #[test]
    fn clear_and_save_keys_use_selected_user_worker_commands_and_pending_guard() {
        let mut fixture = Fixture::new();
        fixture.page.export_path = "C:\\Temp\\draw.png".into();
        assert!(fixture.page.focus_user(&fixture.seats, 1));
        assert!(matches!(
            fixture.alt(egui::Key::C),
            Some(PageAction::Worker(annotation_runtime::Command::ClearUser {
                user: 1
            }))
        ));
        assert!(fixture.page.pending.is_some());
        assert!(!fixture.page.focus_user(&fixture.seats, 0));
        assert!(fixture.alt(egui::Key::V).is_none());
        fixture.page.pending = None;
        assert!(matches!(
            fixture.alt(egui::Key::V),
            Some(PageAction::Worker(annotation_runtime::Command::Export {
                user: 1,
                kind: annotation_runtime::ExportKind::InkOnly,
                ink_scope: annotation_runtime::InkScope::AllUsers,
                desktop_region: None,
                ..
            }))
        ));
    }

    #[test]
    fn alt_clear_uses_applied_scope_and_never_widens_from_an_unapplied_draft() {
        let mut fixture = Fixture::new();
        fixture.page.sync_profile(&fixture.seats);
        fixture.page.drafts[0].clear_all = true;
        assert!(matches!(
            fixture.alt(egui::Key::C),
            Some(PageAction::Worker(annotation_runtime::Command::ClearUser {
                user: 0
            }))
        ));

        fixture.page.pending = None;
        fixture.seats[0].annotation.clear_all = true;
        fixture.page.sync_profile(&fixture.seats);
        fixture.page.drafts[0].clear_all = false;
        assert!(matches!(
            fixture.alt(egui::Key::C),
            Some(PageAction::Worker(annotation_runtime::Command::ClearAll))
        ));

        fixture.page.pending = None;
        assert!(fixture.page.focus_user(&fixture.seats, 1));
        assert!(matches!(
            fixture.alt(egui::Key::C),
            Some(PageAction::Worker(annotation_runtime::Command::ClearUser {
                user: 1
            }))
        ));
        fixture.page.pending = None;
        assert!(fixture.page.focus_user(&fixture.seats, 0));
        assert!(matches!(
            fixture.alt(egui::Key::C),
            Some(PageAction::Worker(annotation_runtime::Command::ClearAll))
        ));
    }

    #[test]
    fn alt_clear_prefers_live_applied_scope_over_profile_and_draft() {
        let mut fixture = Fixture::new();
        fixture.page.sync_profile(&fixture.seats);
        fixture.page.drafts[0].clear_all = false;
        let mut live_settings = fixture.seats[0].annotation.clone();
        live_settings.clear_all = true;
        fixture.page.receive_external(annotation_runtime::State {
            acknowledged: true,
            users: vec![mux_core::annotation::UserStatus {
                user: 0,
                settings: live_settings,
                strokes: 0,
                segments: 0,
                open: false,
            }],
            revision: 1,
            overlay: None,
            export: None,
            export_pending: None,
            error: None,
        });
        assert!(!fixture.page.committed[0].clear_all);
        assert!(matches!(
            fixture.alt(egui::Key::C),
            Some(PageAction::Worker(annotation_runtime::Command::ClearAll))
        ));

        fixture.page.pending = None;
        fixture.seats[0].annotation.clear_all = true;
        fixture.page.sync_profile(&fixture.seats);
        fixture.page.state.as_mut().unwrap().users[0]
            .settings
            .clear_all = false;
        assert!(fixture.page.committed[0].clear_all);
        assert!(matches!(
            fixture.alt(egui::Key::C),
            Some(PageAction::Worker(annotation_runtime::Command::ClearUser {
                user: 0
            }))
        ));
    }

    #[test]
    fn access_keys_respect_ready_busy_path_and_exact_nonrepeating_alt() {
        let mut fixture = Fixture::new();
        fixture.page.export_path = "draw.png".into();
        assert!(
            fixture
                .frame(
                    Some((
                        egui::Key::D,
                        egui::Modifiers::ALT | egui::Modifiers::CTRL,
                        false
                    )),
                    true,
                    true,
                    false
                )
                .is_none()
        );
        assert!(
            fixture
                .frame(
                    Some((
                        egui::Key::D,
                        egui::Modifiers::ALT | egui::Modifiers::SHIFT,
                        false
                    )),
                    true,
                    true,
                    false
                )
                .is_none()
        );
        assert!(
            fixture
                .frame(
                    Some((
                        egui::Key::D,
                        egui::Modifiers::ALT | egui::Modifiers::MAC_CMD,
                        false
                    )),
                    true,
                    true,
                    false
                )
                .is_none()
        );
        assert!(
            fixture
                .frame(
                    Some((
                        egui::Key::D,
                        egui::Modifiers::ALT | egui::Modifiers::COMMAND,
                        false
                    )),
                    true,
                    true,
                    false
                )
                .is_none()
        );
        // Hold D while actions are busy, then send a repeat in an enabled frame.
        // egui only preserves repeat=true when its input state already holds the key.
        assert!(fixture.held_alt_d(true, false, true).is_none());
        assert_eq!(fixture.page.drafts[0].tool, Tool::Off);
        assert!(fixture.held_alt_d(true, true, false).is_none());
        assert!(fixture.held_alt_d(false, false, false).is_none());
        assert!(
            fixture
                .frame(
                    Some((egui::Key::D, egui::Modifiers::ALT, false)),
                    false,
                    true,
                    false
                )
                .is_none()
        );
        assert_eq!(fixture.page.drafts[0].tool, Tool::Off);
        assert!(
            fixture
                .frame(
                    Some((egui::Key::D, egui::Modifiers::ALT, false)),
                    true,
                    true,
                    true
                )
                .is_none()
        );
        assert_eq!(fixture.page.drafts[0].tool, Tool::Off);
        assert!(
            fixture
                .frame(
                    Some((egui::Key::O, egui::Modifiers::ALT, false)),
                    true,
                    true,
                    true
                )
                .is_none()
        );
        assert!(!fixture.page.color_popup_open);
        assert!(
            fixture
                .frame(
                    Some((egui::Key::C, egui::Modifiers::ALT, false)),
                    true,
                    false,
                    false
                )
                .is_none()
        );
        assert!(
            fixture
                .frame(
                    Some((egui::Key::V, egui::Modifiers::ALT, false)),
                    true,
                    false,
                    false
                )
                .is_none()
        );
        fixture.page.export_path.clear();
        assert!(fixture.alt(egui::Key::V).is_none());
        assert!(fixture.page.pending.is_none());
        fixture.page.pending = Some(Pending::Other);
        assert!(fixture.alt(egui::Key::O).is_none());
        assert!(!fixture.page.color_popup_open);
    }

    #[test]
    fn text_edit_suppresses_actions_but_explicit_focus_keys_move_among_draw_controls() {
        let mut fixture = Fixture::new();
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
        let initial_focus = fixture.ctx.memory(|memory| memory.focused());
        assert!(fixture.alt(egui::Key::D).is_none());
        assert_eq!(fixture.page.drafts[0].tool, Tool::Off);
        assert!(fixture.alt(egui::Key::B).is_none());
        let activator_focus = fixture.ctx.memory(|memory| memory.focused());
        assert_ne!(activator_focus, initial_focus);
        assert!(fixture.alt(egui::Key::S).is_none());
        let shortcut_focus = fixture.ctx.memory(|memory| memory.focused());
        assert_ne!(shortcut_focus, activator_focus);
        assert!(fixture.alt(egui::Key::O).is_none());
        let color_focus = fixture.ctx.memory(|memory| memory.focused());
        assert_ne!(color_focus, shortcut_focus);
        assert!(fixture.page.color_popup_open);
        assert!(fixture.ctx.text_edit_focused());
        assert_eq!(fixture.page.drafts[0], fixture.page.committed[0]);
    }

    #[test]
    fn alt_color_opens_keyboard_editor_stages_valid_rgb_and_closes_with_escape() {
        let mut fixture = Fixture::new();
        fixture.page.sync_profile(&fixture.seats);
        let original = fixture.page.drafts[0].color;
        let alpha = fixture.page.drafts[0].alpha;
        assert!(fixture.alt(egui::Key::O).is_none());
        assert!(fixture.page.color_popup_open);
        assert!(fixture.ctx.text_edit_focused());
        assert!(fixture.alt(egui::Key::D).is_none());
        assert!(fixture.alt(egui::Key::C).is_none());
        assert_eq!(fixture.page.drafts[0].tool, Tool::Off);
        assert!(fixture.page.pending.is_none());

        // Native Windows egui input sets both ctrl and the logical command modifier.
        let ctrl = egui::Modifiers::CTRL | egui::Modifiers::COMMAND;
        let mut select_all = egui::RawInput {
            focused: true,
            modifiers: ctrl,
            ..Default::default()
        };
        for pressed in [true, false] {
            select_all.events.push(egui::Event::Key {
                key: egui::Key::A,
                physical_key: Some(egui::Key::A),
                pressed,
                repeat: false,
                modifiers: ctrl,
            });
        }
        assert!(fixture.render(select_all, true, false).is_none());
        let mut replacement = egui::RawInput {
            focused: true,
            ..Default::default()
        };
        replacement.events.push(egui::Event::Text("#112233".into()));
        assert!(fixture.render(replacement, true, false).is_none());
        assert_eq!(fixture.page.drafts[0].color, [0x11, 0x22, 0x33]);
        assert_eq!(fixture.page.committed[0].color, original);
        assert_eq!(fixture.page.drafts[0].alpha, alpha);

        fixture.page.color_hex_text = "#GGGGGG".into();
        assert!(fixture.frame(None, true, true, false).is_none());
        assert_eq!(fixture.page.drafts[0].color, [0x11, 0x22, 0x33]);
        let mut escape = egui::RawInput {
            focused: true,
            ..Default::default()
        };
        escape.events.push(egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: Some(egui::Key::Escape),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        assert!(fixture.render(escape, true, false).is_none());
        assert!(!fixture.page.color_popup_open);
        assert!(fixture.alt(egui::Key::O).is_none());
        assert!(fixture.page.color_popup_open);
        assert_eq!(fixture.page.color_hex_text, "#112233");
    }

    #[test]
    fn unrelated_open_popup_suppresses_draw_access_keys() {
        let ctx = draw_test_context();
        let mut input = egui::RawInput {
            focused: true,
            modifiers: egui::Modifiers::ALT,
            ..Default::default()
        };
        input.events.push(egui::Event::Key {
            key: egui::Key::D,
            physical_key: Some(egui::Key::D),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::ALT,
        });
        let _ = ctx.run_ui(input, |ui| {
            egui::Popup::open_id(ui.ctx(), egui::Id::new("unrelated-popup"));
            assert_eq!(draw_access_key(ui.ctx()), None);
        });
    }
}
