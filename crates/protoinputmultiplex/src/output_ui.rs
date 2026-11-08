//! Ouroboros controls for per-seat mouse-button and keyboard output conversion.
use crate::{output_runtime, ui_script::Script};
use eframe::egui;
use mux_core::{
    Seat,
    output::{KeyOutput, MouseOutput, Settings},
};
use ouroboros_ui::{
    Theme,
    atoms::{Button, ButtonVariant, Checkbox, Divider, Heading, Input, Text},
    molecules::Collapsible,
    organisms::Select,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Actions are committed by the dashboard only after successful worker acknowledgement.
pub enum PageAction {
    SaveToProfile { user: usize, settings: Settings },
    Apply { user: usize, settings: Settings },
}

const FOOTER_HEIGHT: f32 = 136.0;
const MIN_SCROLL_BODY_HEIGHT: f32 = 48.0;

#[derive(Clone, Default, Serialize)]
struct UserDraft {
    identity: String,
    committed: Settings,
    draft: Settings,
    editor: EditorState,
}

#[derive(Clone, Serialize)]
pub struct EditorState {
    pub selected_button: usize,
    pub button_key_text: [String; 5],
    pub source_key_text: String,
    pub destination_key_text: String,
    pub disable_key_output: bool,
    pub parse_error: Option<String>,
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            selected_button: 0,
            button_key_text: std::array::from_fn(|_| String::new()),
            source_key_text: String::new(),
            destination_key_text: String::new(),
            disable_key_output: false,
            parse_error: None,
        }
    }
}

#[derive(Clone, Serialize)]
pub struct PendingSnapshot {
    pub user: usize,
    pub identity: String,
    pub settings: Settings,
}

/// Serializable, app-local evidence for the output editor.
#[derive(Clone, Serialize)]
pub struct Snapshot {
    pub selected_user: usize,
    pub selected_identity: Option<String>,
    pub committed: Vec<(String, Settings)>,
    pub drafts: Vec<(String, Settings)>,
    pub pending: Option<PendingSnapshot>,
    pub error: Option<String>,
    pub status: Option<String>,
    pub editor: Option<EditorState>,
}

#[derive(Clone)]
struct Pending {
    user: usize,
    identity: String,
    settings: Settings,
}

#[derive(Default)]
pub struct OutputPage {
    selected_user: usize,
    selected_identity: Option<String>,
    users: Vec<UserDraft>,
    pending: Option<Pending>,
    state: Option<output_runtime::State>,
    acknowledged_identity: Option<String>,
    error: Option<String>,
    status: Option<String>,
    #[cfg(test)]
    last_footer_rects: Vec<egui::Rect>,
    #[cfg(test)]
    last_footer_clip: Option<egui::Rect>,
}

