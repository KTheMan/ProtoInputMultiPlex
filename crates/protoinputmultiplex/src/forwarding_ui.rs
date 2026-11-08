//! Compact, stable-ID editor for automatic virtual input forwarding.
use crate::{automation_runtime, ui_script::Script};
use eframe::egui;
use mux_core::{
    Seat, emulated,
    forwarding::{Definition, Edge, ResolvedEdge},
};
use ouroboros_ui::{
    atoms::{Button, ButtonVariant, Checkbox, Heading, Text},
    cells::MenuItem,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
struct Pending {
    definition: Definition,
    edges: Vec<ResolvedEdge>,
    revision: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub draft: Definition,
    pub applied: Definition,
    pub revision: u64,
    pub pending: bool,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct ForwardingPage {
    draft: Definition,
    applied: Definition,
    revision: u64,
    pending: Option<Pending>,
    error: Option<String>,
}

impl ForwardingPage {
    pub fn has_profile_draft(&self) -> bool {
        self.pending.is_some() || self.draft != self.applied
    }
    pub fn reset_profile(&mut self, profile: &mux_core::Profile) {
        self.draft = profile.forwarding.clone();
        self.applied = self.draft.clone();
        self.revision = 0;
        self.pending = None;
        self.error = None;
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            draft: self.draft.clone(),
            applied: self.applied.clone(),
            revision: self.revision,
            pending: self.pending.is_some(),
            error: self.error.clone(),
        }
    }

    pub fn stage(&mut self, source: &str, target: Option<&str>, keyboard: bool) {
        self.draft.edges.retain(|edge| edge.source != source);
        if let Some(target) = target {
            self.draft.edges.push(Edge {
                source: source.into(),
                target: target.into(),
                keyboard,
            });
        }
        self.error = None;
    }

    pub fn begin_apply(
        &mut self,
        seats: &[Seat],
        live: bool,
    ) -> anyhow::Result<(Definition, Vec<ResolvedEdge>)> {
        let definition = self.draft.clone();
        let edges = definition.resolve(seats)?;
        if live {
            self.pending = Some(Pending {
                definition: definition.clone(),
                edges: edges.clone(),
                revision: self.revision.saturating_add(1),
            });
        }
        self.error = None;
        Ok((definition, edges))
    }

    pub fn applied_local(&mut self, definition: Definition) {
        self.applied = definition;
        self.pending = None;
        self.error = None;
    }

    pub fn failed_apply(&mut self, error: impl Into<String>) {
        self.pending = None;
        self.error = Some(error.into());
    }

    pub fn session_stopped(&mut self) {
        self.pending = None;
        self.revision = 0;
    }

    /// A matching worker acknowledgement is the only live Apply receipt.
    pub fn receive(
        &mut self,
        state: &automation_runtime::State,
        seats: &[Seat],
    ) -> Option<Definition> {
        if state.forwarding_revision < self.revision {
            return None;
        }
        self.revision = state.forwarding_revision;
        if let Some(pending) = self.pending.clone() {
            if state.acknowledged && state.forwarding_revision == pending.revision {
                self.pending = None;
                if state.error.is_none() && state.forwarding_edges == pending.edges {
                    self.applied = pending.definition.clone();
                    self.error = None;
                    return Some(pending.definition);
                }
                self.error = state
                    .error
                    .clone()
                    .or_else(|| Some("Worker acknowledged different forwarding routes.".into()));
            } else if state.acknowledged
                && (state.error.is_some() || state.forwarding_revision > pending.revision)
            {
                self.pending = None;
                self.error = state
                    .error
                    .clone()
                    .or_else(|| Some("Forwarding changed before Apply was acknowledged.".into()));
            }
            return None;
        }
        let actual = Definition::from_resolved(seats, &state.forwarding_edges).ok()?;
        self.sync_applied(&actual);
        Some(actual)
    }

    pub fn receive_external(
        &mut self,
        state: &automation_runtime::State,
        seats: &[Seat],
    ) -> Option<Definition> {
        if state.forwarding_revision < self.revision {
            return None;
        }
        let actual = Definition::from_resolved(seats, &state.forwarding_edges).ok()?;
        self.revision = state.forwarding_revision;
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| state.forwarding_revision >= pending.revision)
        {
            self.pending = None;
            self.error = Some("Input forwarding changed in another client. Inspect the applied routes before retrying.".into());
        }
        self.sync_applied(&actual);
        Some(actual)
    }

    fn sync_applied(&mut self, actual: &Definition) {
        if self.draft == self.applied {
            self.draft = actual.clone();
        }
        self.applied = actual.clone();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        seats: &[Seat],
        active: &[bool],
        automatic: bool,
        live: bool,
        ready: bool,
        busy: bool,
        replication_active: bool,
        automation_active: bool,
        mut script: Option<&mut Script>,
        action_frame: &mut u64,
        frame_actions: &mut BTreeSet<String>,
    ) -> bool {
        Heading::new("Input Forwarding").heading().show(ui);
        Text::new("Route one virtual user's pointer into another. A forwarded source is hidden while routing. Keyboard follows only when selected.")
            .caption().wrap().show(ui);
        if !automatic {
            ui.weak("Available in automatic desktop routing with virtual users.");
        }
        if replication_active {
            ui.weak("Clear replication before applying input forwarding.");
        }
        ui.strong(format!(
            "{} draft routes · {} applied · revision {}",
            self.draft.edges.len(),
            self.applied.edges.len(),
            self.revision
        ));
        if self.draft != self.applied {
            ui.weak("Unapplied changes");
        }
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if self.pending.is_some() {
            ui.weak("Waiting for worker acknowledgement.");
        }
        let can_edit = automatic && !busy && self.pending.is_none() && (!live || ready);
        for (index, source) in seats.iter().enumerate() {
            let source_id = source.identity();
            let source_virtual = virtual_mouse(&source.mouse)
                && source.keyboard.as_deref().is_none_or(virtual_keyboard);
            let source_active = active.get(index).copied().unwrap_or(false);
            if !source_virtual || !source_active {
                ui.horizontal_wrapped(|ui| {
                    ui.label(&source.name);
                    ui.weak(if source_active {
                        "Physical device: forwarding unavailable"
                    } else {
                        "User unavailable"
                    });
                });
                continue;
            }
            let current = self
                .draft
                .edges
                .iter()
                .find(|edge| edge.source == source_id)
                .cloned();
            let cycle_targets = cycle_targets(&self.draft, &source_id);
            let target_name = current
                .as_ref()
                .and_then(|edge| seats.iter().find(|seat| seat.identity() == edge.target))
                .map_or("Local", |seat| seat.name.as_str());
            let trigger_label = format!("Forward {} to", source.name);
            ui.horizontal_wrapped(|ui| {
                ui.label(&source.name);
                let response = Button::new(target_name)
                    .icon_right(ouroboros_ui::egui_phosphor::light::CARET_DOWN)
                    .variant(ButtonVariant::Outline)
                    .enabled(can_edit)
                    .show(ui);
                if let Some(script) = script.as_deref_mut() {
                    script.observe(&trigger_label, &response);
                }
                let ctx = ui.ctx().clone();
                egui::Popup::menu(&response).show(|menu| {
                    let local = format!("{} to Local", source.name);
                    let option = MenuItem::new("Local input")
                        .checked(current.is_none())
                        .id_source((&source_id, "local"))
                        .show(menu);
                    let accepted = script
                        .as_deref_mut()
                        .is_none_or(|script| script.observe(&local, &option));
                    if can_edit
                        && option.clicked()
                        && accepted
                        && consume(&ctx, &local, action_frame, frame_actions)
                    {
                        self.stage(&source_id, None, false);
                        menu.close();
                    }
                    for (target_index, target) in seats.iter().enumerate() {
                        if target_index == index
                            || !active.get(target_index).copied().unwrap_or(false)
                            || !virtual_mouse(&target.mouse)
                            || !target.keyboard.as_deref().is_none_or(virtual_keyboard)
                        {
                            continue;
                        }
                        let target_id = target.identity();
                        if cycle_targets.contains(&target_id)
                            && current.as_ref().is_none_or(|edge| edge.target != target_id)
                        {
                            continue;
                        }
                        let label = format!("{} to {}", source.name, target.name);
                        let option = MenuItem::new(target.name.as_str())
                            .checked(
                                current
                                    .as_ref()
                                    .is_some_and(|edge| edge.target == target_id),
                            )
                            .id_source((&source_id, &target_id))
                            .show(menu);
                        let accepted = script
                            .as_deref_mut()
                            .is_none_or(|script| script.observe(&label, &option));
                        if can_edit
                            && option.clicked()
                            && accepted
                            && consume(&ctx, &label, action_frame, frame_actions)
                        {
                            self.stage(
                                &source_id,
                                Some(&target_id),
                                current.as_ref().is_some_and(|edge| edge.keyboard)
                                    && target.keyboard.is_some(),
                            );
                            menu.close();
                        }
                    }
                });
                let keyboard_allowed = current
                    .as_ref()
                    .and_then(|edge| seats.iter().find(|seat| seat.identity() == edge.target))
                    .is_some_and(|target| target.keyboard.as_deref().is_some_and(virtual_keyboard))
                    && source.keyboard.as_deref().is_some_and(virtual_keyboard);
                let mut keyboard = current.as_ref().is_some_and(|edge| edge.keyboard);
                let label = format!("Forward keyboard too for {}", source.name);
                let checkbox = Checkbox::new(&mut keyboard)
                    .label("Keyboard too")
                    .enabled(can_edit && keyboard_allowed && current.is_some())
                    .id_source(("forwarding-keyboard", &source_id))
                    .show(ui);
                let accepted = script
                    .as_deref_mut()
                    .is_none_or(|script| script.observe(&label, &checkbox));
                if can_edit
                    && keyboard_allowed
                    && current.is_some()
                    && checkbox.changed()
                    && accepted
                    && consume(&ctx, &label, action_frame, frame_actions)
                    && let Some(edge) = self
                        .draft
                        .edges
                        .iter_mut()
                        .find(|edge| edge.source == source_id)
                {
                    edge.keyboard = keyboard;
                }
            });
        }
        let validation = self.draft.resolve(seats).and_then(|edges| {
            anyhow::ensure!(
                edges
                    .iter()
                    .all(|edge| active.get(edge.source) == Some(&true)
                        && active.get(edge.target) == Some(&true)),
                "Forwarding contains a removed user"
            );
            anyhow::ensure!(
                edges
                    .iter()
                    .all(|edge| virtual_mouse(&seats[edge.source].mouse)
                        && virtual_mouse(&seats[edge.target].mouse)
                        && seats[edge.source]
                            .keyboard
                            .as_deref()
                            .is_none_or(virtual_keyboard)
                        && seats[edge.target]
                            .keyboard
                            .as_deref()
                            .is_none_or(virtual_keyboard)
                        && (!edge.keyboard || seats[edge.source].keyboard.is_some())),
                "Forwarding endpoints need virtual mice and optional virtual keyboards"
            );
            Ok(())
        });
        if let Err(error) = &validation {
            ui.colored_label(ui.visuals().error_fg_color, format!("Draft: {error:#}"));
        }
        let can_apply = can_edit
            && !automation_active
            && !replication_active
            && validation.is_ok()
            && self.draft != self.applied;
        let label = "Apply input forwarding";
        let response = Button::new(label).enabled(can_apply).show(ui);
        let accepted = script.is_none_or(|script| script.observe(label, &response));
        can_apply
            && response.clicked()
            && accepted
            && consume(ui.ctx(), label, action_frame, frame_actions)
    }
}

