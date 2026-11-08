//! Ouroboros visual editor for the stable, directed input replication graph.
use crate::{automation_runtime, ui_script::Script};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use mux_core::{
    Seat,
    replication::{Definition, Edge, EdgeOptions, History, PositionMode, ResolvedEdge},
};
use ouroboros_ui::atoms::{Button, ButtonVariant, Divider, Heading, Input, Text};
use ouroboros_ui::molecules::Collapsible;
use ouroboros_ui::organisms::Select;
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;

fn definition_endpoints_active(
    definition: &Definition,
    seats: &[Seat],
    active_users: &[bool],
) -> bool {
    definition.edges.iter().all(|edge| {
        [edge.source.as_str(), edge.target.as_str()]
            .into_iter()
            .all(|id| {
                seats
                    .iter()
                    .position(|seat| seat.identity() == id)
                    .is_some_and(|index| active_users.get(index).copied().unwrap_or(true))
            })
    })
}

#[derive(Clone)]
pub enum PageAction {
    Apply(Definition),
    CancelReplication { expected_revision: u64 },
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub draft: Definition,
    pub applied: Definition,
    pub selected_source: Option<String>,
    pub selected_target: Option<String>,
    pub selected_edge: Option<usize>,
    pub selected_options: EdgeOptions,
    pub undo_available: bool,
    pub redo_available: bool,
    pub pending: bool,
    pub graph_revision: u64,
    pub unresolved: Option<String>,
    pub error: Option<String>,
    pub replication: crate::replication_queue::Snapshot,
}

#[derive(Clone)]
struct PendingApply {
    definition: Definition,
    expected_revision: u64,
    edges: Vec<automation_runtime::Edge>,
}

pub struct ReplicationPage {
    history: Option<History>,
    profile: Definition,
    applied: Definition,
    selected_source: Option<String>,
    selected_target: Option<String>,
    selected_edge: Option<usize>,
    source_stage: Option<String>,
    positions: BTreeMap<String, [f32; 2]>,
    mouse: bool,
    keyboard: bool,
    text: bool,
    staged_options: EdgeOptions,
    file_path: String,
    error: Option<String>,
    unresolved: Option<String>,
    pending: Option<PendingApply>,
    revision: u64,
    undo_count: usize,
    redo_count: usize,
    replication: crate::replication_queue::Snapshot,
}

impl Default for ReplicationPage {
    fn default() -> Self {
        Self {
            history: History::new(Definition::default()).ok(),
            profile: Definition::default(),
            applied: Definition::default(),
            selected_source: None,
            selected_target: None,
            selected_edge: None,
            source_stage: None,
            positions: BTreeMap::new(),
            mouse: true,
            keyboard: false,
            text: false,
            staged_options: EdgeOptions::default(),
            file_path: "profiles/replication.json".into(),
            error: None,
            unresolved: None,
            pending: None,
            revision: 0,
            undo_count: 0,
            redo_count: 0,
            replication: crate::replication_queue::Snapshot::default(),
        }
    }
}

impl ReplicationPage {
    pub fn has_profile_draft(&self) -> bool {
        self.pending.is_some() || self.draft() != &self.profile || self.source_stage.is_some()
    }
    pub fn reset_profile(&mut self, profile: &mux_core::Profile) {
        self.profile = profile.replication.clone();
        self.applied = profile.replication.clone();
        self.history = History::new(profile.replication.clone()).ok();
        self.pending = None;
        self.error = None;
        self.undo_count = 0;
        self.redo_count = 0;
        self.selected_edge = None;
        self.source_stage = None;
        self.staged_options = EdgeOptions::default();
        self.refresh_seats(&profile.seats);
    }

    /// Re-check endpoint identities without deleting edges when a user disappears.
    pub fn refresh_seats(&mut self, seats: &[Seat]) {
        if self
            .selected_source
            .as_ref()
            .is_some_and(|id| !seats.iter().any(|seat| seat.identity() == *id))
        {
            self.selected_source = None;
        }
        if self
            .selected_target
            .as_ref()
            .is_some_and(|id| !seats.iter().any(|seat| seat.identity() == *id))
        {
            self.selected_target = None;
        }
        if self
            .source_stage
            .as_ref()
            .is_some_and(|id| !seats.iter().any(|seat| seat.identity() == *id))
        {
            self.source_stage = None;
        }
        self.positions
            .retain(|id, _| seats.iter().any(|seat| seat.identity() == *id));
        let draft = self.draft().clone();
        self.unresolved = draft.resolve(seats).err().map(|error| format!("{error:#}"));
    }

    /// Apply automation telemetry and commit only the matching acknowledged graph revision.
    pub fn receive(
        &mut self,
        state: &automation_runtime::State,
        seats: &[Seat],
    ) -> Option<Definition> {
        if state.graph_revision < self.revision {
            if state.acknowledged && self.pending.is_some() {
                self.pending = None;
                self.error = Some(
                    "A newer replication graph is already active; inspect it before applying again."
                        .into(),
                );
            }
            return None;
        }
        self.revision = self.revision.max(state.graph_revision);
        self.replication = state.replication.clone();
        let had_pending = self.pending.is_some();
        if let Some(pending) = self.pending.clone() {
            if state.acknowledged && state.graph_revision == pending.expected_revision {
                let matches = state.edges == pending.edges;
                if matches && state.error.is_none() {
                    let definition = pending.definition;
                    self.pending = None;
                    self.error = None;
                    self.applied = definition.clone();
                    self.profile = definition.clone();
                    self.unresolved = definition.resolve(seats).err().map(|e| format!("{e:#}"));
                    return Some(definition);
                }
                if state.error.is_some() {
                    self.error = state.error.clone();
                } else {
                    self.error =
                        Some("The worker acknowledged a different replication graph.".into());
                }
                self.pending = None;
            } else if state.acknowledged
                && (state.graph_revision > pending.expected_revision || state.error.is_some())
            {
                self.error = state.error.clone().or_else(|| {
                    Some("The replication graph changed before this Apply was acknowledged.".into())
                });
                self.pending = None;
            }
        }
        if state.acknowledged && state.error.is_some() && self.pending.is_none() {
            self.error = state.error.clone();
        }
        if !had_pending {
            let edges: Vec<_> = state
                .edges
                .iter()
                .map(|edge| ResolvedEdge {
                    source: edge.source,
                    target: edge.target,
                    mouse: edge.mouse,
                    keyboard: edge.keyboard,
                    text: edge.text,
                    options: edge.options,
                })
                .collect();
            if let Ok(actual) = Definition::from_resolved(seats, &edges) {
                self.sync_applied(&actual);
                self.refresh_seats(seats);
                return Some(actual);
            }
        }
        self.refresh_seats(seats);
        None
    }