impl OutputPage {
    pub fn has_profile_draft(&self) -> bool {
        self.pending.is_some()
            || self.users.iter().any(|entry| {
                entry.draft != entry.committed || editor_is_dirty(&entry.editor, &entry.draft)
            })
    }
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            selected_user: self.selected_user,
            selected_identity: self.selected_identity.clone(),
            committed: self
                .users
                .iter()
                .map(|u| (u.identity.clone(), u.committed.clone()))
                .collect(),
            drafts: self
                .users
                .iter()
                .map(|u| (u.identity.clone(), u.draft.clone()))
                .collect(),
            pending: self.pending.as_ref().map(|p| PendingSnapshot {
                user: p.user,
                identity: p.identity.clone(),
                settings: p.settings.clone(),
            }),
            error: self.error.clone(),
            status: self.status.clone(),
            editor: self.users.get(self.selected_user).map(|u| u.editor.clone()),
        }
    }

    /// Sync the acknowledged profile while retaining staged edits by stable seat identity.
    pub fn sync_profile(&mut self, seats: &[Seat]) {
        let selected = self.selected_identity.clone().or_else(|| {
            self.users
                .get(self.selected_user)
                .map(|u| u.identity.clone())
        });
        let mut previous: BTreeMap<_, _> = std::mem::take(&mut self.users)
            .into_iter()
            .map(|entry| (entry.identity.clone(), entry))
            .collect();
        let mut next = Vec::with_capacity(seats.len());
        for seat in seats {
            let identity = seat.identity();
            let committed = seat.output.clone();
            let mut entry = previous.remove(&identity).unwrap_or_else(|| {
                let mut entry = UserDraft {
                    identity: identity.clone(),
                    committed: committed.clone(),
                    draft: committed.clone(),
                    editor: EditorState::default(),
                };
                sync_editor(&mut entry.editor, &committed);
                entry
            });
            if entry.committed != committed {
                let dirty =
                    entry.draft != entry.committed || editor_is_dirty(&entry.editor, &entry.draft);
                entry.committed = committed.clone();
                if !dirty && self.pending.as_ref().is_none_or(|p| p.identity != identity) {
                    entry.draft = committed;
                    sync_editor(&mut entry.editor, &entry.draft);
                }
            }
            entry.identity = identity;
            next.push(entry);
        }
        self.users = next;
        self.selected_user = selected
            .as_ref()
            .and_then(|id| self.users.iter().position(|u| &u.identity == id))
            .unwrap_or_else(|| self.selected_user.min(self.users.len().saturating_sub(1)));
        self.selected_identity = self
            .users
            .get(self.selected_user)
            .map(|u| u.identity.clone());
    }

    pub fn owns_ack(&self, state: &output_runtime::State) -> bool {
        state.acknowledged
            && self.pending.as_ref().is_some_and(|p| {
                state.user == Some(p.user)
                    && self
                        .users
                        .get(p.user)
                        .is_some_and(|entry| entry.identity == p.identity)
            })
    }

    /// Only a matching successful worker acknowledgement commits staged settings.
    pub fn receive(&mut self, state: output_runtime::State) -> Option<(usize, Settings)> {
        self.state = Some(state.clone());
        if !state.acknowledged {
            if let Some(error) = state.error.clone()
                && self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| state.user == Some(pending.user))
            {
                self.pending = None;
                self.error = Some(error);
                self.status = None;
                self.acknowledged_identity = None;
            }
            return None;
        }
        self.acknowledged_identity = None;
        let pending = self.pending.clone()?;
        if state.user != Some(pending.user)
            || self
                .users
                .get(pending.user)
                .is_none_or(|entry| entry.identity != pending.identity)
        {
            self.pending = None;
            self.error =
                Some("Output settings acknowledgement did not match the pending user.".into());
            return None;
        }
        self.pending = None;
        self.error = state
            .error
            .map(|error| format!("User {}: {error}", pending.user + 1));
        if self.error.is_some() {
            self.acknowledged_identity = None;
            return None;
        }
        let Some(committed) = state
            .users
            .iter()
            .find(|status| status.user == pending.user)
            .map(|status| status.settings.clone())
        else {
            self.error =
                Some("Output acknowledgement omitted the user's committed settings.".into());
            return None;
        };
        if let Some(entry) = self
            .users
            .iter_mut()
            .find(|entry| entry.identity == pending.identity)
        {
            entry.committed = committed.clone();
            entry.draft = committed.clone();
            sync_editor(&mut entry.editor, &committed);
        }
        self.acknowledged_identity = Some(pending.identity.clone());
        self.status = Some(format!("User {} settings acknowledged.", pending.user + 1));
        Some((pending.user, committed))
    }

    /// Clear a failed request and preserve the user's staged values for correction or retry.
    pub fn request_failed(&mut self, error: impl Into<String>) {
        self.pending = None;
        self.error = Some(error.into());
        self.status = None;
        self.acknowledged_identity = None;
    }

    /// Record a profile-memory commit. The profile still needs its separate disk Save.
    pub fn saved_to_profile(&mut self, user: usize, settings: Settings) {
        if let Some(entry) = self.users.get_mut(user) {
            entry.committed = settings.clone();
            entry.draft = settings.clone();
            sync_editor(&mut entry.editor, &settings);
            self.error = None;
            self.status =
                Some("Saved in profile memory. Save the profile to write it to disk.".into());
        }
    }

    pub fn session_stopped(&mut self) {
        self.pending = None;
        self.state = None;
        self.error = None;
        self.status = None;
        self.acknowledged_identity = None;
    }

    /// Select a profile user without replacing that user's draft or editor text.
    pub fn select_user(&mut self, user: usize) {
        if user < self.users.len() {
            let changed = self.selected_user != user
                || self.selected_identity.as_deref() != Some(self.users[user].identity.as_str());
            self.selected_user = user;
            self.selected_identity = Some(self.users[user].identity.clone());
            if changed {
                self.error = None;
                self.status = None;
            }
        }
    }

    /// Apply deterministic script field edits to the real staged controls.
    pub fn apply_script_fields(&mut self, fields: &BTreeMap<String, String>) {
        if self.pending.is_some() {
            return;
        }
        if let Some(value) = fields.get("output_user")
            && let Ok(user) = value.parse::<usize>()
        {
            self.select_user(user);
        }
        let Some(entry) = self.users.get_mut(self.selected_user) else {
            return;
        };
        if let Some(value) = fields.get("output_button")
            && let Ok(i) = value.parse::<usize>()
            && i < 5
        {
            entry.editor.selected_button = i;
        }
        for (name, value) in fields {
            match name.as_str() {
                "output_user" | "output_button" => (),
                "output_kind" => set_button_kind(entry, value),
                "output_key" => set_button_key(entry, value),
                "output_source_key" => entry.editor.source_key_text = value.clone(),
                "output_destination_key" => entry.editor.destination_key_text = value.clone(),
                "output_disable" => entry.editor.disable_key_output = parse_bool(value),
                _ => (),
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        seats: &[Seat],
        active_users: &[bool],
        session_active: bool,
        session_ready: bool,
        routing_supported: bool,
        session_busy: bool,
        script: Option<&mut Script>,
        action_frame: &mut u64,
        frame_actions: &mut BTreeSet<String>,
    ) -> Option<PageAction> {
        self.sync_profile(seats);
        let mut script = script;
        let mut action = None;
        #[cfg(test)]
        let mut footer_rects = Vec::new();
        #[cfg(test)]
        let mut footer_clip = None;
        ui.vertical(|ui| {
            let body_height = (ui.available_height() - FOOTER_HEIGHT).max(MIN_SCROLL_BODY_HEIGHT);
            egui::ScrollArea::vertical()
                .id_salt("output-workspace-body")
                .max_height(body_height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    self.show_body(
                        ui,
                        seats,
                        active_users,
                        session_busy,
                        &mut script,
                        action_frame,
                        frame_actions,
                    );
                });
            Divider::horizontal().show(ui);
            #[cfg(test)]
            {
                footer_clip = Some(ui.clip_rect());
            }
            let selected_name = seats
                .get(self.selected_user)
                .map(|seat| short_name(&seat.name, 18))
                .unwrap_or_else(|| "No user".to_owned());
            let edit_state = if self.is_selected_dirty() {
                "Changes staged"
            } else {
                "No staged changes"
            };
            ui.add(egui::Label::new(format!("{} · {}", selected_name, edit_state)).truncate())
                .on_hover_text(
                    seats
                        .get(self.selected_user)
                        .map(|seat| seat.name.as_str())
                        .unwrap_or("No user"),
                );
            ui.horizontal(|ui| {
                let enabled = !session_busy && self.pending.is_none();
                let (clicked, rect) = action_button_named(
                    ui,
                    "Apply",
                    "Apply output rules",
                    enabled
                        && session_ready
                        && self.can_commit(seats, active_users, session_active, routing_supported),
                    script.as_deref_mut(),
                    action_frame,
                    frame_actions,
                );
                #[cfg(test)]
                footer_rects.push(rect);
                #[cfg(not(test))]
                let _ = rect;
                if clicked {
                    let user = self.selected_user;
                    let settings = self.users[user].draft.clone();
                    self.pending = Some(Pending {
                        user,
                        identity: self.users[user].identity.clone(),
                        settings: settings.clone(),
                    });
                    self.error = None;
                    action = Some(PageAction::Apply { user, settings });
                }
                let (clicked, rect) = action_button_named(
                    ui,
                    "Save to profile",
                    "Save output rules to profile",
                    enabled
                        && !session_active
                        && self.can_commit(seats, active_users, session_active, routing_supported),
                    script.as_deref_mut(),
                    action_frame,
                    frame_actions,
                );
                #[cfg(test)]
                footer_rects.push(rect);
                #[cfg(not(test))]
                let _ = rect;
                if action.is_none() && clicked {
                    let user = self.selected_user;
                    action = Some(PageAction::SaveToProfile {
                        user,
                        settings: self.users[user].draft.clone(),
                    });
                }
                let dirty = self.is_selected_dirty();
                let (clicked, rect) = action_button_named(
                    ui,
                    "Discard",
                    "Discard output changes",
                    enabled && dirty,
                    script,
                    action_frame,
                    frame_actions,
                );
                #[cfg(test)]
                footer_rects.push(rect);
                #[cfg(not(test))]
                let _ = rect;
                if clicked {
                    self.discard_user(self.selected_user);
                }
            });
            if let Some(error) = &self.error {
                ui.label(
                    egui::RichText::new("Output change failed; draft kept")
                        .color(Theme::get(ui).error),
                )
                .on_hover_text(error);
            } else if let Some(error) = self
                .users
                .get(self.selected_user)
                .and_then(|entry| entry.editor.parse_error.as_ref())
            {
                ui.label(
                    egui::RichText::new("Fix the key entry before saving")
                        .color(Theme::get(ui).error),
                )
                .on_hover_text(error);
            } else if self.has_unapplied_key_entry() {
                ui.weak("Add the key mapping before applying or saving.");
            }
            if let Some(status) = &self.status {
                ui.add(
                    egui::Label::new(egui::RichText::new(status).color(Theme::get(ui).primary))
                        .truncate(),
                )
                .on_hover_text(status);
            }
        });
        #[cfg(test)]
        {
            self.last_footer_rects = footer_rects;
            self.last_footer_clip = footer_clip;
        }
        action
    }

    #[allow(clippy::too_many_arguments)]
    fn show_body(
        &mut self,
        ui: &mut egui::Ui,
        seats: &[Seat],
        active_users: &[bool],
        session_busy: bool,
        script: &mut Option<&mut Script>,
        action_frame: &mut u64,
        frame_actions: &mut BTreeSet<String>,
    ) {
        Heading::new("Output mapping").heading().show(ui);
        Text::new(
            "Convert virtual mouse buttons or named keys into per-user virtual keyboard output.",
        )
        .caption()
        .wrap()
        .show(ui);
        if seats.is_empty() {
            ui.weak("Add a user before configuring output rules.");
            return;
        }
        let font = ui.style().text_styles[&egui::TextStyle::Body].clone();
        let option_width = (ui.available_width() - 56.0).max(24.0);
        let names: Vec<_> = seats
            .iter()
            .enumerate()
            .map(|(i, seat)| {
                let prefix = format!("User {}: ", i + 1);
                let remaining =
                    (option_width - measured_text_width(ui, &prefix, font.clone())).max(0.0);
                format!(
                    "{}{}",
                    prefix,
                    bounded_display_text(ui, &seat.name, remaining, font.clone())
                )
            })
            .collect();
        let selection = Select::new(&mut self.selected_user)
            .options(names)
            .placeholder("Choose output user")
            .show(ui);
        let selection = selection.on_hover_text(
            seats
                .get(self.selected_user)
                .map(|seat| seat.name.as_str())
                .unwrap_or("Choose output user"),
        );
        if selection.changed() {
            self.selected_identity = self
                .users
                .get(self.selected_user)
                .map(|u| u.identity.clone());
            self.error = None;
        }
        let user = self.selected_user.min(seats.len() - 1);
        self.selected_user = user;
        let seat = &seats[user];
        let identity = seat.identity();
        ui.add(egui::Label::new(format!("Selected user: {}", seat.name)).wrap())
            .on_hover_text(&seat.name);
        if let Some(error) = &self.error {
            ui.colored_label(Theme::get(ui).error, error);
        }
        let is_virtual_mouse = mux_core::emulated::parse(&seat.mouse)
            .ok()
            .flatten()
            .is_some();
        let has_virtual_keyboard = seat
            .keyboard
            .as_deref()
            .and_then(|p| mux_core::emulated::parse(p).ok().flatten())
            .is_some();
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.identity == identity)
        {
            ui.weak("Waiting for the worker to acknowledge these output settings.");
        }
        if let Some(state) = &self.state
            && state.acknowledged
            && state.error.is_none()
            && state.user == Some(user)
            && self.acknowledged_identity.as_deref() == Some(identity.as_str())
        {
            ui.colored_label(
                Theme::get(ui).primary,
                "Worker acknowledged output settings.",
            );
        }
        if active_users.get(user).is_some_and(|active| !active) {
            ui.colored_label(
                Theme::get(ui).error,
                "Virtual replica removed; output rules are unavailable for this user.",
            );
            return;
        }
        if !is_virtual_mouse {
            ui.colored_label(
                Theme::get(ui).error,
                "Output conversion is unavailable for physical mouse seats.",
            );
            ui.weak("These controls are read-only for this user.");
            return;
        }
        if self
            .state
            .as_ref()
            .and_then(|s| s.users.iter().find(|status| status.user == user))
            .is_some_and(|status| !status.active)
        {
            ui.colored_label(
                Theme::get(ui).error,
                "This virtual user is inactive or has been removed.",
            );
            ui.weak("Output rules are unavailable until the user is active.");
            return;
        }
        let can_edit = !session_busy && self.pending.is_none();
        if !can_edit {
            ui.weak("Editing is locked while a session command is pending.");
        }
        let entry = &mut self.users[user];
        ui.add_enabled_ui(can_edit, |ui| {
            let UserDraft { draft, editor, identity, .. } = entry;
            Heading::new("Mouse buttons to output").heading().show(ui);
            Text::new("Legacy keeps the existing mapped mouse output; Disabled consumes the source button.").caption().wrap().show(ui);
            Collapsible::new("How converted clicks are delivered")
                .id_source(("output_help", identity.as_str()))
                .show(ui, |ui| {
                Text::new("Key output goes to the user's currently selected keyboard target. Use a Legacy mouse click first to select or focus a target; hovering alone does not select it.").caption().wrap().show(ui);
            });
            let labels = ["Primary", "Secondary", "Middle", "Back", "Forward"];
            let mut editor_error = None;
            for (i, label) in labels.iter().enumerate() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(*label);
                    let mut kind = match draft.buttons[i] { MouseOutput::Legacy => 0, MouseOutput::Key { .. } => 1, MouseOutput::Disabled => 2 };
                    if Select::new(&mut kind).options(["Legacy mouse", "Named key", "Disabled"].map(str::to_owned)).placeholder("Choose output").show(ui).changed() {
                        set_button_kind_parts(draft, editor, i, kind);
                    }
                    if matches!(draft.buttons[i], MouseOutput::Key { .. }) {
                        Input::new(&mut editor.button_key_text[i]).id_source(("output_button_key", identity.as_str(), i)).show(ui);
                        if let Ok(key) = parse_vk(&editor.button_key_text[i]) { draft.buttons[i] = MouseOutput::Key { key }; }
                        else { editor_error = Some(format!("{label} output key is not a supported virtual key name.")); }
                    }
                });
            }
            Divider::horizontal().show(ui);
            Heading::new("Keyboard remapping").heading().show(ui);
            Text::new("Map a named source key to another named virtual key, or disable its output.").caption().wrap().show(ui);
            ui.horizontal_wrapped(|ui| {
                ui.label("Source key");
                Input::new(&mut editor.source_key_text).id_source(("output_source_key", identity.as_str())).show(ui);
                ui.label("Destination key");
                Input::new(&mut editor.destination_key_text).id_source(("output_destination_key", identity.as_str())).show(ui);
                Checkbox::new(&mut editor.disable_key_output).label("Disable this source key").show(ui);
            });
            let source = parse_vk(&editor.source_key_text);
            let destination = parse_vk(&editor.destination_key_text);
            let valid_map = source.is_ok() && (editor.disable_key_output || destination.is_ok());
            if (!editor.source_key_text.trim().is_empty() || !editor.destination_key_text.trim().is_empty()) && !valid_map {
                editor_error = Some("Enter supported named keys (virtual-key values 8-254) before adding a rule.".into());
            }
            if action_button(
                ui,
                "Add or replace key mapping",
                valid_map,
                script.as_deref_mut(),
                action_frame,
                frame_actions,
            ) {
                add_key_mapping(draft, editor);
            }
            if !editor.source_key_text.trim().is_empty()
                || !editor.destination_key_text.trim().is_empty()
                || editor.disable_key_output
            {
                ui.weak("Add the key mapping before applying or saving.");
            }
            let mut remove = None;
            for (source, output) in &draft.keys {
                let output_label = match output { KeyOutput::Key { key } => format!("{} → {}", display_vk(*source), display_vk(*key)), KeyOutput::Disabled => format!("{} → Disabled", display_vk(*source)) };
                ui.horizontal_wrapped(|ui| { ui.label(output_label); let label = format!("Remove key mapping {}", display_vk(*source)); if action_button(ui, &label, true, script.as_deref_mut(), action_frame, frame_actions) { remove = Some(*source); } });
            }
            if let Some(source) = remove { draft.keys.remove(&source); }
            if draft.keys.len() > 255 { editor_error = Some("The output map is limited to 255 key rules.".into()); }
            editor.parse_error = editor_error;
            if action_button(ui, "Restore legacy mouse output", true, script.as_deref_mut(), action_frame, frame_actions) {
                draft.buttons = [MouseOutput::Legacy; 5];
                editor.button_key_text = std::array::from_fn(|_| String::new());
                editor.source_key_text.clear();
                editor.destination_key_text.clear();
                editor.disable_key_output = false;
                editor.parse_error = None;
            }
        });
        let entry = &self.users[user];
        let keyboard_required = entry.draft.requires_keyboard() || !entry.draft.keys.is_empty();
        if keyboard_required && !has_virtual_keyboard {
            ui.colored_label(
                Theme::get(ui).error,
                "Assign a virtual keyboard before saving or applying key output rules.",
            );
        }
        let validity = output_runtime::validate_keys(&entry.draft, mux_core::emulated::BASE | 3)
            .err()
            .map(|e| e.to_string());
        if let Some(error) = entry.editor.parse_error.as_ref().or(validity.as_ref()) {
            ui.colored_label(Theme::get(ui).error, error);
        }
    }

    fn can_commit(
        &self,
        seats: &[Seat],
        active_users: &[bool],
        session_active: bool,
        routing_supported: bool,
    ) -> bool {
        let (Some(seat), Some(entry)) = (
            seats.get(self.selected_user),
            self.users.get(self.selected_user),
        ) else {
            return false;
        };
        let keyboard = seat
            .keyboard
            .as_deref()
            .and_then(|p| mux_core::emulated::parse(p).ok().flatten())
            .is_some();
        let needs_keyboard = entry.draft.requires_keyboard() || !entry.draft.keys.is_empty();
        mux_core::emulated::parse(&seat.mouse)
            .ok()
            .flatten()
            .is_some()
            && active_users
                .get(self.selected_user)
                .is_none_or(|active| *active)
            && (routing_supported || !session_active)
            && (!needs_keyboard || keyboard)
            && !self.has_unapplied_key_entry()
            && entry.editor.parse_error.is_none()
            && output_runtime::validate_keys(&entry.draft, mux_core::emulated::BASE | 3).is_ok()
    }

    fn has_unapplied_key_entry(&self) -> bool {
        self.users.get(self.selected_user).is_some_and(|entry| {
            !entry.editor.source_key_text.trim().is_empty()
                || !entry.editor.destination_key_text.trim().is_empty()
                || entry.editor.disable_key_output
        })
    }

    fn is_selected_dirty(&self) -> bool {
        self.users
            .get(self.selected_user)
            .is_some_and(|e| e.draft != e.committed || editor_is_dirty(&e.editor, &e.draft))
    }

    fn discard_user(&mut self, user: usize) {
        if let Some(entry) = self.users.get_mut(user) {
            entry.draft = entry.committed.clone();
            sync_editor(&mut entry.editor, &entry.draft);
        }
        self.error = None;
        self.status = None;
    }
}

