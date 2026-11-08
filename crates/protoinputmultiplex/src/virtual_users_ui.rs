//! Ouroboros controls for ephemeral, live virtual mouse replicas.
use crate::{ui_script::Script, virtual_users};
use eframe::egui;
use mux_core::{Seat, replication::EdgeOptions};
use ouroboros_ui::{
    atoms::{Button, Checkbox, Divider, Heading, Input, Text},
    molecules::Collapsible,
    organisms::Select,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
pub enum PageAction {
    Worker(virtual_users::Command),
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub source_id: String,
    pub target_pid: u32,
    pub name: String,
    pub options: EdgeOptions,
    pub selected_clone_id: Option<String>,
    pub state: Option<virtual_users::State>,
    pub pending: bool,
    pub error: Option<String>,
}

#[derive(Clone, Copy)]
struct Pending {
    expected_revision: u64,
}

pub struct VirtualUsersPage {
    source_id: String,
    target_pid: u32,
    name: String,
    options: EdgeOptions,
    selected_clone_id: Option<String>,
    state: Option<virtual_users::State>,
    pending: Option<Pending>,
    error: Option<String>,
}

impl Default for VirtualUsersPage {
    fn default() -> Self {
        Self {
            source_id: String::new(),
            target_pid: 0,
            name: "Mouse replica".into(),
            options: EdgeOptions::default(),
            selected_clone_id: None,
            state: None,
            pending: None,
            error: None,
        }
    }
}

impl VirtualUsersPage {
    pub fn reset_session(&mut self) {
        self.state = None;
        self.pending = None;
        self.selected_clone_id = None;
        self.error = None;
    }

    pub fn apply_script_fields(&mut self, fields: &BTreeMap<String, String>) {
        for (key, value) in fields {
            match key.as_str() {
                "clone_source_id" => self.source_id.clone_from(value),
                "clone_target_pid" => self.target_pid = value.parse().unwrap_or_default(),
                "clone_name" => self.name.clone_from(value),
                "clone_id" => self.selected_clone_id = Some(value.clone()),
                "clone_motion" => {
                    self.options.motion = value.parse().unwrap_or(self.options.motion)
                }
                "clone_wheel" => self.options.wheel = value.parse().unwrap_or(self.options.wheel),
                "clone_horizontal_wheel" => {
                    self.options.horizontal_wheel =
                        value.parse().unwrap_or(self.options.horizontal_wheel)
                }
                "clone_button_primary" => {
                    self.options.buttons[0] = value.parse().unwrap_or(self.options.buttons[0])
                }
                "clone_button_secondary" => {
                    self.options.buttons[1] = value.parse().unwrap_or(self.options.buttons[1])
                }
                "clone_button_middle" => {
                    self.options.buttons[2] = value.parse().unwrap_or(self.options.buttons[2])
                }
                "clone_button_back" => {
                    self.options.buttons[3] = value.parse().unwrap_or(self.options.buttons[3])
                }
                "clone_button_forward" => {
                    self.options.buttons[4] = value.parse().unwrap_or(self.options.buttons[4])
                }
                "clone_click_delay_ms" => {
                    self.options.click_delay_ms =
                        value.parse().unwrap_or(self.options.click_delay_ms)
                }
                "clone_step_interval_ms" => {
                    self.options.step_interval_ms =
                        value.parse().unwrap_or(self.options.step_interval_ms)
                }
                "clone_position" => {
                    self.options.position = match value.as_str() {
                        "anchored" => mux_core::replication::PositionMode::Anchored,
                        "source_relative" => mux_core::replication::PositionMode::SourceRelative,
                        _ => self.options.position,
                    }
                }
                "clone_offset_x" => {
                    self.options.offset_x = value.parse().unwrap_or(self.options.offset_x)
                }
                "clone_offset_y" => {
                    self.options.offset_y = value.parse().unwrap_or(self.options.offset_y)
                }
                _ => {}
            }
        }
    }

    pub fn receive(&mut self, state: virtual_users::State) {
        if self
            .state
            .as_ref()
            .is_some_and(|current| current.revision > state.revision)
        {
            if state.acknowledged && self.pending.is_some() {
                self.pending = None;
                self.error = Some(
                    "The user roster changed after this request; inspect before retrying.".into(),
                );
            }
            return;
        }
        if state.acknowledged {
            if let Some(pending) = self.pending
                && state.revision == pending.expected_revision
            {
                self.pending = None;
            } else if self.pending.is_some() && state.error.is_some() {
                self.pending = None;
            }
            self.error = state.error.clone();
        }
        if let Some(selected) = &self.selected_clone_id
            && !state
                .slots
                .iter()
                .any(|slot| slot.clone_of.is_some() && slot.seat.identity() == *selected)
        {
            self.selected_clone_id = None;
        }
        self.state = Some(state);
    }

    /// Refresh session telemetry without acknowledging a local request or replacing its fields.
    pub fn receive_external(&mut self, state: virtual_users::State) -> bool {
        if self
            .state
            .as_ref()
            .is_some_and(|current| current.revision > state.revision)
        {
            return false;
        }
        self.state = Some(state);
        true
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            source_id: self.source_id.clone(),
            target_pid: self.target_pid,
            name: self.name.clone(),
            options: self.options,
            selected_clone_id: self.selected_clone_id.clone(),
            state: self.state.clone(),
            pending: self.pending.is_some(),
            error: self.error.clone(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        seats: &[Seat],
        allowed_pids: &[u32],
        targets: &[mux_windows::targets::Target],
        automatic_desktop: bool,
        session_active: bool,
        session_ready: bool,
        session_busy: bool,
        mapper_blocks_creation: bool,
        script: Option<&mut Script>,
        action_frame: &mut u64,
        frame_actions: &mut BTreeSet<String>,
    ) -> Option<PageAction> {
        let mut script = script;
        let mut action = None;
        egui::ScrollArea::vertical().id_salt("virtual-users-body").show(ui, |ui| {
            // Nested scroll layouts may retain a wider content size after resizing.
            // Bound every form row to the actual visible body, not the old content width.
            let width = ui.available_width().min(ui.clip_rect().right() - ui.max_rect().left()).max(1.0);
            ui.set_min_width(0.0);
            ui.set_max_width(width);
            let select_text_width = (width - 160.0).max(48.0);
            Heading::new("Virtual replicas").heading().show(ui);
            Text::new("Create a session-only virtual user for an explicitly selected target already in this session's process scope.")
                .caption().wrap().show(ui);
            if mapper_blocks_creation {
                ui.weak("Disarm Input Mapper and complete its pending releases before starting a virtual replica.");
            }
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            let state = self.state.clone();
            if self.pending.is_some() {
                ui.weak("Waiting for the matching worker acknowledgement.");
            }

            if seats.is_empty() {
                ui.weak("Start a routing session with at least one source user first.");
                return;
            }
            let active_sources: Vec<usize> = seats.iter().enumerate()
                .filter_map(|(index, _)| state.as_ref().and_then(|state| state.slots.get(index)).is_none_or(|slot| slot.active).then_some(index))
                .collect();
            let source_options: Vec<String> = active_sources
                .iter()
                .map(|index| format!("User {}: {}", index + 1, seats[*index].name))
                .collect();
            let mut selected_source = seats
                .iter()
                .position(|seat| seat.identity() == self.source_id)
                .unwrap_or(0);
            let mut source_option = active_sources.iter().position(|index| *index == selected_source).unwrap_or(0);
            ui.horizontal_wrapped(|ui| {
                ui.label("Source user");
                let response = Select::new(&mut source_option)
                    .options(source_options.iter().map(|label| bounded_option_label(ui, label, select_text_width)))
                    .placeholder("Choose source")
                    .show(ui);
                let full_label = source_options.get(source_option).map_or("", String::as_str);
                response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, response.enabled(), full_label));
                response.on_hover_text(full_label);
            });
            selected_source = active_sources.get(source_option).copied().unwrap_or(usize::MAX);
            if let Some(source) = seats.get(selected_source) {
                self.source_id.clone_from(&source.identity());
            }
            ui.horizontal_wrapped(|ui| {
                ui.label("Replica name");
                Input::new(&mut self.name).placeholder("Replica name").show(ui);
            });

            let mut pid_options: Vec<(u32, String)> = targets
                .iter()
                .filter(|target| {
                    target.pid != 0
                        && (allowed_pids.is_empty() || allowed_pids.contains(&target.pid))
                        && target.pid != std::process::id()
                })
                .map(|target| (target.pid, format!("PID {} · {}", target.pid, target.title)))
                .collect();
            pid_options.sort_by_key(|(pid, _)| *pid);
            pid_options.dedup_by_key(|(pid, _)| *pid);
            let labels: Vec<String> = pid_options.iter().map(|(_, label)| bounded_option_label(ui, label, select_text_width)).collect();
            if pid_options.is_empty() {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    "No eligible target window is available in the current routing scope.",
                );
            } else {
                let mut target_index = pid_options
                    .iter()
                    .position(|(pid, _)| *pid == self.target_pid)
                    .unwrap_or(0);
                ui.horizontal_wrapped(|ui| {
                    ui.label("Allowed target");
                    let response = Select::new(&mut target_index)
                        .options(labels)
                        .placeholder("Choose allowed PID")
                        .show(ui);
                    let full_label = pid_options.get(target_index).map_or("", |(_, label)| label.as_str());
                    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, response.enabled(), full_label));
                    response.on_hover_text(full_label);
                });
                if let Some((pid, _)) = pid_options.get(target_index) {
                    self.target_pid = *pid;
                }
            }

            let route_options = Collapsible::new("Route options")
                .id_source("virtual-replica-route-options")
                .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                self.options.motion = checkbox(ui, "Movement", self.options.motion, script.as_deref_mut(), action_frame, frame_actions);
                self.options.wheel = checkbox(ui, "Vertical wheel", self.options.wheel, script.as_deref_mut(), action_frame, frame_actions);
                self.options.horizontal_wheel = checkbox(ui, "Horizontal wheel", self.options.horizontal_wheel, script.as_deref_mut(), action_frame, frame_actions);
            });
            ui.horizontal_wrapped(|ui| {
                for (index, label) in ["Primary", "Secondary", "Middle", "Back", "Forward"].into_iter().enumerate() {
                    self.options.buttons[index] = checkbox(ui, &format!("Allow {label}"), self.options.buttons[index], script.as_deref_mut(), action_frame, frame_actions);
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("Click delay (ms)");
                ui.add(egui::DragValue::new(&mut self.options.click_delay_ms).range(0..=60_000));
                ui.label("Step interval (ms)");
                ui.add(egui::DragValue::new(&mut self.options.step_interval_ms).range(0..=1_000));
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("Pointer position");
                for (label, position) in [
                    ("Anchored", mux_core::replication::PositionMode::Anchored),
                    ("Source-relative", mux_core::replication::PositionMode::SourceRelative),
                ] {
                    let response = Button::new(label)
                        .variant(if self.options.position == position {
                            ouroboros_ui::atoms::ButtonVariant::Secondary
                        } else {
                            ouroboros_ui::atoms::ButtonVariant::Outline
                        })
                        .show(ui);
                    let accepted = script.as_deref_mut().is_none_or(|script| script.observe(label, &response));
                    if response.clicked() && accepted && consume(action_frame, frame_actions, ui.ctx(), label) {
                        self.options.position = position;
                        if position == mux_core::replication::PositionMode::Anchored {
                            self.options.offset_x = 0;
                            self.options.offset_y = 0;
                        }
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                let relative = self.options.position == mux_core::replication::PositionMode::SourceRelative;
                ui.add_enabled_ui(relative, |ui| {
                    ui.label("Relative offset X/Y");
                    ui.add(egui::DragValue::new(&mut self.options.offset_x).range(-32_767..=32_767));
                    ui.add(egui::DragValue::new(&mut self.options.offset_y).range(-32_767..=32_767));
                });
            });
            });
            let _ = script.as_deref_mut().is_none_or(|s| s.observe("Route options", &route_options));
            let valid_name = !self.name.trim().is_empty() && self.name.trim().chars().count() <= 64;
            if let Err(error) = self.options.validate() {
                ui.colored_label(ui.visuals().error_fg_color, error.to_string());
            }
            let can_start = session_active
                && automatic_desktop
                && session_ready
                && !session_busy
                && !mapper_blocks_creation
                && self.pending.is_none()
                && pid_options.iter().any(|(pid, _)| *pid == self.target_pid)
                && seats.iter().enumerate().any(|(index, seat)| seat.identity() == self.source_id && active_sources.contains(&index))
                && valid_name
                && state.is_some();
            let can_start = can_start && self.options.validate().is_ok();
            if button(
                ui,
                "Start virtual replica",
                can_start,
                script.as_deref_mut(),
                action_frame,
                frame_actions,
            ) {
                let revision = state.as_ref().map_or(0, |state| state.revision);
                self.pending = Some(Pending {
                    expected_revision: revision.saturating_add(1),
                });
                self.error = None;
                action = Some(PageAction::Worker(virtual_users::Command::Start {
                    source_id: self.source_id.clone(),
                    target_pid: self.target_pid,
                    name: self.name.trim().to_owned(),
                    options: self.options,
                    expected_revision: Some(revision),
                }));
            }

            Divider::horizontal().show(ui);
            Heading::new("Generated session users").heading().show(ui);
            let Some(state) = state else {
                ui.weak("The worker will report generated users after routing starts.");
                return;
            };
            ui.weak(format!("{} users · roster revision {}", state.slots.len(), state.revision));
            let mut selected = None;
            for slot in state.slots.iter().filter(|slot| slot.clone_of.is_some()) {
                let clone_id = slot.seat.identity();
                ui.horizontal_wrapped(|ui| {
                    if quiet_button(
                        ui,
                        &format!("Select replica {}", slot.user + 1),
                        true,
                        script.as_deref_mut(),
                        action_frame,
                        frame_actions,
                    ) {
                        selected = Some(clone_id.clone());
                    }
                    wrapped_value(ui, &slot.seat.name, egui::TextStyle::Body);
                });
                Text::new(format!("{} · PID {}{}",
                    if slot.active { "Active" } else { "Stopped" },
                    slot.seat.target_pid,
                    if self.selected_clone_id.as_deref() == Some(clone_id.as_str()) { " · Selected" } else { "" }
                )).caption().wrap().show(ui);
            }
            if let Some(id) = selected {
                self.selected_clone_id = Some(id);
            }
            let identity_details = Collapsible::new("Stable identity details")
                .id_source("virtual-replica-identities")
                .show(ui, |ui| {
                    for slot in state.slots.iter().filter(|slot| slot.clone_of.is_some()) {
                        Text::new(&slot.seat.name).label_strong().wrap().show(ui);
                        Text::new("User ID").caption().show(ui);
                        wrapped_value(ui, &slot.seat.identity(), egui::TextStyle::Monospace);
                        Text::new("Source ID").caption().show(ui);
                        wrapped_value(ui, slot.clone_of.as_deref().unwrap_or("unknown"), egui::TextStyle::Monospace);
                    }
                });
            let _ = script.as_deref_mut().is_none_or(|s| s.observe("Stable identity details", &identity_details));
            let selected_active = self.selected_clone_id.as_ref().and_then(|id| {
                state.slots.iter().find(|slot| slot.seat.identity() == *id && slot.clone_of.is_some())
            }).is_some_and(|slot| slot.active);
            if quiet_button(
                ui,
                "Stop selected virtual replica",
                session_active && session_ready && !session_busy && self.pending.is_none() && selected_active,
                script.as_deref_mut(),
                action_frame,
                frame_actions,
            ) {
                let id = self.selected_clone_id.clone().unwrap_or_default();
                self.pending = Some(Pending {
                    expected_revision: state.revision.saturating_add(1),
                });
                self.error = None;
                action = Some(PageAction::Worker(virtual_users::Command::Stop {
                    id,
                    expected_revision: Some(state.revision),
                }));
            }
            if self.state.as_ref().is_some_and(|state| !state.slots.is_empty()) {
                Text::new("Removed slots remain visible for this session so stable user indexes are never reused. Stopping the parent routing session removes all generated registrations and ephemeral graph edges.")
                    .caption().wrap().show(ui);
            }
        });
        if let Some(script) = script {
            script.virtual_users_state(&self.snapshot());
        }
        action
    }
}