    /// External state is authoritative telemetry, never a receipt for this page's pending Apply.
    pub fn receive_external(
        &mut self,
        state: &automation_runtime::State,
        seats: &[Seat],
    ) -> Option<Definition> {
        if state.graph_revision < self.revision {
            return None;
        }
        let actual = Definition::from_resolved(seats, &state.edges).ok()?;
        self.revision = state.graph_revision;
        self.replication = state.replication.clone();
        if self.pending.is_some() {
            self.applied = actual.clone();
            self.profile = actual.clone();
        } else {
            self.sync_applied(&actual);
        }
        self.refresh_seats(seats);
        Some(actual)
    }

    pub fn applied_local(&mut self, definition: &Definition) {
        self.applied = definition.clone();
        self.profile = definition.clone();
        self.error = None;
        self.pending = None;
    }

    pub fn sync_applied(&mut self, definition: &Definition) {
        let changed = definition != &self.applied;
        let was_clean = self.draft() == &self.applied && self.draft() == &self.profile;
        self.applied = definition.clone();
        self.profile = definition.clone();
        if changed
            && was_clean
            && self.pending.is_none()
            && let Ok(history) = History::new(definition.clone())
        {
            self.history = Some(history);
            self.undo_count = 0;
            self.redo_count = 0;
        }
    }

    pub fn failed_apply(&mut self, error: impl Into<String>) {
        self.pending = None;
        self.error = Some(error.into());
    }