fn sync_editor(editor: &mut EditorState, settings: &Settings) {
    for (i, output) in settings.buttons.iter().enumerate() {
        editor.button_key_text[i] = match output {
            MouseOutput::Key { key } => display_vk(*key),
            _ => String::new(),
        };
    }
    editor.source_key_text.clear();
    editor.destination_key_text.clear();
    editor.disable_key_output = false;
    editor.parse_error = None;
}

fn editor_is_dirty(editor: &EditorState, settings: &Settings) -> bool {
    !editor.source_key_text.is_empty()
        || !editor.destination_key_text.is_empty()
        || editor.disable_key_output
        || editor.button_key_text.iter().enumerate().any(|(i, text)| {
            let expected = match settings.buttons[i] {
                MouseOutput::Key { key } => display_vk(key),
                _ => String::new(),
            };
            *text != expected
        })
}

fn add_key_mapping(draft: &mut Settings, editor: &mut EditorState) -> bool {
    let Ok(source) = parse_vk(&editor.source_key_text) else {
        return false;
    };
    let output = if editor.disable_key_output {
        KeyOutput::Disabled
    } else {
        let Ok(key) = parse_vk(&editor.destination_key_text) else {
            return false;
        };
        KeyOutput::Key { key }
    };
    draft.keys.insert(source, output);
    editor.source_key_text.clear();
    editor.destination_key_text.clear();
    editor.disable_key_output = false;
    true
}