fn consume(
    ctx: &egui::Context,
    label: &str,
    action_frame: &mut u64,
    frame_actions: &mut BTreeSet<String>,
) -> bool {
    let frame = ctx.cumulative_frame_nr();
    if *action_frame != frame {
        *action_frame = frame;
        frame_actions.clear();
    }
    frame_actions.insert(label.to_owned())
}

fn virtual_mouse(path: &str) -> bool {
    emulated::parse(path)
        .ok()
        .flatten()
        .is_some_and(|device| device.kind == mux_core::DeviceKind::Mouse)
}

fn virtual_keyboard(path: &str) -> bool {
    emulated::parse(path)
        .ok()
        .flatten()
        .is_some_and(|device| device.kind == mux_core::DeviceKind::Keyboard)
}

/// A new successor is invalid when it can already reach this source.
fn cycle_targets(definition: &Definition, source: &str) -> BTreeSet<String> {
    let mut reverse: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in &definition.edges {
        reverse.entry(&edge.target).or_default().push(&edge.source);
    }
    let mut seen = BTreeSet::new();
    let mut pending = vec![source.to_owned()];
    while let Some(node) = pending.pop() {
        if seen.insert(node.clone())
            && let Some(predecessors) = reverse.get(node.as_str())
        {
            pending.extend(predecessors.iter().map(|id| (*id).to_owned()));
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seats() -> Vec<Seat> {
        (1..=3)
            .map(|index| {
                serde_json::from_value(serde_json::json!({
                    "id": format!("seat-{index}"), "name": format!("User {index}"),
                    "mouse": format!("virtual://mouse/{index}"),
                    "keyboard": format!("virtual://keyboard/{index}")
                }))
                .unwrap()
            })
            .collect()
    }

    #[test]
    fn live_apply_commits_only_matching_forwarding_revision_and_keeps_external_draft() {
        let seats = seats();
        let mut page = ForwardingPage::default();
        let profile = mux_core::Profile {
            seats: seats.clone(),
            ..Default::default()
        };
        page.reset_profile(&profile);
        page.stage("seat-1", Some("seat-2"), true);
        let (_, edges) = page.begin_apply(&seats, true).unwrap();
        let mut state = automation_runtime::State {
            acknowledged: true,
            ..Default::default()
        };
        assert!(page.receive(&state, &seats).is_none());
        assert!(page.snapshot().pending);
        state.forwarding_revision = 1;
        state.forwarding_edges = edges;
        let committed = page.receive(&state, &seats).unwrap();
        assert_eq!(committed.edges[0].target, "seat-2");
        assert!(!page.snapshot().pending);
        page.stage("seat-1", Some("seat-3"), false);
        let external = automation_runtime::State {
            forwarding_revision: 2,
            forwarding_edges: vec![ResolvedEdge {
                source: 1,
                target: 2,
                keyboard: false,
            }],
            ..Default::default()
        };
        assert!(page.receive_external(&external, &seats).is_some());
        assert_eq!(page.snapshot().draft.edges[0].target, "seat-3");
        assert_eq!(page.snapshot().applied.edges[0].source, "seat-2");
    }

    #[test]
    fn mismatched_ack_does_not_commit_staged_routes() {
        let seats = seats();
        let mut page = ForwardingPage::default();
        page.stage("seat-1", Some("seat-2"), false);
        page.begin_apply(&seats, true).unwrap();
        let state = automation_runtime::State {
            acknowledged: true,
            forwarding_revision: 1,
            forwarding_edges: vec![ResolvedEdge {
                source: 1,
                target: 2,
                keyboard: false,
            }],
            ..Default::default()
        };
        assert!(page.receive(&state, &seats).is_none());
        assert!(page.snapshot().applied.edges.is_empty());
        assert!(page.snapshot().error.is_some());
    }

    #[test]
    fn menu_excludes_targets_that_would_close_a_chain() {
        let definition = Definition {
            edges: vec![
                Edge {
                    source: "seat-1".into(),
                    target: "seat-2".into(),
                    keyboard: false,
                },
                Edge {
                    source: "seat-2".into(),
                    target: "seat-3".into(),
                    keyboard: false,
                },
            ],
            ..Default::default()
        };
        let blocked = cycle_targets(&definition, "seat-3");
        assert!(blocked.contains("seat-1"));
        assert!(blocked.contains("seat-2"));
        assert!(!cycle_targets(&definition, "seat-1").contains("seat-3"));
    }
}