    pub fn session_stopped(&mut self) {
        self.pending = None;
        self.revision = 0;
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            draft: self.draft().clone(),
            applied: self.applied.clone(),
            selected_source: self.selected_source.clone(),
            selected_target: self.selected_target.clone(),
            selected_edge: self.selected_edge,
            selected_options: self.staged_options,
            undo_available: self.undo_count > 0,
            redo_available: self.redo_count > 0,
            pending: self.pending.is_some(),
            graph_revision: self.revision,
            unresolved: self.unresolved.clone(),
            error: self.error.clone(),
            replication: self.replication.clone(),
        }
    }

    pub fn apply_script_fields(&mut self, fields: &BTreeMap<String, String>, seats: &[Seat]) {
        let mut options = self.staged_options;
        let mut options_touched = false;
        let mut options_error = None;
        for (key, value) in fields {
            match key.as_str() {
                "graph_source" => self.selected_source = select_identity(value, seats),
                "graph_target" => self.selected_target = select_identity(value, seats),
                "graph_mouse" => self.mouse = parse_bool(value).unwrap_or(self.mouse),
                "graph_keyboard" => self.keyboard = parse_bool(value).unwrap_or(self.keyboard),
                "graph_text" => self.text = parse_bool(value).unwrap_or(self.text),
                "graph_motion" => set_bool(
                    value,
                    &mut options.motion,
                    &mut options_touched,
                    &mut options_error,
                ),
                "graph_wheel" => set_bool(
                    value,
                    &mut options.wheel,
                    &mut options_touched,
                    &mut options_error,
                ),
                "graph_horizontal_wheel" => set_bool(
                    value,
                    &mut options.horizontal_wheel,
                    &mut options_touched,
                    &mut options_error,
                ),
                "graph_button_primary" => set_button(
                    value,
                    0,
                    &mut options,
                    &mut options_touched,
                    &mut options_error,
                ),
                "graph_button_secondary" => set_button(
                    value,
                    1,
                    &mut options,
                    &mut options_touched,
                    &mut options_error,
                ),
                "graph_button_middle" => set_button(
                    value,
                    2,
                    &mut options,
                    &mut options_touched,
                    &mut options_error,
                ),
                "graph_button_back" => set_button(
                    value,
                    3,
                    &mut options,
                    &mut options_touched,
                    &mut options_error,
                ),
                "graph_button_forward" => set_button(
                    value,
                    4,
                    &mut options,
                    &mut options_touched,
                    &mut options_error,
                ),
                "graph_position" => {
                    options_touched = true;
                    options.position = match value.as_str() {
                        "anchored" => PositionMode::Anchored,
                        "source_relative" => PositionMode::SourceRelative,
                        _ => {
                            options_error =
                                Some("Position must be anchored or source_relative.".to_owned());
                            options.position
                        }
                    };
                }
                "graph_offset_x" => set_i32(
                    value,
                    &mut options.offset_x,
                    &mut options_touched,
                    &mut options_error,
                    "X offset",
                ),
                "graph_offset_y" => set_i32(
                    value,
                    &mut options.offset_y,
                    &mut options_touched,
                    &mut options_error,
                    "Y offset",
                ),
                "graph_click_delay_ms" => set_u32(
                    value,
                    &mut options.click_delay_ms,
                    &mut options_touched,
                    &mut options_error,
                    "Click delay",
                ),
                "graph_step_interval_ms" => set_u32(
                    value,
                    &mut options.step_interval_ms,
                    &mut options_touched,
                    &mut options_error,
                    "Step interval",
                ),
                "graph_file_path" | "file_path" => self.file_path = value.clone(),
                "selected_edge" | "graph_selected_edge" => self.selected_edge = value.parse().ok(),
                _ => {}
            }
        }
        if options_touched {
            let validation = match options_error {
                Some(error) => Err(anyhow::anyhow!(error)),
                None => validate_options(options),
            };
            match validation {
                Ok(()) => {
                    self.staged_options = options;
                    self.error = None;
                }
                Err(error) => self.error = Some(format!("{error:#}")),
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
        session_busy: bool,
        automation_active: bool,
        script: Option<&mut Script>,
        action_frame: &mut u64,
        frame_actions: &mut BTreeSet<String>,
    ) -> Option<PageAction> {
        self.refresh_seats(seats);
        let mut script = script;
        let mut action = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            Heading::new("Replication Graph").heading().show(ui);
            Text::new("Build and apply directed routes between virtual users.")
                .caption().wrap().show(ui);
            if seats.is_empty() {
                ui.weak("Add users before building a replication graph.");
                return;
            }

            let fields = script.as_deref().and_then(Script::field_edits).cloned().unwrap_or_default();
            self.apply_script_fields(&fields, seats);
            if !definition_endpoints_active(self.draft(), seats, active_users) {
                self.unresolved = Some("Graph contains a removed virtual replica. Remove or rewire that connection before applying.".into());
            }
            ui.strong(format!("{} draft · {} applied · revision {}",
                self.draft().edges.len(), self.applied.edges.len(), self.revision));
            if let Some(error) = &self.unresolved {
                ui.colored_label(ui.visuals().error_fg_color, format!("Unresolved graph: {error}"));
            }
            if let Some(error) = &self.error { ui.colored_label(ui.visuals().error_fg_color, error); }
            if self.pending.is_some() { ui.weak("Waiting for the matching worker graph acknowledgement."); }
            let queue = &self.replication;
            let queue_inactive = !session_active || !session_ready || queue.suspended;
            ui.horizontal_wrapped(|ui| {
                ui.strong(if queue_inactive { "Queue inactive" } else { "Queue active" });
                Text::new(format!("{} events", queue.pending_events))
                    .wrap()
                    .show(ui);
                Text::new(format!("{} motion", queue.pending_motion))
                    .wrap()
                    .show(ui);
            });
            if queue.suspended {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    "Replication suspended after queue overflow. Apply the graph to resume.",
                );
            }
            if let Some(error) = &queue.error {
                ui.colored_label(ui.visuals().error_fg_color, format!("Replication queue: {error}"));
            }
            let can_apply = self.pending.is_none() && !session_busy && !automation_active && (!session_active || session_ready) && self.unresolved.is_none() && definition_endpoints_active(self.draft(), seats, active_users);
            if primary_button(ui, "Apply replication graph", can_apply, script.as_deref_mut(), action_frame, frame_actions) {
                let candidate = self.draft().clone();
                match candidate.resolve(seats) {
                    Ok(resolved) => {
                        let edges = resolved.into_iter().map(|edge| automation_runtime::Edge { source: edge.source, target: edge.target, mouse: edge.mouse, keyboard: edge.keyboard, text: edge.text, options: edge.options }).collect();
                        self.error = None;
                        if session_active {
                            self.pending = Some(PendingApply { definition: candidate.clone(), expected_revision: self.revision.saturating_add(1), edges });
                        }
                        action = Some(PageAction::Apply(candidate));
                    }
                    Err(error) => self.error = Some(format!("{error:#}")),
                }
            }
            if quiet_button(
                ui,
                "Cancel pending replication",
                session_active && session_ready && !session_busy,
                script.as_deref_mut(),
                action_frame,
                frame_actions,
            ) {
                action = Some(PageAction::CancelReplication {
                    expected_revision: self.revision,
                });
            }

            self.show_canvas(
                ui,
                seats,
                active_users,
                script.as_deref_mut(),
                action_frame,
                frame_actions,
            );

            let queue = &self.replication;
            let queue_diagnostics = Collapsible::new("Queue diagnostics")
                .default_open(false)
                .id_source("replication-queue-diagnostics")
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        Text::new(format!("Pending bytes: {}", queue.pending_bytes))
                            .wrap()
                            .show(ui);
                        Text::new(format!("Event capacity: {}", queue.capacity))
                            .wrap()
                            .show(ui);
                    });
                    ui.horizontal_wrapped(|ui| {
                        Text::new(format!("Byte capacity: {}", queue.byte_capacity))
                            .wrap()
                            .show(ui);
                        Text::new(format!("Emitted: {}", queue.emitted))
                            .wrap()
                            .show(ui);
                    });
                    ui.horizontal_wrapped(|ui| {
                        Text::new(format!("Coalesced: {}", queue.coalesced))
                            .wrap()
                            .show(ui);
                        Text::new(format!("Cancelled: {}", queue.cancelled))
                            .wrap()
                            .show(ui);
                    });
                });
            let _ = script
                .as_deref_mut()
                .is_none_or(|s| s.observe("Queue diagnostics", &queue_diagnostics));

            Divider::horizontal().show(ui);
            let connect_users = Collapsible::new("Connect users")
                .id_source("graph-connect-users")
                .show(ui, |ui| {
                    Heading::new("Connect users").heading().show(ui);
                    let users: Vec<String> = seats.iter().enumerate().map(|(i, seat)| format!("User {}: {}", i + 1, seat.name)).collect();
                    let mut source_index = identity_index(self.selected_source.as_deref(), seats).unwrap_or(0);
                    let mut target_index = identity_index(self.selected_target.as_deref(), seats).unwrap_or(0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label("From"); Select::new(&mut source_index).options(users.clone()).placeholder("Source user").show(ui);
                        ui.label("To"); Select::new(&mut target_index).options(users).placeholder("Destination user").show(ui);
                    });
                    self.selected_source = seats.get(source_index).map(Seat::identity);
                    self.selected_target = seats.get(target_index).map(Seat::identity);
                    ui.horizontal_wrapped(|ui| {
                        self.mouse = checkbox(ui, "Replicate mouse", self.mouse, "graph_mouse", script.as_deref_mut(), action_frame, frame_actions);
                        self.keyboard = checkbox(ui, "Replicate keyboard", self.keyboard, "graph_keyboard", script.as_deref_mut(), action_frame, frame_actions);
                        self.text = checkbox(ui, "Replicate text", self.text, "graph_text", script.as_deref_mut(), action_frame, frame_actions);
                    });
                    let advanced = Collapsible::new("Advanced edge behavior")
                        .id_source("graph-advanced-edge-behavior")
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                self.staged_options.motion = checkbox(ui, "Movement", self.staged_options.motion, "graph_motion", script.as_deref_mut(), action_frame, frame_actions);
                                self.staged_options.wheel = checkbox(ui, "Vertical wheel", self.staged_options.wheel, "graph_wheel", script.as_deref_mut(), action_frame, frame_actions);
                                self.staged_options.horizontal_wheel = checkbox(ui, "Horizontal wheel", self.staged_options.horizontal_wheel, "graph_horizontal_wheel", script.as_deref_mut(), action_frame, frame_actions);
                            });
                            ui.horizontal_wrapped(|ui| {
                                for (index, label) in ["Primary", "Secondary", "Middle", "Back", "Forward"].into_iter().enumerate() {
                                    self.staged_options.buttons[index] = checkbox(ui, &format!("Allow {label}"), self.staged_options.buttons[index], button_field(index), script.as_deref_mut(), action_frame, frame_actions);
                                }
                            });
                            let mut position_index = usize::from(self.staged_options.position == PositionMode::SourceRelative);
                            if Select::new(&mut position_index).options(["Anchored", "Source-relative"].map(str::to_owned)).placeholder("Position behavior").show(ui).changed() {
                                self.staged_options.position = if position_index == 1 { PositionMode::SourceRelative } else { PositionMode::Anchored };
                            }
                            ui.horizontal_wrapped(|ui| {
                                ui.label("Offset X"); ui.add(egui::DragValue::new(&mut self.staged_options.offset_x).range(-32_767..=32_767));
                                ui.label("Offset Y"); ui.add(egui::DragValue::new(&mut self.staged_options.offset_y).range(-32_767..=32_767));
                            });
                            ui.horizontal_wrapped(|ui| {
                                ui.label("Click delay (ms)"); ui.add(egui::DragValue::new(&mut self.staged_options.click_delay_ms).range(0..=60_000));
                                ui.label("Step interval (ms)"); ui.add(egui::DragValue::new(&mut self.staged_options.step_interval_ms).range(0..=1_000));
                            });
                            Text::new("Anchored preserves each destination's anchor. Source-relative offsets use virtual desktop pixels. Step interval paces/coalesces motion; it is not a motion gain.")
                                .caption().wrap().show(ui);
                        });
                    let _ = script.as_deref_mut().is_none_or(|s| s.observe("Advanced edge behavior", &advanced));
                });
            let _ = script.as_deref_mut().is_none_or(|s| s.observe("Connect users", &connect_users));
            if let Err(error) = validate_options(self.staged_options) {
                ui.colored_label(ui.visuals().error_fg_color, format!("Edge options: {error:#}"));
            }
            let selected_users_active = identity_index(self.selected_source.as_deref(), seats)
                .is_some_and(|index| active_users.get(index).copied().unwrap_or(true))
                && identity_index(self.selected_target.as_deref(), seats)
                    .is_some_and(|index| active_users.get(index).copied().unwrap_or(true));
            ui.horizontal_wrapped(|ui| {
                if primary_button(ui, "Connect selected users", selected_users_active && self.selected_source != self.selected_target && (self.mouse || self.keyboard || self.text) && validate_options(self.staged_options).is_ok(), script.as_deref_mut(), action_frame, frame_actions)
                    && let (Some(source), Some(target)) = (&self.selected_source, &self.selected_target)
                {
                    let mut candidate = self.draft().clone();
                    let edge = Edge { source: source.clone(), target: target.clone(), mouse: self.mouse, keyboard: self.keyboard, text: self.text, options: self.staged_options };
                    if candidate.edges.iter().any(|edge| edge.source == *source && edge.target == *target) {
                        self.error = Some("That connection already exists. Select it and use Update selected connection to change its categories.".into());
                    } else { candidate.edges.push(edge); self.accept_resolved(candidate, seats); }
                }
                if quiet_button(ui, "Update selected connection", selected_users_active && self.selected_edge.is_some() && (self.mouse || self.keyboard || self.text) && validate_options(self.staged_options).is_ok(), script.as_deref_mut(), action_frame, frame_actions)
                    && let (Some(index), Some(source), Some(target)) = (self.selected_edge, &self.selected_source, &self.selected_target)
                {
                    let mut candidate = self.draft().clone();
                    if let Some(edge) = candidate.edges.get_mut(index) {
                        *edge = Edge { source: source.clone(), target: target.clone(), mouse: self.mouse, keyboard: self.keyboard, text: self.text, options: self.staged_options };
                        self.accept_resolved(candidate, seats);
                    }
                }
            });

            let connections = Collapsible::new("Connections")
                .id_source("graph-connections")
                .show(ui, |ui| {
            let mut remove = None;
            let displayed_edges = self.draft().edges.clone();
            egui::Grid::new("replication-edge-list").striped(true).show(ui, |ui| {
                for (index, edge) in displayed_edges.iter().enumerate() {
                    let source = seat_name(&edge.source, seats);
                    let target = seat_name(&edge.target, seats);
                    let label = format!("Graph connection {}", index + 1);
                    let response = Button::new(format!("{source} → {target}"))
                        .variant(if self.selected_edge == Some(index) { ButtonVariant::Secondary } else { ButtonVariant::Outline })
                        .id_source(("graph-edge", index)).show(ui);
                    let accepted = script.as_deref_mut().is_none_or(|s| s.observe(&label, &response));
                    if accepted && response.clicked() && consume(action_frame, frame_actions, ui.ctx(), &label) { self.select_edge(index); }
                    ui.label(edge_summary(edge));
                    ui.end_row();
                }
            });
            ui.horizontal_wrapped(|ui| {
                if button(ui, "Remove selected connection", self.selected_edge.is_some(), script.as_deref_mut(), action_frame, frame_actions) { remove = self.selected_edge; }
                if button(ui, "Clear replication graph", !self.draft().edges.is_empty(), script.as_deref_mut(), action_frame, frame_actions) { self.replace_structural(Definition::default()); }
            });
            if let Some(index) = remove { let mut candidate = self.draft().clone(); if index < candidate.edges.len() { candidate.edges.remove(index); self.replace_structural(candidate); self.selected_edge = None; } }
                });
            let _ = script.as_deref_mut().is_none_or(|s| s.observe("Connections", &connections));

            let tools = Collapsible::new("Graph tools")
                .id_source("graph-tools")
                .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if button(ui, "Undo graph edit", self.undo_count > 0, script.as_deref_mut(), action_frame, frame_actions) {
                    if let Some(history) = &mut self.history { history.undo(); }
                    self.undo_count = self.undo_count.saturating_sub(1); self.redo_count = self.redo_count.saturating_add(1);
                    self.refresh_seats(seats);
                }
                if button(ui, "Redo graph edit", self.redo_count > 0, script.as_deref_mut(), action_frame, frame_actions) {
                    if let Some(history) = &mut self.history { history.redo(); }
                    self.redo_count = self.redo_count.saturating_sub(1); self.undo_count = self.undo_count.saturating_add(1).min(mux_core::replication::MAX_HISTORY);
                    self.refresh_seats(seats);
                }
            });
            Heading::new("Graph files").heading().show(ui);
            Input::new(&mut self.file_path).id_source("replication-file-path").show(ui);
            ui.horizontal_wrapped(|ui| {
                if button(ui, "Save graph JSON", true, script.as_deref_mut(), action_frame, frame_actions) {
                    match self.draft().to_json().and_then(|json| write_bounded(&self.file_path, json.as_bytes())) {
                        Ok(()) => self.error = None,
                        Err(error) => self.error = Some(format!("{error:#}")),
                    }
                }
                if button(ui, "Load graph JSON", self.pending.is_none(), script.as_deref_mut(), action_frame, frame_actions) {
                    match read_graph(&self.file_path).and_then(|candidate| { candidate.resolve(seats)?; Ok(candidate) }) {
                        Ok(candidate) if definition_endpoints_active(&candidate, seats, active_users) => self.accept_resolved(candidate, seats),
                        Ok(_) => self.error = Some("Graph JSON includes a removed virtual replica.".into()),
                        Err(error) => self.error = Some(format!("{error:#}")),
                    }
                }
            });
            Text::new("The graph routes virtual input inside this session. Generated replicas are session-only and cannot be restored after stopping the session.")
                .caption().wrap().show(ui);
                });
            let _ = script.is_none_or(|s| s.observe("Graph tools", &tools));
        });
        action
    }

    fn draft(&self) -> &Definition {
        self.history
            .as_ref()
            .map_or(&self.profile, History::current)
    }

    fn accept_resolved(&mut self, candidate: Definition, seats: &[Seat]) {
        if let Err(error) = candidate.resolve(seats) {
            self.error = Some(format!("{error:#}"));
            return;
        }
        self.replace_structural(candidate);
    }

    fn replace_structural(&mut self, candidate: Definition) {
        let changed = self.draft() != &candidate;
        match self
            .history
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("graph edit history is unavailable"))
            .and_then(|history| history.replace(candidate))
        {
            Ok(()) => {
                self.error = None;
                self.selected_edge = None;
                if changed {
                    self.undo_count = self
                        .undo_count
                        .saturating_add(1)
                        .min(mux_core::replication::MAX_HISTORY);
                    self.redo_count = 0;
                }
            }
            Err(error) => self.error = Some(format!("{error:#}")),
        }
    }

    fn select_edge(&mut self, index: usize) {
        let Some(edge) = self.draft().edges.get(index).cloned() else {
            return;
        };
        self.selected_edge = Some(index);
        self.selected_source = Some(edge.source);
        self.selected_target = Some(edge.target);
        self.mouse = edge.mouse;
        self.keyboard = edge.keyboard;
        self.text = edge.text;
        self.staged_options = edge.options;
    }

    fn show_canvas(
        &mut self,
        ui: &mut egui::Ui,
        seats: &[Seat],
        active_users: &[bool],
        mut script: Option<&mut Script>,
        action_frame: &mut u64,
        frame_actions: &mut BTreeSet<String>,
    ) {
        let desired = Vec2::new(ui.available_width().max(360.0), 220.0);
        let (rect, painter) = ui.allocate_painter(desired, Sense::click());
        painter.rect_filled(rect.rect, 8.0, Color32::from_rgb(20, 29, 43));
        painter.rect_stroke(
            rect.rect,
            8.0,
            Stroke::new(1.0_f32, Color32::from_rgb(62, 78, 101)),
            egui::StrokeKind::Outside,
        );
        let canvas = rect.rect.shrink(30.0);
        let mut points = BTreeMap::new();
        for (index, seat) in seats.iter().enumerate() {
            let id = seat.identity();
            let default = default_position(index, seats.len());
            let normalized = *self.positions.entry(id.clone()).or_insert(default);
            points.insert(
                id,
                Pos2::new(
                    canvas.left() + normalized[0] * canvas.width(),
                    canvas.top() + normalized[1] * canvas.height(),
                ),
            );
        }
        let displayed_edges = self.draft().edges.clone();
        for (index, edge) in displayed_edges.iter().enumerate() {
            if let (Some(from), Some(to)) = (points.get(&edge.source), points.get(&edge.target)) {
                let color = category_color(edge);
                painter.line_segment(
                    [*from, *to],
                    Stroke::new(
                        if self.selected_edge == Some(index) {
                            4.0_f32
                        } else {
                            2.0_f32
                        },
                        color,
                    ),
                );
                let direction = (*to - *from).normalized();
                let clearance = (65.0 / direction.x.abs().max(f32::EPSILON))
                    .min(27.0 / direction.y.abs().max(f32::EPSILON));
                let tip = *to - direction * (clearance + 3.0);
                let base = tip - direction * 10.0;
                let perpendicular = Vec2::new(-direction.y, direction.x) * 6.0;
                painter.line_segment([tip, base + perpendicular], Stroke::new(2.0_f32, color));
                painter.line_segment([tip, base - perpendicular], Stroke::new(2.0_f32, color));
                painter.text(
                    from.lerp(*to, 0.5),
                    egui::Align2::CENTER_CENTER,
                    category_label(edge),
                    egui::FontId::proportional(11.0),
                    color,
                );
                // Place the selectable label away from central crossings in fan-out diagrams.
                let midpoint = from.lerp(*to, 0.4);
                painter.circle_filled(midpoint, 4.0, color);
                let hitbox = Rect::from_center_size(midpoint, Vec2::splat(24.0));
                let wire_response = ui.interact(
                    hitbox,
                    ui.id().with(("replication-wire", index)),
                    Sense::click(),
                );
                let label = format!("Graph wire {}", index + 1);
                let accepted = script
                    .as_deref_mut()
                    .is_none_or(|script| script.observe(&label, &wire_response));
                if wire_response.clicked()
                    && accepted
                    && consume(action_frame, frame_actions, ui.ctx(), &label)
                {
                    self.select_edge(index);
                }
            }
        }
        for (index, seat) in seats.iter().enumerate() {
            let id = seat.identity();
            let Some(center) = points.get(&id).copied() else {
                continue;
            };
            let node = Rect::from_center_size(center, Vec2::new(130.0, 54.0));
            let user_active = active_users.get(index).copied().unwrap_or(true);
            let response = ui.interact(
                node,
                ui.id().with(("replication-node", id.clone())),
                if user_active {
                    Sense::click_and_drag()
                } else {
                    Sense::click()
                },
            );
            if user_active && response.dragged() {
                let next = (center + response.drag_delta()).clamp(
                    canvas.min + Vec2::splat(65.0),
                    canvas.max - Vec2::splat(65.0),
                );
                self.positions.insert(
                    id.clone(),
                    [
                        ((next.x - canvas.left()) / canvas.width()).clamp(0.0, 1.0),
                        ((next.y - canvas.top()) / canvas.height()).clamp(0.0, 1.0),
                    ],
                );
            }
            let label = format!("Graph user {}", index + 1);
            let accepted = script
                .as_deref_mut()
                .is_none_or(|script| script.observe(&label, &response));
            if response.clicked()
                && accepted
                && consume(action_frame, frame_actions, ui.ctx(), &label)
            {
                if self.source_stage.is_none() {
                    self.source_stage = Some(id.clone());
                    self.selected_source = Some(id.clone());
                } else if self.source_stage.as_deref() != Some(id.as_str()) {
                    self.selected_source = self.source_stage.take();
                    self.selected_target = Some(id.clone());
                } else {
                    self.source_stage = None;
                }
            }
            let color = if user_active {
                seat_color(index)
            } else {
                Color32::DARK_GRAY
            };
            painter.rect_filled(node, 8.0, color);
            painter.rect_stroke(
                node,
                8.0,
                Stroke::new(
                    2.0_f32,
                    if self.selected_source.as_deref() == Some(id.as_str()) {
                        Color32::WHITE
                    } else {
                        color.gamma_multiply(0.7)
                    },
                ),
                egui::StrokeKind::Outside,
            );
            painter.text(
                node.center(),
                egui::Align2::CENTER_CENTER,
                format!(
                    "{} {}\n{}",
                    if user_active { "User" } else { "Removed" },
                    index + 1,
                    seat.name
                ),
                egui::FontId::proportional(14.0),
                Color32::WHITE,
            );
        }
        ui.label("Click one user, then another to choose a directed source and destination. Drag nodes to arrange the graph; click a wire or choose it from Connections.");
    }
}