fn set_button_kind(entry: &mut UserDraft, value: &str) {
    let kind = match value.to_ascii_lowercase().as_str() {
        "legacy" | "mouse" => 0,
        "key" => 1,
        "disabled" | "off" => 2,
        _ => return,
    };
    set_button_kind_at(entry, entry.editor.selected_button, kind);
}

fn set_button_kind_at(entry: &mut UserDraft, index: usize, kind: usize) {
    set_button_kind_parts(&mut entry.draft, &mut entry.editor, index, kind);
}

fn set_button_kind_parts(
    draft: &mut Settings,
    editor: &mut EditorState,
    index: usize,
    kind: usize,
) {
    if index >= 5 {
        return;
    }
    match kind {
        0 => {
            draft.buttons[index] = MouseOutput::Legacy;
            editor.button_key_text[index].clear();
        }
        1 => match parse_vk(&editor.button_key_text[index]) {
            Ok(key) => draft.buttons[index] = MouseOutput::Key { key },
            Err(_) => {
                draft.buttons[index] = MouseOutput::Key { key: 65 };
                editor.button_key_text[index] = "A".into();
            }
        },
        2 => {
            draft.buttons[index] = MouseOutput::Disabled;
            editor.button_key_text[index].clear();
        }
        _ => (),
    }
    editor.parse_error = None;
}