fn bounded_option_label(ui: &egui::Ui, text: &str, max_width: f32) -> String {
    let font = ui.style().text_styles[&egui::TextStyle::Body].clone();
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
                .layout_no_wrap(
                    candidate.to_owned(),
                    font.clone(),
                    ui.visuals().text_color(),
                )
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
fn wrapped_value(ui: &mut egui::Ui, value: &str, style: egui::TextStyle) {
    let width = ui
        .available_width()
        .min(ui.clip_rect().right() - ui.cursor().left())
        .max(1.0);
    let mut job = egui::text::LayoutJob::simple(
        value.to_owned(),
        ui.style().text_styles[&style].clone(),
        ui.visuals().text_color(),
        width,
    );
    job.wrap.break_anywhere = true;
    ui.add(egui::Label::new(job).wrap().selectable(true));
}

fn button(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    script: Option<&mut Script>,
    action_frame: &mut u64,
    frame_actions: &mut BTreeSet<String>,
) -> bool {
    let response = Button::new(label).enabled(enabled).show(ui);
    let accepted = script.is_none_or(|script| script.observe(label, &response));
    response.clicked() && accepted && consume(action_frame, frame_actions, ui.ctx(), label)
}

fn quiet_button(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    script: Option<&mut Script>,
    action_frame: &mut u64,
    frame_actions: &mut BTreeSet<String>,
) -> bool {
    let response = Button::new(label)
        .variant(ouroboros_ui::atoms::ButtonVariant::Outline)
        .enabled(enabled)
        .show(ui);
    let accepted = script.is_none_or(|script| script.observe(label, &response));
    response.clicked() && accepted && consume(action_frame, frame_actions, ui.ctx(), label)
}

fn checkbox(
    ui: &mut egui::Ui,
    label: &str,
    value: bool,
    script: Option<&mut Script>,
    action_frame: &mut u64,
    frame_actions: &mut BTreeSet<String>,
) -> bool {
    let mut value = value;
    let original = value;
    let response = Checkbox::new(&mut value).label(label).show(ui);
    let accepted = script.is_none_or(|script| script.observe(label, &response));
    if response.clicked() && accepted && consume(action_frame, frame_actions, ui.ctx(), label) {
        value
    } else {
        original
    }
}

fn consume(
    action_frame: &mut u64,
    frame_actions: &mut BTreeSet<String>,
    ctx: &egui::Context,
    label: &str,
) -> bool {
    let frame = ctx.cumulative_frame_nr();
    if *action_frame != frame {
        *action_frame = frame;
        frame_actions.clear();
    }
    frame_actions.insert(label.to_owned())
}

#[cfg(test)]
mod external_state_tests {
    use super::*;

    #[test]
    fn external_ack_preserves_pending_request_and_unsubmitted_fields() {
        let mut page = VirtualUsersPage {
            pending: Some(Pending {
                expected_revision: 1,
            }),
            name: "Unsubmitted replica".into(),
            target_pid: 42,
            error: Some("local feedback".into()),
            ..Default::default()
        };
        let state = virtual_users::State {
            acknowledged: true,
            revision: 1,
            ..Default::default()
        };
        assert!(page.receive_external(state.clone()));
        assert!(page.snapshot().pending);
        assert_eq!(page.name, "Unsubmitted replica");
        assert_eq!(page.target_pid, 42);
        assert_eq!(page.error.as_deref(), Some("local feedback"));
        page.receive(state);
        assert!(!page.snapshot().pending);
    }

    #[test]
    fn newer_external_roster_survives_delayed_local_ack() {
        let mut page = VirtualUsersPage {
            pending: Some(Pending {
                expected_revision: 1,
            }),
            ..Default::default()
        };
        let newer = virtual_users::State {
            revision: 2,
            ..Default::default()
        };
        assert!(page.receive_external(newer));
        let old = virtual_users::State {
            acknowledged: true,
            revision: 1,
            ..Default::default()
        };
        page.receive(old.clone());
        assert!(!page.snapshot().pending);
        assert_eq!(page.snapshot().state.unwrap().revision, 2);
        assert!(page.error.is_some());
        assert!(!page.receive_external(old));
        assert_eq!(page.snapshot().state.unwrap().revision, 2);
    }
}