fn checkbox(
    ui: &mut egui::Ui,
    label: &str,
    mut checked: bool,
    id: &str,
    script: Option<&mut Script>,
    action_frame: &mut u64,
    frame_actions: &mut BTreeSet<String>,
) -> bool {
    let original = checked;
    let response = ouroboros_ui::atoms::Checkbox::new(&mut checked)
        .label(label)
        .id_source(id)
        .show(ui);
    let accepted = script.is_none_or(|script| script.observe(label, &response));
    if response.clicked() && accepted && consume(action_frame, frame_actions, ui.ctx(), label) {
        checked
    } else {
        original
    }
}

fn set_bool(value: &str, target: &mut bool, touched: &mut bool, error: &mut Option<String>) {
    *touched = true;
    if let Some(value) = parse_bool(value) {
        *target = value;
    } else if error.is_none() {
        *error = Some("Boolean edge settings must be true or false.".into());
    }
}

fn set_button(
    value: &str,
    index: usize,
    options: &mut EdgeOptions,
    touched: &mut bool,
    error: &mut Option<String>,
) {
    set_bool(value, &mut options.buttons[index], touched, error);
}

fn set_i32(
    value: &str,
    target: &mut i32,
    touched: &mut bool,
    error: &mut Option<String>,
    label: &str,
) {
    *touched = true;
    match value.parse() {
        Ok(value) => *target = value,
        Err(_) if error.is_none() => *error = Some(format!("{label} must be an integer.")),
        Err(_) => {}
    }
}