fn set_button_key(entry: &mut UserDraft, value: &str) {
    let index = entry.editor.selected_button;
    if index >= 5 {
        return;
    }
    entry.editor.button_key_text[index] = value.to_owned();
    match parse_vk(value) {
        Ok(key) => {
            entry.draft.buttons[index] = MouseOutput::Key { key };
            entry.editor.parse_error = None;
        }
        Err(_) => {
            entry.editor.parse_error =
                Some("Enter a supported named key with virtual-key value 8-254.".into())
        }
    }
}

fn action_button(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    script: Option<&mut Script>,
    action_frame: &mut u64,
    frame_actions: &mut BTreeSet<String>,
) -> bool {
    action_button_named(
        ui,
        label,
        label,
        enabled,
        script,
        action_frame,
        frame_actions,
    )
    .0
}

fn action_button_named(
    ui: &mut egui::Ui,
    visible_label: &str,
    action_label: &str,
    enabled: bool,
    script: Option<&mut Script>,
    action_frame: &mut u64,
    frame_actions: &mut BTreeSet<String>,
) -> (bool, egui::Rect) {
    let response = Button::new(visible_label)
        .variant(ButtonVariant::Outline)
        .enabled(enabled)
        .id_source(("output", action_label))
        .show(ui);
    let accessible_label = action_label.to_owned();
    response.widget_info(move || {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, accessible_label.clone())
    });
    let response = response.on_hover_text(action_label);
    let accepted = script.is_none_or(|script| script.observe(action_label, &response));
    (
        response.clicked()
            && accepted
            && consume(action_frame, frame_actions, ui.ctx(), action_label),
        response.rect,
    )
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