fn set_u32(
    value: &str,
    target: &mut u32,
    touched: &mut bool,
    error: &mut Option<String>,
    label: &str,
) {
    *touched = true;
    match value.parse() {
        Ok(value) => *target = value,
        Err(_) if error.is_none() => {
            *error = Some(format!("{label} must be a nonnegative integer."));
        }
        Err(_) => {}
    }
}

fn validate_options(options: EdgeOptions) -> anyhow::Result<()> {
    options.validate()
}

fn button_field(index: usize) -> &'static str {
    [
        "graph_button_primary",
        "graph_button_secondary",
        "graph_button_middle",
        "graph_button_back",
        "graph_button_forward",
    ][index]
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
        .id_source(("replication", label))
        .show(ui);
    if !script.is_none_or(|script| script.observe(label, &response)) || !response.clicked() {
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
        .id_source(("replication_primary", label))
        .show(ui);
    if !script.is_none_or(|script| script.observe(label, &response)) || !response.clicked() {
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
    let response = Button::new(label)
        .variant(ButtonVariant::Outline)
        .enabled(enabled)
        .show(ui);
    let accepted = script.is_none_or(|script| script.observe(label, &response));
    response.clicked() && accepted && consume(action_frame, frame_actions, ui.ctx(), label)
}

fn select_identity(value: &str, seats: &[Seat]) -> Option<String> {
    value
        .parse::<usize>()
        .ok()
        .and_then(|index| seats.get(index).map(Seat::identity))
        .or_else(|| {
            seats
                .iter()
                .find(|seat| seat.identity() == value || seat.name == value)
                .map(Seat::identity)
        })
}
fn identity_index(id: Option<&str>, seats: &[Seat]) -> Option<usize> {
    id.and_then(|id| seats.iter().position(|seat| seat.identity() == id))
}
fn seat_name(id: &str, seats: &[Seat]) -> String {
    seats
        .iter()
        .position(|seat| seat.identity() == id)
        .map(|index| format!("User {}: {}", index + 1, seats[index].name))
        .unwrap_or_else(|| format!("Missing user ({})", id.chars().take(16).collect::<String>()))
}
fn category_label(edge: &Edge) -> String {
    [
        edge.mouse.then_some("Mouse"),
        edge.keyboard.then_some("Keyboard"),
        edge.text.then_some("Text"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}
fn edge_summary(edge: &Edge) -> String {
    let mut details = Vec::new();
    if !edge.options.motion {
        details.push("no movement".to_owned());
    }
    if !edge.options.wheel {
        details.push("no vertical wheel".to_owned());
    }
    if !edge.options.horizontal_wheel {
        details.push("no horizontal wheel".to_owned());
    }
    if edge.options.buttons != [true; 5] {
        let names = ["P", "S", "M", "Back", "Forward"];
        let allowed = edge
            .options
            .buttons
            .iter()
            .zip(names)
            .filter_map(|(allowed, name)| allowed.then_some(name))
            .collect::<Vec<_>>();
        details.push(if allowed.is_empty() {
            "no buttons".into()
        } else {
            format!("buttons {}", allowed.join("/"))
        });
    }
    if edge.options.position == PositionMode::SourceRelative {
        details.push(format!(
            "from source {:+},{:+}",
            edge.options.offset_x, edge.options.offset_y
        ));
    }
    if edge.options.click_delay_ms > 0 {
        details.push(format!("click {} ms", edge.options.click_delay_ms));
    }
    if edge.options.step_interval_ms > 0 {
        details.push(format!("step {} ms", edge.options.step_interval_ms));
    }
    if details.is_empty() {
        category_label(edge)
    } else {
        format!("{} · {}", category_label(edge), details.join(" · "))
    }
}

fn category_color(edge: &Edge) -> Color32 {
    if edge.keyboard || edge.text {
        Color32::from_rgb(123, 174, 255)
    } else {
        Color32::from_rgb(91, 220, 182)
    }
}
fn seat_color(index: usize) -> Color32 {
    const COLORS: [[u8; 3]; 6] = [
        [42, 112, 150],
        [118, 78, 160],
        [49, 135, 105],
        [173, 101, 44],
        [157, 67, 88],
        [68, 111, 177],
    ];
    let c = COLORS[index % COLORS.len()];
    Color32::from_rgb(c[0], c[1], c[2])
}
fn default_position(index: usize, count: usize) -> [f32; 2] {
    let angle = std::f32::consts::TAU * index as f32 / count.max(1) as f32;
    [0.5 + 0.38 * angle.cos(), 0.5 + 0.38 * angle.sin()]
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
fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
}
fn read_graph(path: &str) -> anyhow::Result<Definition> {
    let metadata = fs::metadata(path)?;
    anyhow::ensure!(
        metadata.len() as usize <= MAX_FILE_BYTES,
        "replication JSON exceeds 4 MiB"
    );
    Definition::from_json(&fs::read_to_string(path)?)
}
fn write_bounded(path: &str, bytes: &[u8]) -> anyhow::Result<()> {
    anyhow::ensure!(
        bytes.len() <= MAX_FILE_BYTES,
        "replication JSON exceeds 4 MiB"
    );
    let path = Path::new(path);
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seats() -> Vec<Seat> {
        vec![
            Seat {
                id: "source".into(),
                name: "Source".into(),
                mouse: "mouse-source".into(),
                keyboard: Some("keyboard-source".into()),
                target_pid: 1,
                desktop_start: None,
                cursor: Default::default(),
                annotation: Default::default(),
                magnifier: Default::default(),
                output: Default::default(),
            },
            Seat {
                id: "target".into(),
                name: "Target".into(),
                mouse: "mouse-target".into(),
                keyboard: Some("keyboard-target".into()),
                target_pid: 2,
                desktop_start: None,
                cursor: Default::default(),
                annotation: Default::default(),
                magnifier: Default::default(),
                output: Default::default(),
            },
        ]
    }

    fn candidate() -> Definition {
        Definition {
            edges: vec![Edge {
                source: "source".into(),
                target: "target".into(),
                mouse: true,
                keyboard: false,
                text: false,
                options: EdgeOptions::default(),
            }],
            ..Default::default()
        }
    }

    fn reverse_candidate() -> Definition {
        Definition {
            edges: vec![Edge {
                source: "target".into(),
                target: "source".into(),
                mouse: true,
                keyboard: false,
                text: false,
                options: EdgeOptions::default(),
            }],
            ..Default::default()
        }
    }

    fn stage_apply(page: &mut ReplicationPage, seats: &[Seat], expected_revision: u64) {
        let definition = candidate();
        page.accept_resolved(definition.clone(), seats);
        let edges = definition
            .resolve(seats)
            .unwrap()
            .into_iter()
            .map(|edge| automation_runtime::Edge {
                source: edge.source,
                target: edge.target,
                mouse: edge.mouse,
                keyboard: edge.keyboard,
                text: edge.text,
                options: edge.options,
            })
            .collect();
        page.pending = Some(PendingApply {
            definition,
            expected_revision,
            edges,
        });
    }

    #[test]
    fn external_ack_updates_actual_graph_without_consuming_local_pending_or_editor() {
        let seats = seats();
        let mut page = ReplicationPage::default();
        stage_apply(&mut page, &seats, 1);
        page.error = Some("local editor feedback".into());
        page.file_path = "unsaved-local-path.json".into();
        let state = automation_runtime::State {
            acknowledged: true,
            graph_revision: 1,
            edges: page.pending.as_ref().unwrap().edges.clone(),
            ..Default::default()
        };

        assert_eq!(page.receive_external(&state, &seats), Some(candidate()));
        assert!(page.snapshot().pending);
        assert_eq!(page.draft(), &candidate());
        assert_eq!(page.file_path, "unsaved-local-path.json");
        assert_eq!(page.error.as_deref(), Some("local editor feedback"));
        assert!(page.snapshot().undo_available);
        assert_eq!(page.receive(&state, &seats), Some(candidate()));
        assert!(!page.snapshot().pending);
    }

    #[test]
    fn newer_external_graph_survives_delayed_local_ack_and_stale_telemetry() {
        let seats = seats();
        let mut page = ReplicationPage::default();
        stage_apply(&mut page, &seats, 1);
        let local_ack = automation_runtime::State {
            acknowledged: true,
            graph_revision: 1,
            edges: page.pending.as_ref().unwrap().edges.clone(),
            ..Default::default()
        };
        let actual = reverse_candidate();
        let newer = automation_runtime::State {
            acknowledged: true,
            graph_revision: 2,
            edges: actual.resolve(&seats).unwrap(),
            ..Default::default()
        };

        assert_eq!(page.receive_external(&newer, &seats), Some(actual.clone()));
        assert!(page.snapshot().pending);
        assert_eq!(page.draft(), &candidate());
        assert!(page.receive(&local_ack, &seats).is_none());
        assert!(!page.snapshot().pending);
        assert!(page.snapshot().error.is_some());
        assert!(page.receive_external(&local_ack, &seats).is_none());
        assert_eq!(page.applied, actual);
        assert_eq!(page.profile, actual);
        assert_eq!(page.revision, 2);
        assert_eq!(page.draft(), &candidate());
    }

    #[test]
    fn external_graph_refreshes_clean_draft_but_preserves_unsent_local_history() {
        let seats = seats();
        let mut page = ReplicationPage::default();
        let mut state = automation_runtime::State {
            graph_revision: 1,
            edges: candidate().resolve(&seats).unwrap(),
            ..Default::default()
        };
        assert_eq!(page.receive_external(&state, &seats), Some(candidate()));
        assert_eq!(page.draft(), &candidate());
        page.accept_resolved(reverse_candidate(), &seats);
        state.graph_revision = 2;
        state.edges.clear();
        state.error = Some("other client's command failed".into());
        assert_eq!(
            page.receive_external(&state, &seats),
            Some(Definition::default())
        );
        assert_eq!(page.applied, Definition::default());
        assert_eq!(page.draft(), &reverse_candidate());
        assert!(page.snapshot().undo_available);
        assert!(page.error.is_none());
    }

    #[test]
    fn roster_graph_pruning_keeps_pending_draft_and_history() {
        let seats = seats();
        let mut page = ReplicationPage::default();
        stage_apply(&mut page, &seats, 1);
        page.sync_applied(&reverse_candidate());
        assert!(page.snapshot().pending);
        assert_eq!(page.applied, reverse_candidate());
        assert_eq!(page.draft(), &candidate());
        assert!(page.snapshot().undo_available);
    }

    #[test]
    fn pending_apply_requires_exact_revision_and_matching_edges() {
        let seats = seats();
        let mut page = ReplicationPage::default();
        page.reset_profile(&mux_core::Profile {
            version: 1,
            concurrent: false,
            desktop: None,
            replication: Definition::default(),
            forwarding: Default::default(),
            mapper: Default::default(),
            seats: seats.clone(),
        });
        stage_apply(&mut page, &seats, 4);

        let old_ack = automation_runtime::State {
            acknowledged: true,
            graph_revision: 3,
            edges: page.pending.as_ref().unwrap().edges.clone(),
            ..Default::default()
        };
        assert!(page.receive(&old_ack, &seats).is_none());
        assert!(page.snapshot().pending);
        assert_eq!(page.applied, Definition::default());

        let mut newer_page = ReplicationPage::default();
        newer_page.reset_profile(&mux_core::Profile {
            version: 1,
            concurrent: false,
            desktop: None,
            replication: Definition::default(),
            forwarding: Default::default(),
            mapper: Default::default(),
            seats: seats.clone(),
        });
        stage_apply(&mut newer_page, &seats, 4);
        let newer_ack = automation_runtime::State {
            acknowledged: true,
            graph_revision: 5,
            edges: newer_page.pending.as_ref().unwrap().edges.clone(),
            ..Default::default()
        };
        assert!(newer_page.receive(&newer_ack, &seats).is_none());
        assert!(!newer_page.snapshot().pending);
        assert_eq!(newer_page.applied, Definition::default());

        let matching_ack = automation_runtime::State {
            acknowledged: true,
            graph_revision: 4,
            edges: page.pending.as_ref().unwrap().edges.clone(),
            ..Default::default()
        };
        assert_eq!(page.receive(&matching_ack, &seats), Some(candidate()));
        assert!(!page.snapshot().pending);
        assert_eq!(page.applied, candidate());
    }

    #[test]
    fn worker_graph_sync_propagates_managed_definition_without_discarding_staged_draft() {
        let seats = seats();
        let mut page = ReplicationPage::default();
        page.reset_profile(&mux_core::Profile {
            version: 1,
            concurrent: false,
            desktop: None,
            replication: Definition::default(),
            forwarding: Default::default(),
            mapper: Default::default(),
            seats: seats.clone(),
        });
        let staged = reverse_candidate();
        page.accept_resolved(staged.clone(), &seats);

        let actual = candidate();
        let edges = actual
            .resolve(&seats)
            .unwrap()
            .into_iter()
            .map(|edge| automation_runtime::Edge {
                source: edge.source,
                target: edge.target,
                mouse: edge.mouse,
                keyboard: edge.keyboard,
                text: edge.text,
                options: edge.options,
            })
            .collect();
        let telemetry = automation_runtime::State {
            acknowledged: false,
            graph_revision: 3,
            edges,
            ..Default::default()
        };

        assert_eq!(page.receive(&telemetry, &seats), Some(actual.clone()));
        assert_eq!(page.applied, actual);
        assert_eq!(page.draft(), &staged);
        assert!(page.snapshot().undo_available);
    }

    #[test]
    fn failed_apply_keeps_draft_applied_graph_and_undo_history() {
        let seats = seats();
        let mut page = ReplicationPage::default();
        page.reset_profile(&mux_core::Profile {
            version: 1,
            concurrent: false,
            desktop: None,
            replication: Definition::default(),
            forwarding: Default::default(),
            mapper: Default::default(),
            seats: seats.clone(),
        });
        stage_apply(&mut page, &seats, 1);
        let failed_ack = automation_runtime::State {
            acknowledged: true,
            graph_revision: 0,
            error: Some("apply rejected".into()),
            ..Default::default()
        };

        assert!(page.receive(&failed_ack, &seats).is_none());
        assert!(!page.snapshot().pending);
        assert_eq!(page.draft(), &candidate());
        assert_eq!(page.applied, Definition::default());
        assert!(page.snapshot().undo_available);
        assert_eq!(page.snapshot().error.as_deref(), Some("apply rejected"));
    }

    #[test]
    fn invalid_advanced_options_do_not_change_staged_or_committed_graph() {
        let seats = seats();
        let mut page = ReplicationPage::default();
        page.reset_profile(&mux_core::Profile {
            version: 1,
            concurrent: false,
            desktop: None,
            replication: Definition::default(),
            forwarding: Default::default(),
            mapper: Default::default(),
            seats: seats.clone(),
        });
        let original = page.staged_options;
        let fields = BTreeMap::from([
            ("graph_position".into(), "anchored".into()),
            ("graph_offset_x".into(), "12".into()),
            ("graph_click_delay_ms".into(), "60001".into()),
        ]);

        page.apply_script_fields(&fields, &seats);

        assert_eq!(page.staged_options, original);
        assert_eq!(page.draft(), &Definition::default());
        assert!(page.snapshot().error.is_some());
    }
}