fn parse_vk(text: &str) -> anyhow::Result<u16> {
    let value = mux_windows::keyboard::code(text)?;
    anyhow::ensure!(
        (8..=254).contains(&value),
        "virtual keyboard code must be in the range 8-254"
    );
    Ok(value as u16)
}

fn display_vk(key: u16) -> String {
    match key {
        0x41..=0x5A => char::from_u32(u32::from(key)).unwrap_or('?').to_string(),
        0x30..=0x39 => char::from_u32(u32::from(key)).unwrap_or('?').to_string(),
        0x70..=0x87 => format!("F{}", key - 0x6F),
        0x60..=0x69 => format!("Numpad{}", key - 0x60),
        _ => format!("VK 0x{key:02X}"),
    }
}

fn parse_bool(text: &str) -> bool {
    matches!(text.to_ascii_lowercase().as_str(), "1" | "true" | "yes")
}

fn short_name(name: &str, max_chars: usize) -> String {
    let mut chars = name.chars();
    let prefix = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn measured_text_width(ui: &mut egui::Ui, text: &str, font: egui::FontId) -> f32 {
    ui.fonts_mut(|fonts| {
        fonts
            .layout_no_wrap(text.to_owned(), font, ui.visuals().text_color())
            .size()
            .x
    })
}

fn bounded_display_text(
    ui: &mut egui::Ui,
    text: &str,
    max_width: f32,
    font: egui::FontId,
) -> String {
    const MAX_SCALARS: usize = 256;
    let boundaries = text
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(text.len()))
        .take(MAX_SCALARS + 1)
        .collect::<Vec<_>>();
    let truncated_source = boundaries.last().copied().unwrap_or(0) < text.len();
    let mut fits =
        |candidate: &str| measured_text_width(ui, candidate, font.clone()) <= max_width.max(0.0);
    if !truncated_source && fits(text) {
        return text.to_owned();
    }
    let mut low = 0;
    let mut high = boundaries.len().saturating_sub(1);
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        let candidate = format!("{}…", &text[..boundaries[middle]]);
        if fits(&candidate) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    if low == 0 && !fits("…") {
        String::new()
    } else {
        format!("{}…", &text[..boundaries[low]])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mux_core::output::{KeyOutput, MouseOutput};

    fn seat(id: &str, name: &str, settings: Settings) -> Seat {
        Seat {
            id: id.into(),
            name: name.into(),
            mouse: format!("\\?\\ROOT#VIRTUALMOUSE#{id}"),
            keyboard: None,
            target_pid: 0,
            desktop_start: None,
            cursor: Default::default(),
            annotation: Default::default(),
            magnifier: Default::default(),
            output: settings,
        }
    }

    #[test]
    fn external_sync_preserves_dirty_values_by_identity_across_append_and_reorder() {
        let mut page = OutputPage::default();
        let a = seat("a", "A", Settings::default());
        let b = seat("b", "B", Settings::default());
        page.sync_profile(&[a.clone(), b.clone()]);
        page.select_user(0);
        page.users[0].editor.source_key_text = "BAD".into();
        page.users[0].editor.parse_error = Some("invalid source".into());
        page.users[0].draft.buttons[0] = MouseOutput::Disabled;
        let mut external = a.clone();
        external.output.buttons[1] = MouseOutput::Disabled;
        let c = seat("c", "C", Settings::default());
        page.sync_profile(&[b, external, c]);
        assert_eq!(page.selected_identity.as_deref(), Some("a"));
        let a = &page.users[1];
        assert_eq!(a.draft.buttons[0], MouseOutput::Disabled);
        assert_eq!(a.committed.buttons[1], MouseOutput::Disabled);
        assert_eq!(a.editor.source_key_text, "BAD");
        assert_eq!(page.users[2].identity, "c");
    }

    #[test]
    fn initial_profile_key_mappings_populate_editor_and_start_clean() {
        let mut settings = Settings::default();
        settings.buttons[0] = MouseOutput::Key { key: 116 };
        let mut page = OutputPage::default();
        page.sync_profile(&[seat("loaded", "Loaded profile", settings.clone())]);
        assert_eq!(page.users[0].editor.button_key_text[0], "F5");
        assert_eq!(page.users[0].draft, settings);
        assert!(!page.is_selected_dirty());
    }

    #[test]
    fn unadded_key_entry_blocks_commit_until_mapping_is_added() {
        let mut virtual_user = seat("keys", "Keyboard user", Settings::default());
        virtual_user.mouse = "virtual://mouse/1".into();
        virtual_user.keyboard = Some("virtual://keyboard/1".into());
        let mut page = OutputPage::default();
        page.sync_profile(std::slice::from_ref(&virtual_user));
        page.users[0].editor.source_key_text = "A".into();
        page.users[0].editor.destination_key_text = "F5".into();

        assert!(page.is_selected_dirty());
        assert!(page.has_unapplied_key_entry());
        assert!(!page.can_commit(std::slice::from_ref(&virtual_user), &[true], false, true));
        assert!(!page.can_commit(std::slice::from_ref(&virtual_user), &[true], true, true));

        let entry = &mut page.users[0];
        assert!(add_key_mapping(&mut entry.draft, &mut entry.editor));
        assert!(entry.editor.source_key_text.is_empty());
        assert!(entry.editor.destination_key_text.is_empty());
        assert_eq!(
            entry.draft.keys.get(&65),
            Some(&KeyOutput::Key { key: 116 })
        );
        assert!(!page.has_unapplied_key_entry());
        assert!(page.can_commit(std::slice::from_ref(&virtual_user), &[true], false, true));
        assert!(page.can_commit(std::slice::from_ref(&virtual_user), &[true], true, true));
    }

    #[test]
    fn acknowledgement_after_reorder_cannot_commit_to_another_identity() {
        let mut page = OutputPage::default();
        page.sync_profile(&[
            seat("a", "A", Settings::default()),
            seat("b", "B", Settings::default()),
        ]);
        let staged = Settings {
            buttons: [MouseOutput::Disabled; 5],
            keys: BTreeMap::new(),
        };
        page.users[0].draft = staged.clone();
        page.pending = Some(Pending {
            user: 0,
            identity: "a".into(),
            settings: staged.clone(),
        });
        page.sync_profile(&[
            seat("b", "B", Settings::default()),
            seat("a", "A", Settings::default()),
        ]);
        let ack = output_runtime::State {
            acknowledged: true,
            user: Some(0),
            users: vec![output_runtime::UserStatus {
                user: 0,
                active: true,
                settings: Settings::default(),
            }],
            error: None,
        };
        assert!(!page.owns_ack(&ack));
        assert!(page.receive(ack).is_none());
        assert_eq!(page.users[1].identity, "a");
        assert_eq!(page.users[1].draft, staged);
        assert_eq!(page.users[0].identity, "b");
        assert_ne!(page.users[0].draft, staged);
    }

    #[test]
    fn pending_ack_and_rejection_preserve_draft_until_successful_ack() {
        let mut page = OutputPage::default();
        page.sync_profile(&[seat("a", "A", Settings::default())]);
        let settings = Settings {
            buttons: [MouseOutput::Disabled; 5],
            keys: BTreeMap::new(),
        };
        page.users[0].draft = settings.clone();
        page.pending = Some(Pending {
            user: 0,
            identity: "a".into(),
            settings: settings.clone(),
        });
        page.request_failed("worker request failed");
        assert!(page.pending.is_none());
        assert_eq!(page.users[0].draft, settings);
        assert_eq!(
            page.snapshot().error.as_deref(),
            Some("worker request failed")
        );

        page.pending = Some(Pending {
            user: 0,
            identity: "a".into(),
            settings: settings.clone(),
        });
        let rejected = output_runtime::State {
            acknowledged: true,
            user: Some(0),
            users: vec![],
            error: Some("validation failed".into()),
        };
        assert!(page.receive(rejected).is_none());
        assert_eq!(page.users[0].draft, settings);
        assert_eq!(page.error.as_deref(), Some("User 1: validation failed"));

        page.pending = Some(Pending {
            user: 0,
            identity: "a".into(),
            settings: settings.clone(),
        });
        let ack = output_runtime::State {
            acknowledged: true,
            user: Some(0),
            users: vec![output_runtime::UserStatus {
                user: 0,
                active: true,
                settings: settings.clone(),
            }],
            error: None,
        };
        assert!(page.receive(ack).is_some());
        assert_eq!(page.users[0].committed, settings);
        assert_eq!(page.users[0].draft, settings);
    }

    #[test]
    fn delayed_acknowledgement_names_its_user_after_selection_changes() {
        let mut page = OutputPage::default();
        page.sync_profile(&[
            seat("a", "Alpha", Settings::default()),
            seat("b", "Beta", Settings::default()),
        ]);
        page.pending = Some(Pending {
            user: 0,
            identity: "a".into(),
            settings: Settings::default(),
        });
        page.select_user(1);
        let ack = output_runtime::State {
            acknowledged: true,
            user: Some(0),
            users: vec![output_runtime::UserStatus {
                user: 0,
                active: true,
                settings: Settings::default(),
            }],
            error: None,
        };
        assert!(page.receive(ack).is_some());
        assert_eq!(page.selected_identity.as_deref(), Some("b"));
        assert_eq!(
            page.status.as_deref(),
            Some("User 1 settings acknowledged.")
        );
        page.select_user(0);
        assert!(page.status.is_none());
        page.request_failed("still actionable");
        page.select_user(0);
        assert!(
            page.error.is_some(),
            "Revisiting the same selection must not hide feedback"
        );
    }

    #[test]
    fn discard_restores_latest_committed_and_profile_save_is_memory_only() {
        let mut page = OutputPage::default();
        let mut committed = Settings::default();
        committed.keys.insert(65, KeyOutput::Disabled);
        page.sync_profile(&[seat("a", "A", committed.clone())]);
        page.users[0].draft.keys.clear();
        page.users[0].editor.source_key_text = "bad".into();
        page.users[0].editor.parse_error = Some("bad".into());
        assert!(page.is_selected_dirty());
        page.discard_user(0);
        assert_eq!(page.users[0].draft, committed);
        assert!(!page.is_selected_dirty());
        let newer = Settings {
            buttons: [MouseOutput::Disabled; 5],
            keys: BTreeMap::new(),
        };
        page.saved_to_profile(0, newer.clone());
        assert_eq!(page.users[0].committed, newer);
        assert!(page.status.as_deref().unwrap().contains("profile memory"));
    }

    #[test]
    fn compact_page_keeps_footer_visible_at_minimum_content_size() {
        let mut page = OutputPage::default();
        let seats = [seat(
            "a",
            &"User with an unusually long profile name 漢字 mixed long name ".repeat(3),
            Settings::default(),
        )];
        page.request_failed("worker rejection with a deliberately long diagnostic ".repeat(5));
        let context = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        ouroboros_ui::theme::typography::register(&mut fonts);
        context.set_fonts(fonts);
        Theme::install(&context, ouroboros_ui::Mode::Dark);
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(320.0, 210.0),
                )),
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    let font = ui.style().text_styles[&egui::TextStyle::Body].clone();
                    let available = (ui.available_width() - 56.0).max(24.0);
                    let prefix = "User 1: ";
                    let name_width = available - measured_text_width(ui, prefix, font.clone());
                    let bounded =
                        bounded_display_text(ui, &seats[0].name, name_width, font.clone());
                    assert!(
                        measured_text_width(ui, &format!("{prefix}{bounded}"), font) <= available
                    );
                    page.show(
                        ui,
                        &seats,
                        &[true],
                        false,
                        false,
                        true,
                        false,
                        None,
                        &mut 0,
                        &mut BTreeSet::new(),
                    );
                });
            },
        );
        assert!(!output.shapes.is_empty());
        assert_eq!(page.last_footer_rects.len(), 3);
        assert!(page.last_footer_rects.iter().all(|rect| {
            page.last_footer_clip
                .is_some_and(|clip| clip.contains_rect(*rect))
        }));
    }
}
