//! Ouroboros controls for granting a local MCP client scoped access to a live GUI session.
use crate::{agent_runtime, ui_script::Script};
use eframe::egui::{self, Color32};
use mux_core::Seat;
use mux_windows::targets::Target;
use ouroboros_ui::{
    atoms::{Button, ButtonVariant, Checkbox, Divider, Heading, Input, Text},
    molecules::Collapsible,
    tokens::core,
};
use std::collections::{BTreeMap, BTreeSet};

/// Reserve actions before laying out the independently scrolling form body.
/// A narrow viewport uses a column, so labels never compete for one short row.
pub(crate) fn action_bar<R>(
    ui: &mut egui::Ui,
    id: &str,
    count: usize,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let narrow = ui.available_width() < 600.0;
    let rows = if narrow { count } else { 1 };
    let height = rows as f32 * core::Size::Sm.height()
        + rows.saturating_sub(1) as f32 * ui.spacing().item_spacing.y
        + core::SPACE_2;
    egui::Panel::bottom(ui.make_persistent_id(id))
        .frame(egui::Frame::NONE)
        .exact_size(height)
        .show_inside(ui, |ui| {
            ui.add_space(core::SPACE_2);
            ui.with_layout(
                if narrow {
                    egui::Layout::top_down(egui::Align::Min)
                } else {
                    egui::Layout::left_to_right(egui::Align::Center)
                },
                contents,
            )
            .inner
        })
        .inner
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SdkAction {
    Enable,
    Disable,
    Copy,
}

fn action_button(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    variant: ButtonVariant,
    script: Option<&mut Script>,
    actions: (&mut u64, &mut BTreeSet<String>),
) -> bool {
    let response = Button::new(label)
        .sm()
        .variant(variant)
        .enabled(enabled)
        .show(ui);
    #[cfg(test)]
    retain_test_response(ui, label, &response);
    let accepted = script.is_none_or(|script| script.observe(label, &response));
    response.clicked() && accepted && consume(actions.0, actions.1, ui.ctx(), label)
}

pub(crate) fn access_tabs(
    ui: &mut egui::Ui,
    sdk: &mut bool,
    script: Option<&mut Script>,
    actions: (&mut u64, &mut BTreeSet<String>),
) {
    let mut script = script;
    ui.horizontal(|ui| {
        for (label, control, selected) in [
            ("Access", "Agent Access tab", false),
            ("SDK", "SDK tab", true),
        ] {
            let response = Button::new(label)
                .sm()
                .variant(if *sdk == selected {
                    ButtonVariant::Secondary
                } else {
                    ButtonVariant::Ghost
                })
                .show(ui);
            #[cfg(test)]
            retain_test_response(ui, control, &response);
            let accepted = script
                .as_deref_mut()
                .is_none_or(|script| script.observe(control, &response));
            if response.clicked() && accepted && consume(actions.0, actions.1, ui.ctx(), control) {
                *sdk = selected;
            }
        }
    });
}

pub(crate) fn sdk_controls(
    ui: &mut egui::Ui,
    snapshot: &crate::gui_sdk::Snapshot,
    origins: &mut String,
    bridge_ready: bool,
    script: Option<&mut Script>,
    actions: (&mut u64, &mut BTreeSet<String>),
) -> Option<SdkAction> {
    let mut script = script;
    let mut action = None;
    Text::new(format!("Command SDK: {}", snapshot.phase))
        .caption()
        .show(ui);
    action_bar(ui, "sdk-connection-actions", 2, |ui| {
        if action_button(
            ui,
            "Enable SDK connections",
            snapshot.retired && bridge_ready,
            ButtonVariant::Secondary,
            script.as_deref_mut(),
            (actions.0, actions.1),
        ) {
            action = Some(SdkAction::Enable);
        }
        if action_button(
            ui,
            "Disable SDK connections",
            !snapshot.retired,
            ButtonVariant::Outline,
            script.as_deref_mut(),
            (actions.0, actions.1),
        ) {
            action = Some(SdkAction::Disable);
        }
    });
    egui::ScrollArea::vertical()
        .id_salt("sdk-connection-body")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            Text::new("Rust and browser clients over a private loopback WebSocket.")
                .caption().wrap().show(ui);
            Text::new("Trusted browser origins (comma separated)").caption().show(ui);
            ui.add_enabled_ui(snapshot.retired, |ui| {
                let input = Input::new(origins).sm().id_source("sdk-origin-list").show(ui);
                if let Some(script) = script.as_deref_mut() {
                    script.observe("Trusted browser origins", &input);
                }
            });
            Text::new("Leave empty for native clients only. Browser apps need an exact http:// or https:// origin and the private bearer.")
                .caption().wrap().show(ui);
            if !bridge_ready && snapshot.retired {
                Text::new("Enable setup access or live agent access on the Access tab first.").caption().wrap().show(ui);
            }
            if let Some(error) = &snapshot.error {
                Text::new(error.clone()).caption().wrap().show(ui);
            }
            let details = Collapsible::new("Connection details")
                .id_source("sdk-connection-details")
                .default_open(false)
                .show(ui, |ui| {
                    Text::new("Private loopback WebSocket; existing setup or agent permissions still apply.")
                        .caption().wrap().show(ui);
                    if let Some(url) = &snapshot.url {
                        Text::new(url.clone()).caption().wrap().show(ui);
                    }
                    if let Some(path) = &snapshot.endpoint_path {
                        Text::new(format!("Private connection file: {}", path.display())).caption().wrap().show(ui);
                    }
                    if action_button(ui, "Copy private SDK connection", snapshot.phase == "active" && bridge_ready,
                        ButtonVariant::Outline, script.as_deref_mut(), (actions.0, actions.1)) {
                        action = Some(SdkAction::Copy);
                    }
                    Text::new("Rust clients read the private connection file. For a trusted browser app, explicitly copy its connection JSON or open the file yourself. The bearer is private and is never shown in status.")
                        .caption().wrap().show(ui);
                });
            if let Some(script) = script {
                script.observe("SDK connection details", &details);
            }
        });
    action
}

#[cfg(test)]
fn retain_test_response(ui: &egui::Ui, label: &str, response: &egui::Response) {
    ui.ctx().data_mut(|data| {
        data.insert_temp(
            egui::Id::new(("agent-layout-response", label)),
            (response.rect, response.interact_rect, response.enabled()),
        )
    });
}

#[derive(Clone)]
pub enum PageAction {
    Configure(agent_runtime::Command),
}

fn activity_details(
    ui: &mut egui::Ui,
    snapshot: Option<&crate::gui_activity::Snapshot>,
    mut script: Option<&mut Script>,
) {
    let disclosure = Collapsible::new("GUI activity diagnostics")
        .id_source("gui-activity-diagnostics").default_open(false).show(ui, |ui| {
                let explanation = Text::new("Callback observations only. UI returns are not paint or input-delivery receipts; hidden or minimized UI may not advance. These observations grant no routing or recovery authority.").caption().wrap().show(ui);
                if let Some(script) = script.as_deref_mut() { script.observe("GUI activity explanation", &explanation); }
                let Some(snapshot) = snapshot else { Text::new("Activity observation unavailable.").caption().show(ui);return; };
                Text::new(format!("GUI instance {} · sample age {} ms", snapshot.gui_instance,snapshot.sample_age_ms)).caption().wrap().show(ui);
                script_capture_details(ui, snapshot, script.as_deref_mut());
                let Some(observation) = snapshot.observation.as_ref().filter(|_|snapshot.coherent) else {
                    Text::new("No coherent observation available; inspect again.").caption().wrap().show(ui);return;
                };
                let phase=observation.phase.map_or("none",|phase|match phase {
                    crate::gui_activity::Callback::RawInput=>"raw input",
                    crate::gui_activity::Callback::Logic=>"logic",
                    crate::gui_activity::Callback::Ui=>"UI construction",
                });
                Text::new(format!("Last callback: {phase} · {}{}",if observation.in_callback {"inside callback"}else{"outside callback"},if observation.closed {" · writer closed"}else{""})).caption().wrap().show(ui);
                let age=|value:Option<u64>|value.map_or_else(||"unavailable".to_owned(),|value|format!("{value} ms"));
                Text::new(format!("Phase age: {} · last return: {}",age(observation.phase_age_ms),age(observation.last_return_age_ms))).caption().wrap().show(ui);
                for (label,counter) in [("Raw input",&observation.raw_input),("Logic",&observation.logic),("UI",&observation.ui)] {
                    let response = Text::new(format!("{label}: {} entered · {} returned · {} unwound",counter.entered,counter.returned,counter.unwound)).caption().wrap().show(ui);
                    if label == "UI" && let Some(script) = script.as_deref_mut() { script.observe("GUI activity counters", &response); }
                }
        });
    if let Some(script) = script {
        script.observe("GUI activity diagnostics", &disclosure);
    }
}

fn script_capture_details(
    ui: &mut egui::Ui,
    snapshot: &crate::gui_activity::Snapshot,
    script: Option<&mut Script>,
) {
    ui.separator();
    let response = Text::new("Script screenshot capture")
        .caption()
        .wrap()
        .show(ui);
    if let Some(script) = script {
        script.observe("Script capture diagnostics", &response);
    }
    let Some(capture) = &snapshot.render_capture else {
        Text::new("No Script capture observation available.")
            .caption()
            .wrap()
            .show(ui);
        return;
    };
    let age = |value: Option<u64>| value.map_or_else(|| "none".to_owned(), |ms| format!("{ms} ms"));
    let generation =
        |value: Option<u64>| value.map_or_else(|| "none".to_owned(), |value| value.to_string());
    Text::new(format!(
        "Script {} · screenshots {}{}",
        capture.script_id,
        if capture.enabled {
            "enabled"
        } else {
            "disabled"
        },
        if capture.retired { " · retired" } else { "" },
    ))
    .caption()
    .wrap()
    .show(ui);
    Text::new(format!(
        "Requests: {} · generation {} · age {}",
        capture.requested_count,
        capture.requested_generation,
        age(capture.request_age_ms),
    ))
    .caption()
    .wrap()
    .show(ui);
    Text::new(format!(
        "Frames received: {} · generation {} · age {}",
        capture.received_count,
        capture.received_generation,
        age(capture.receipt_age_ms),
    ))
    .caption()
    .wrap()
    .show(ui);
    Text::new(format!(
        "Pending renderer response: {}",
        age(capture.pending_age_ms)
    ))
    .caption()
    .wrap()
    .show(ui);
    if let Some([width, height]) = capture.received_dimensions {
        Text::new(format!(
            "Received frame: {width} × {height} · PNG worker accepted: {}",
            capture
                .worker_accepted
                .map_or("unknown", |accepted| if accepted { "yes" } else { "no" })
        ))
        .caption()
        .wrap()
        .show(ui);
    }
    if let Some(publication) = &capture.publication {
        Text::new(format!(
            "PNG generations · accepted {} · active {} · pending {}",
            publication.accepted,
            generation(publication.active),
            generation(publication.pending)
        ))
        .caption()
        .wrap()
        .show(ui);
        Text::new(format!(
            "Published {} · failed {} · rejected {}{}",
            generation(publication.published),
            generation(publication.failed),
            generation(
                publication
                    .rejected
                    .as_ref()
                    .map(|rejection| rejection.generation)
            ),
            if publication.retired {
                " · worker retired"
            } else {
                ""
            }
        ))
        .caption()
        .wrap()
        .show(ui);
    } else {
        Text::new("PNG worker observation unavailable.")
            .caption()
            .wrap()
            .show(ui);
    }
    Text::new("Requests are not received frames; received frames are not published PNGs. Ages do not measure GPU readback time or normal repaint cadence. Retirement does not confirm completion.")
        .caption().wrap().show(ui);
}

#[derive(Default)]
pub struct SetupPage {
    activity: Option<crate::gui_activity::Snapshot>,
    pub pending: bool,
    pub descriptor: Option<String>,
    pub error: Option<String>,
}

impl SetupPage {
    pub fn set_activity(&mut self, activity: Option<crate::gui_activity::Snapshot>) {
        self.activity = activity;
    }
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        enabled: bool,
        broker_phase: &crate::gui_agent_broker::Phase,
        executable: Option<&str>,
        script: Option<&mut Script>,
        actions: (&mut u64, &mut BTreeSet<String>),
    ) -> Option<bool> {
        let mut script = script;
        let (action_frame, frame_actions) = actions;
        let mut action = None;
        action_bar(ui, "setup-access-actions", 1, |ui| {
            let label = if enabled {
                "Disable setup access"
            } else {
                "Enable setup access"
            };
            let response = Button::new(label)
                .sm()
                .variant(if enabled {
                    ButtonVariant::Outline
                } else {
                    ButtonVariant::Secondary
                })
                .enabled(
                    !self.pending
                        && (enabled
                            || matches!(
                                broker_phase,
                                crate::gui_agent_broker::Phase::Stopped
                                    | crate::gui_agent_broker::Phase::StartFailed(_)
                            )),
                )
                .show(ui);
            let accepted = script
                .as_deref_mut()
                .is_none_or(|script| script.observe(label, &response));
            if response.clicked()
                && accepted
                && consume(action_frame, frame_actions, ui.ctx(), label)
            {
                action = Some(!enabled);
            }
        });
        egui::ScrollArea::vertical()
            .id_salt("setup-access-body")
            .auto_shrink([false, false])
            .show(ui, |ui| {
        Heading::new("Stopped GUI setup").heading().show(ui);
        Text::new("Allow a local MCP client to inspect and configure this stopped GUI profile, change startup/exit profile-saving preferences, and Save/Load or Import/Export through the paths selected in the GUI. Profile edits may stage target PIDs; GUI Start checks them. This access also reads cooperative application-event status counts and ages, without user identities, event credentials or consent changes. It is separate from live routing permissions and turns off before Start.")
            .wrap().show(ui);
        Text::new(if enabled {
            "Applied: setup access enabled."
        } else {
            "Applied: setup access off."
        })
        .caption()
        .show(ui);
        if let Some(error) = &self.error {
            Text::new(error.clone()).wrap().show(ui);
        }
        if self.pending {
            Text::new("Preparing setup connection…").caption().show(ui);
        }
        if let Some(descriptor) = &self.descriptor
            && enabled
            && matches!(broker_phase, crate::gui_agent_broker::Phase::Active)
        {
            Text::new(format!("Descriptor: {descriptor}"))
                .caption()
                .wrap()
                .show(ui);
            if let Some(entry) =
                executable.and_then(|executable| mcp_server_entry(executable, descriptor))
            {
                let label = "Copy setup MCP server entry";
                let response = Button::new(label)
                    .variant(ButtonVariant::Secondary)
                    .show(ui);
                let accepted = script.as_deref_mut().is_none_or(|script| script.observe(label, &response));
                if response.clicked()
                    && accepted
                    && consume(action_frame, frame_actions, ui.ctx(), label)
                {
                    ui.ctx().copy_text(entry);
                }
            }
        }
        activity_details(ui,self.activity.as_ref(),script);
            });
        action
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Snapshot {
    pub selected_users: Vec<String>,
    pub allowed_pids: Vec<u32>,
    pub capture: bool,
    pub desktop_capture: bool,
    pub session_control: bool,
    pub app_preferences: bool,
    pub policy_enabled: bool,
    pub clients: Vec<u64>,
    pub cleanup: Vec<agent_runtime::ClientCleanupState>,
    pub pending: bool,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct AgentPage {
    activity: Option<crate::gui_activity::Snapshot>,
    selected_users: BTreeSet<String>,
    allowed_pids: BTreeSet<u32>,
    capture: bool,
    desktop_capture: bool,
    session_control: bool,
    app_preferences: bool,
    state: Option<agent_runtime::State>,
    pending: bool,
    error: Option<String>,
    connection_feedback: Option<String>,
    connection_feedback_descriptor: Option<String>,
}

impl AgentPage {
    pub fn set_activity(&mut self, activity: Option<crate::gui_activity::Snapshot>) {
        self.activity = activity;
    }
    pub fn reset_session(&mut self) {
        self.selected_users.clear();
        self.allowed_pids.clear();
        self.capture = false;
        self.desktop_capture = false;
        self.session_control = false;
        self.app_preferences = false;
        self.state = None;
        self.pending = false;
        self.error = None;
        self.connection_feedback = None;
        self.connection_feedback_descriptor = None;
    }

    pub fn apply_script_fields(&mut self, fields: &BTreeMap<String, String>, seats: &[Seat]) {
        for (key, value) in fields {
            match key.as_str() {
                "agent_users" => {
                    self.selected_users = value
                        .split(',')
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                        .filter(|id| seats.iter().any(|seat| seat.identity() == *id))
                        .map(str::to_owned)
                        .collect();
                }
                "agent_pids" => {
                    self.allowed_pids = value
                        .split(',')
                        .filter_map(|pid| pid.trim().parse::<u32>().ok())
                        .filter(|pid| *pid != 0 && *pid != std::process::id())
                        .collect();
                }
                "agent_capture" => self.capture = value.parse().unwrap_or(self.capture),
                "agent_desktop_capture" => {
                    self.desktop_capture = value.parse().unwrap_or(self.desktop_capture)
                }
                "agent_session_control" => {
                    self.session_control = value.parse().unwrap_or(self.session_control)
                }
                "agent_app_preferences" => {
                    self.app_preferences = value.parse().unwrap_or(self.app_preferences)
                }
                _ => {}
            }
        }
    }

    pub fn receive(&mut self, state: agent_runtime::State) -> bool {
        let acknowledged = state.acknowledged;
        if acknowledged && state.client == 0 {
            self.pending = false;
            self.error.clone_from(&state.error);
            if !state.policy.enabled {
                self.connection_feedback = None;
                self.connection_feedback_descriptor = None;
            }
            self.selected_users = state.policy.user_ids.iter().cloned().collect();
            self.allowed_pids = state.policy.allowed_pids.iter().copied().collect();
            self.capture = state.policy.capture;
            self.desktop_capture = state.policy.desktop_capture;
            self.session_control = state.policy.session_control;
            self.app_preferences = state.policy.app_preferences;
        } else if state.client == 0 && state.error.is_some() {
            self.error.clone_from(&state.error);
        }
        self.state = Some(state);
        acknowledged
    }

    pub fn request_failed(&mut self, error: impl Into<String>) {
        self.pending = false;
        self.error = Some(error.into());
    }

    pub fn snapshot(&self) -> Snapshot {
        let policy = self.state.as_ref().map(|state| &state.policy);
        Snapshot {
            selected_users: self.selected_users.iter().cloned().collect(),
            allowed_pids: self.allowed_pids.iter().copied().collect(),
            capture: self.capture,
            desktop_capture: self.desktop_capture,
            session_control: self.session_control,
            app_preferences: self.app_preferences,
            policy_enabled: policy.is_some_and(|policy| policy.enabled),
            clients: self
                .state
                .as_ref()
                .map(|state| state.clients.clone())
                .unwrap_or_default(),
            cleanup: self
                .state
                .as_ref()
                .map(|state| state.cleanup.clone())
                .unwrap_or_default(),
            pending: self.pending,
            error: self.error.clone(),
        }
    }

    pub fn allows_app_preferences(&self, client: u64) -> bool {
        self.client_is_enabled(client)
            && self
                .state
                .as_ref()
                .is_some_and(|state| state.policy.app_preferences)
    }

    pub fn client_is_enabled(&self, client: u64) -> bool {
        self.state
            .as_ref()
            .is_some_and(|state| state.policy.enabled && state.clients.contains(&client))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        seats: &[Seat],
        active_users: &[bool],
        allowed_pids: &[u32],
        targets: &[Target],
        descriptor_path: &str,
        executable_path: Option<&str>,
        broker_phase: &crate::gui_agent_broker::Phase,
        session_active: bool,
        session_ready: bool,
        busy: bool,
        script: Option<&mut Script>,
        action_frame: &mut u64,
        frame_actions: &mut BTreeSet<String>,
    ) -> Option<PageAction> {
        let mut script = script;
        let mut action = None;
        let active = self
            .state
            .as_ref()
            .is_some_and(|state| state.policy.enabled);
        // The connection controls now precede the policy scroller's early
        // return, so retain its original session-readiness gate explicitly.
        let connection_available = active
            && !self.pending
            && matches!(broker_phase, crate::gui_agent_broker::Phase::Active)
            && session_active
            && session_ready;
        if self.connection_feedback.is_some()
            && !connection_feedback_is_current(
                connection_available,
                descriptor_path,
                self.connection_feedback_descriptor.as_deref(),
            )
        {
            self.connection_feedback = None;
            self.connection_feedback_descriptor = None;
        }
        let active_ids: BTreeSet<String> = seats
            .iter()
            .enumerate()
            .filter(|(index, seat)| {
                active_users.get(*index).copied().unwrap_or(true) && has_virtual_devices(seat)
            })
            .map(|(_, seat)| seat.identity())
            .collect();
        self.selected_users.retain(|id| active_ids.contains(id));
        Text::new(format!(
            "{} · {} client(s)",
            match broker_phase {
                crate::gui_agent_broker::Phase::Starting => "Preparing access",
                crate::gui_agent_broker::Phase::Stopping => "Closing access",
                crate::gui_agent_broker::Phase::CleanupFailed(_) => "Cleanup incomplete",
                crate::gui_agent_broker::Phase::StartFailed(_)
                | crate::gui_agent_broker::Phase::WorkerFailed(_) => "Access unavailable",
                crate::gui_agent_broker::Phase::Active if self.pending => "Applying policy",
                crate::gui_agent_broker::Phase::Active if active => "Access enabled",
                crate::gui_agent_broker::Phase::Stopped if active => "Broker unavailable",
                crate::gui_agent_broker::Phase::Active
                | crate::gui_agent_broker::Phase::Stopped => "Access off",
            },
            self.state.as_ref().map_or(0, |state| state.clients.len()),
        ))
        .caption()
        .show(ui);
        if let Some(state) = &self.state
            && !state.cleanup.is_empty()
        {
            Text::new(format!(
                "{} client cleanup(s) pending; releases retry automatically.",
                state.cleanup.len()
            ))
            .caption()
            .wrap()
            .show(ui);
        }
        action_bar(ui, "agent-access-actions", 3, |ui| {
            let allowed = session_active
                && session_ready
                && !busy
                && !self.pending
                && !self.selected_users.is_empty()
                && !self.allowed_pids.is_empty();
            let enable = Button::new("Enable agent access")
                .sm()
                .variant(ButtonVariant::Secondary)
                .enabled(allowed && !active)
                .show(ui);
            let label = "Enable agent access";
            let accepted = script
                .as_deref_mut()
                .is_none_or(|script| script.observe(label, &enable));
            if enable.clicked() && accepted && consume(action_frame, frame_actions, ui.ctx(), label)
            {
                self.pending = true;
                action = Some(PageAction::Configure(configure(
                    true,
                    &self.selected_users,
                    &self.allowed_pids,
                    self.capture,
                    self.desktop_capture,
                    self.session_control,
                    self.app_preferences,
                )));
            }
            let disable = Button::new("Disable agent access")
                .sm()
                .variant(ButtonVariant::Outline)
                .enabled(session_active && session_ready && !busy && !self.pending && active)
                .show(ui);
            let label = "Disable agent access";
            let accepted = script
                .as_deref_mut()
                .is_none_or(|script| script.observe(label, &disable));
            if disable.clicked()
                && accepted
                && consume(action_frame, frame_actions, ui.ctx(), label)
            {
                self.pending = true;
                action = Some(PageAction::Configure(configure(
                    false,
                    &self.selected_users,
                    &self.allowed_pids,
                    self.capture,
                    self.desktop_capture,
                    self.session_control,
                    self.app_preferences,
                )));
            }
            let apply = Button::new("Apply access policy")
                .sm()
                .variant(ButtonVariant::Secondary)
                .enabled(allowed && active)
                .show(ui);
            let label = "Apply access policy";
            #[cfg(test)]
            retain_test_response(ui, label, &apply);
            let accepted = script
                .as_deref_mut()
                .is_none_or(|script| script.observe(label, &apply));
            if apply.clicked() && accepted && consume(action_frame, frame_actions, ui.ctx(), label)
            {
                self.pending = true;
                action = Some(PageAction::Configure(configure(
                    true,
                    &self.selected_users,
                    &self.allowed_pids,
                    self.capture,
                    self.desktop_capture,
                    self.session_control,
                    self.app_preferences,
                )));
            }
        });
        egui::ScrollArea::vertical()
            .id_salt("agent-access-policy")
            .auto_shrink([false, false])
            .show(ui, |ui| {
            Text::new("Connect a local MCP client to this running GUI session. Access is limited to the selected active virtual users and explicit target process IDs. Input remains virtual and app-local.")
                .caption().wrap().show(ui);
            if let Some(error) = &self.error {
                ui.colored_label(Color32::LIGHT_RED, error);
            }
            if self.pending {
                ui.weak("Waiting for the GUI session to acknowledge this policy.");
            }
            let details = Collapsible::new("Connection details")
                .id_source("agent-connection-details")
                .default_open(false)
                .show(ui, |ui| {
        activity_details(ui,self.activity.as_ref(),script.as_deref_mut());
        Text::new(connection_status(
            active,
            self.pending,
            broker_phase,
            !descriptor_path.is_empty(),
        ))
        .wrap()
        .show(ui);
        if !session_active || !session_ready {
            Text::new("Start a ready routing session before enabling Agent Access or copying connection setup.")
                .caption().wrap().show(ui);
        }
        let copy_ready = connection_available && !descriptor_path.is_empty();
        if copy_ready {
            Text::new(format!("Descriptor: {descriptor_path}"))
                .caption()
                .wrap()
                .show(ui);
            Text::new("Copy the server entry into a trusted local MCP client. Keep the descriptor file private.")
                    .caption().wrap().show(ui);
        }
        let entry = executable_path
            .filter(|_| copy_ready)
            .and_then(|executable| mcp_server_entry(executable, descriptor_path));
        let entry_available = entry.is_some();
        ui.horizontal_wrapped(|ui| {
            let label = "Copy MCP server entry";
            let response = Button::new(label)
                .variant(ButtonVariant::Secondary)
                .enabled(entry_available)
                .show(ui);
            let accepted = script
                .as_deref_mut()
                .is_none_or(|script| script.observe(label, &response));
            if response.clicked()
                && accepted
                && consume(action_frame, frame_actions, ui.ctx(), label)
                && let Some(entry) = entry
            {
                ui.ctx().copy_text(entry);
                self.connection_feedback = Some(
                    "MCP server entry copied. The descriptor remains in its private file.".into(),
                );
                self.connection_feedback_descriptor = Some(descriptor_path.to_owned());
            }
            let path_label = "Copy descriptor path";
            let path_response = Button::new(path_label)
                .variant(ButtonVariant::Outline)
                .enabled(copy_ready)
                .show(ui);
            let accepted = script
                .as_deref_mut()
                .is_none_or(|script| script.observe(path_label, &path_response));
            if path_response.clicked()
                && accepted
                && consume(action_frame, frame_actions, ui.ctx(), path_label)
            {
                ui.ctx().copy_text(descriptor_path.to_owned());
                self.connection_feedback =
                    Some("Descriptor path copied. The descriptor contents were not read.".into());
                self.connection_feedback_descriptor = Some(descriptor_path.to_owned());
            }
        });
        if copy_ready && !entry_available {
            ui.weak("The running executable path is unavailable; the MCP server entry cannot be prepared exactly.");
        }
        if let Some(feedback) = &self.connection_feedback {
            ui.weak(feedback);
        }
        if let Some(state) = &self.state {
            if state.clients.is_empty() {
                ui.label("Connected clients: none.");
            } else {
                ui.label(format!(
                    "Connected local MCP clients: {}",
                    state.clients.len()
                ));
                ui.label(format!("Active client IDs: {:?}", state.clients));
            }
        }
        Divider::horizontal().show(ui);
        if let Some(state) = &self.state {
            if state.cleanup.is_empty() {
                ui.weak("No agent cleanup pending.");
            } else {
                // Keep release status visible while the policy form is scrolled.
                // Bound the region so all eight retained clients cannot crowd out the form.
                let status_height = (ui.available_height() * 0.45).min(180.0);
                egui::ScrollArea::vertical()
                    .id_salt("agent-cleanup-status")
                    .max_height(status_height)
                    .show(ui, |ui| {
                        for cleanup in &state.cleanup {
                            ui.push_id(cleanup.client, |ui| {
                                Text::new(format!(
                                    "Client {} · {} input owner(s) remaining",
                                    cleanup.client, cleanup.pending_owners
                                ))
                                .caption()
                                .wrap()
                                .show(ui);
                                ui.weak("Releases retry automatically.");
                                if !cleanup.last_error.is_empty() {
                                    let error = bounded_error(&cleanup.last_error);
                                    let disclosure = ui.collapsing("Last release error", |ui| {
                                        ui.label(&error);
                                    });
                                    if let Some(script) = script.as_deref_mut() {
                                        script.observe(
                                            "Last release error",
                                            &disclosure.header_response,
                                        );
                                    }
                                    disclosure
                                        .header_response
                                        .on_hover_text(&cleanup.last_error);
                                }
                            });
                        }
                    });
            }
        }

                });
            if let Some(script) = script.as_deref_mut() {
                script.observe("Agent connection details", &details);
            }
            if !session_active || !session_ready {
                return;
            }

            Heading::new("Virtual users").heading().show(ui);
            let mut seen = BTreeSet::new();
            for (index, seat) in seats.iter().enumerate() {
                let id = seat.identity();
                if !seen.insert(id.clone()) {
                    continue;
                }
                if !active_users.get(index).copied().unwrap_or(true) {
                    ui.horizontal(|ui| {
                        ui.weak(format!("{} — Virtual replica removed", seat.name));
                    });
                    continue;
                }
                let mut selected = self.selected_users.contains(&id);
                let short_id: String = id.chars().take(12).collect();
                let label = format!("{} · {}", seat.name, short_id);
                let virtual_devices = has_virtual_devices(seat);
                let response = ui
                    .add_enabled_ui(virtual_devices, |ui| {
                        Checkbox::new(&mut selected).label(label).show(ui)
                    })
                    .inner;
                if !virtual_devices {
                    ui.weak("Physical or unavailable devices cannot be shared.");
                    continue;
                }
                let accepted = script
                    .as_deref_mut()
                    .is_none_or(|script| script.observe(&format!("Agent user {}", index + 1), &response));
                if response.changed() && accepted && consume(action_frame, frame_actions, ui.ctx(), &format!("Agent user {}", index + 1)) {
                    if selected {
                        self.selected_users.insert(id);
                    } else {
                        self.selected_users.remove(&id);
                    }
                }
            }

            Heading::new("Allowed target processes").heading().show(ui);
            if allowed_pids.is_empty() {
                Text::new("The GUI routing session has no PID allowlist. Select exact existing targets here to create Agent Access's narrower process scope.")
                    .caption().wrap().show(ui);
            }
            let mut eligible: Vec<_> = targets
                .iter()
                .filter(|target| {
                    target.pid != 0
                        && target.pid != std::process::id()
                        && (allowed_pids.is_empty() || allowed_pids.contains(&target.pid))
                })
                .map(|target| (target.pid, target.title.clone()))
                .collect();
            eligible.sort_by_key(|(pid, _)| *pid);
            eligible.dedup_by_key(|(pid, _)| *pid);
            if eligible.is_empty() {
                ui.weak("No eligible target process is currently available in the routing scope.");
            }
            for (pid, title) in eligible {
                let mut selected = self.allowed_pids.contains(&pid);
                let label = format!("PID {pid} · {title}");
                let response = Checkbox::new(&mut selected).label(label).show(ui);
                let control = format!("Agent target PID {pid}");
                let accepted = script
                    .as_deref_mut()
                    .is_none_or(|script| script.observe(&control, &response));
                if response.changed() && accepted && consume(action_frame, frame_actions, ui.ctx(), &control) {
                    if selected {
                        self.allowed_pids.insert(pid);
                    } else {
                        self.allowed_pids.remove(&pid);
                    }
                }
            }

            let policy = self.state.as_ref().map(|state| &state.policy);
            let granted: Vec<_> = [
                ("selected-target capture", policy.is_some_and(|policy| policy.enabled && policy.capture)),
                ("desktop screenshots", policy.is_some_and(|policy| policy.enabled && policy.desktop_capture)),
                ("session changes", policy.is_some_and(|policy| policy.enabled && policy.session_control)),
                ("app preferences", policy.is_some_and(|policy| policy.enabled && policy.app_preferences)),
            ].into_iter().filter(|(_, enabled)| *enabled).map(|(label, _)| label).collect();
            Text::new(if granted.is_empty() {
                "Applied grants: none.".to_owned()
            } else {
                format!("Applied grants: {}.", granted.join(", "))
            }).caption().wrap().show(ui);
            let draft: Vec<_> = [
                ("selected-target capture", self.capture),
                ("desktop screenshots", self.desktop_capture),
                ("session changes", self.session_control),
                ("app preferences", self.app_preferences),
            ].into_iter().filter(|(_, selected)| *selected).map(|(label, _)| label).collect();
            if draft != granted {
                Text::new(format!("Draft permissions for Enable/Apply: {}.",
                    if draft.is_empty() { "none".to_owned() } else { draft.join(", ") }))
                    .caption().wrap().show(ui);
            }
            let permissions = Collapsible::new("Additional permissions")
                .id_source("agent-additional-permissions")
                .default_open(false)
                .show(ui, |ui| {
            Text::new("Permission selections take effect only after a successful policy acknowledgment.")
                .caption().wrap().show(ui);
            self.capture = checkbox(ui, "Allow selected-target capture", self.capture, script.as_deref_mut(), action_frame, frame_actions, "Agent capture permission");
            Text::new("Transparent-ink and selected-target annotation exports use this grant. Selected-user ink needs that user's grant; all-users ink needs every active user's grant. Target composition captures only the selected authorized client.")
                .caption().wrap().show(ui);
            self.desktop_capture = checkbox(
                ui,
                "Allow desktop screenshots",
                self.desktop_capture,
                script.as_deref_mut(),
                action_frame,
                frame_actions,
                "Agent desktop capture permission",
            );
            Text::new("This separate grant permits explicit full-desktop or physical-region PNG capture through annotation export. The PID allowlist scopes virtual routing and does not authorize desktop pixels.")
                .caption().wrap().show(ui);
            self.session_control = checkbox(ui, "Allow session changes", self.session_control, script.as_deref_mut(), action_frame, frame_actions, "Agent session control permission");
            self.app_preferences = checkbox(ui, "Allow app-preference changes", self.app_preferences, script.as_deref_mut(), action_frame, frame_actions, "Agent app-preferences permission");
            Text::new("Session changes include cursor/output settings, annotation, per-user Macro Studio authoring, recording, playback and cancel, routing mode, pause/resume, and virtual replica Start/Stop. Connected replica changes require the full explicit automatic PID scope and permission for every other active user; Stop of another client's clone also requires its permission. A new replica is not added to Agent Access: grant it with its existing user checkbox, choose Apply access policy, then reconnect and Inspect before using it or a graph path that reaches it. The creating client may Stop its own clone while still connected without granting that clone. Disconnect leaves replicas owned by the GUI session. Apply may release held session input. Macro Studio records only this client's successfully accepted virtual actions, never physical or other clients' input. Magnifier configuration uses session changes; enabled capture, refresh, or export also needs the separate selected-target capture permission. App-preference changes are separate; login launch remains local-only. Connected replication graph Apply/Cancel require automatic routing, an explicit desktop process allowlist, and permission for every active user and allowed process.")
                .caption().wrap().show(ui);

                });
            let _ = script.take().is_none_or(|script| script.observe("Additional permissions", &permissions));

        });
        action
    }
}

fn bounded_error(error: &str) -> String {
    const MAX_CHARS: usize = 240;
    let mut bounded: String = error.chars().take(MAX_CHARS).collect();
    if error.chars().count() > MAX_CHARS {
        bounded.push('…');
    }
    bounded
}

fn configure(
    enabled: bool,
    users: &BTreeSet<String>,
    pids: &BTreeSet<u32>,
    capture: bool,
    desktop_capture: bool,
    session_control: bool,
    app_preferences: bool,
) -> agent_runtime::Command {
    agent_runtime::Command::Configure {
        enabled,
        user_ids: users.iter().cloned().collect(),
        allowed_pids: pids.iter().copied().collect(),
        capture,
        desktop_capture,
        session_control,
        app_preferences,
    }
}

fn mcp_server_entry(executable: &str, descriptor_path: &str) -> Option<String> {
    if executable.trim().is_empty() || descriptor_path.trim().is_empty() {
        return None;
    }
    serde_json::to_string_pretty(&serde_json::json!({
        "command": executable,
        "args": ["mcp", "--gui-connection", descriptor_path],
    }))
    .ok()
}

fn connection_status(
    active: bool,
    pending: bool,
    phase: &crate::gui_agent_broker::Phase,
    has_descriptor_path: bool,
) -> &'static str {
    use crate::gui_agent_broker::Phase;
    match phase {
        Phase::Starting => {
            "Preparing Agent Access. Connection copying is unavailable until the broker and policy are ready."
        }
        Phase::Stopping => {
            "Agent Access broker cleanup is pending. Connection copying is unavailable."
        }
        Phase::CleanupFailed(_) => {
            "Agent Access cleanup is incomplete and will be retried. Connection copying is unavailable."
        }
        Phase::StartFailed(_) => {
            "Agent Access broker could not start. Enable access again to retry."
        }
        Phase::WorkerFailed(_) => {
            "Agent Access lifecycle worker failed. Connection copying is unavailable."
        }
        Phase::Active if pending => {
            "Waiting for the Agent Access policy acknowledgment. Connection copying is unavailable."
        }
        Phase::Active if active && has_descriptor_path => {
            "Agent Access policy is enabled; descriptor available."
        }
        Phase::Active if active => {
            "Agent Access policy is enabled; its descriptor path cannot be represented exactly, so connection copying is unavailable."
        }
        Phase::Stopped if active => {
            "Agent Access policy is enabled, but the broker is stopped. Connection copying is unavailable."
        }
        Phase::Active | Phase::Stopped => {
            "Agent Access policy is disabled. Choose at least one virtual user and target process, then enable access; connection copying becomes available after policy acknowledgment."
        }
    }
}

pub(crate) fn exact_path_string(path: &std::path::Path) -> Option<String> {
    path.to_str().map(str::to_owned)
}

fn connection_feedback_is_current(
    active: bool,
    descriptor_path: &str,
    copied_descriptor_path: Option<&str>,
) -> bool {
    active && !descriptor_path.is_empty() && copied_descriptor_path == Some(descriptor_path)
}

fn has_virtual_devices(seat: &Seat) -> bool {
    mux_core::emulated::parse(&seat.mouse)
        .ok()
        .flatten()
        .is_some_and(|device| device.kind == mux_core::DeviceKind::Mouse)
        && seat.keyboard.as_deref().is_none_or(|path| {
            mux_core::emulated::parse(path)
                .ok()
                .flatten()
                .is_some_and(|device| device.kind == mux_core::DeviceKind::Keyboard)
        })
}

fn checkbox(
    ui: &mut egui::Ui,
    label: &str,
    value: bool,
    script: Option<&mut Script>,
    action_frame: &mut u64,
    frame_actions: &mut BTreeSet<String>,
    control: &str,
) -> bool {
    let mut draft = value;
    let response = Checkbox::new(&mut draft).label(label).show(ui);
    let accepted = script.is_none_or(|script| script.observe(control, &response));
    if response.changed() && accepted && consume(action_frame, frame_actions, ui.ctx(), control) {
        draft
    } else {
        value
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
mod tests {
    use super::*;

    fn layout_frame(
        ctx: &egui::Context,
        size: egui::Vec2,
        events: Vec<egui::Event>,
        mut show: impl FnMut(&mut egui::Ui),
    ) -> egui::FullOutput {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                events,
                ..Default::default()
            },
            |root| {
                egui::CentralPanel::default().show_inside(root, |ui| {
                    // The actual page also has navigation above the pane.
                    Button::new("Back to Home").sm().show(ui);
                    ui.add_space(core::SPACE_2);
                    show(ui);
                });
            },
        )
    }

    fn control_rect(ctx: &egui::Context, label: &str, screen: egui::Rect) -> egui::Rect {
        let (rect, interact, _) = ctx
            .data(|data| {
                data.get_temp::<(egui::Rect, egui::Rect, bool)>(egui::Id::new((
                    "agent-layout-response",
                    label,
                )))
            })
            .unwrap_or_else(|| panic!("{label} was not rendered"));
        assert!(
            screen.contains_rect(rect),
            "{label} left the viewport: {rect:?}"
        );
        assert!(
            interact.contains_rect(rect),
            "{label} was not fully interactable: {rect:?} within {interact:?}"
        );
        rect
    }

    fn click_events(position: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    #[test]
    fn policy_apply_remains_visible_and_clickable_with_long_open_details_and_cleanup() {
        for size in [egui::vec2(320.0, 360.0), egui::vec2(680.0, 620.0)] {
            let ctx = egui::Context::default();
            ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
            ctx.data_mut(|data| data.insert_temp(egui::Id::new("agent-connection-details"), true));
            let seat: Seat = serde_json::from_value(serde_json::json!({
                "id": "layout-user", "name": "Owned virtual user", "mouse": "virtual://mouse/1", "keyboard": "virtual://keyboard/1"
            })).unwrap();
            let pid = u32::MAX - 10;
            let targets = [Target {
                hwnd: 0,
                pid,
                title: "A long owned target title".repeat(8),
            }];
            let mut page = AgentPage::default();
            page.receive(agent_runtime::State {
                acknowledged: true,
                policy: agent_runtime::Policy {
                    enabled: true,
                    user_ids: vec![seat.identity()],
                    allowed_pids: vec![pid],
                    ..Default::default()
                },
                cleanup: (1..=8)
                    .map(|client| agent_runtime::ClientCleanupState {
                        client,
                        pending_owners: 1,
                        attempts: 1,
                        last_error: "A retained cleanup explanation".repeat(16),
                    })
                    .collect(),
                ..Default::default()
            });
            let mut frame = 0;
            let mut actions = BTreeSet::new();
            let mut sdk = false;
            let mut result = None;
            let descriptor = "C:/private/".repeat(80);
            let mut render = |events| {
                layout_frame(&ctx, size, events, |ui| {
                    access_tabs(ui, &mut sdk, None, (&mut frame, &mut actions));
                    if let Some(action) = page.show(
                        ui,
                        std::slice::from_ref(&seat),
                        &[true],
                        &[pid],
                        &targets,
                        &descriptor,
                        Some("C:/private/protoinputmultiplex.exe"),
                        &crate::gui_agent_broker::Phase::Active,
                        true,
                        true,
                        false,
                        None,
                        &mut frame,
                        &mut actions,
                    ) {
                        result = Some(action);
                    }
                })
            };
            let _ = render(vec![]);
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            let apply = control_rect(&ctx, "Apply access policy", screen);
            control_rect(&ctx, "SDK tab", screen);
            render(click_events(apply.center(), true));
            render(click_events(apply.center(), false));
            assert!(matches!(
                result,
                Some(PageAction::Configure(agent_runtime::Command::Configure {
                    enabled: true,
                    ..
                }))
            ));
        }
    }

    #[test]
    fn sdk_actions_stay_clickable_with_long_details_and_keep_readiness_gates() {
        for size in [egui::vec2(320.0, 360.0), egui::vec2(680.0, 620.0)] {
            for (active, bridge_ready) in [(false, true), (true, true), (false, false)] {
                let ctx = egui::Context::default();
                ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
                ctx.data_mut(|data| {
                    data.insert_temp(egui::Id::new("sdk-connection-details"), true)
                });
                let snapshot = crate::gui_sdk::Snapshot {
                    phase: if active { "active" } else { "off" },
                    retired: !active,
                    url: Some("ws://127.0.0.1:12345".into()),
                    endpoint_path: Some(std::path::PathBuf::from("C:/private/".repeat(80))),
                    error: None,
                };
                let mut origins = "https://trusted.example".to_owned();
                let mut frame = 0;
                let mut actions = BTreeSet::new();
                let mut sdk = true;
                let mut result = None;
                let mut render = |events| {
                    layout_frame(&ctx, size, events, |ui| {
                        access_tabs(ui, &mut sdk, None, (&mut frame, &mut actions));
                        if let Some(action) = sdk_controls(
                            ui,
                            &snapshot,
                            &mut origins,
                            bridge_ready,
                            None,
                            (&mut frame, &mut actions),
                        ) {
                            result = Some(action);
                        }
                    })
                };
                let _ = render(vec![]);
                let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
                let label = if active {
                    "Disable SDK connections"
                } else {
                    "Enable SDK connections"
                };
                let button = control_rect(&ctx, label, screen);
                control_rect(&ctx, "Agent Access tab", screen);
                render(click_events(button.center(), true));
                render(click_events(button.center(), false));
                assert_eq!(
                    result,
                    if active {
                        Some(SdkAction::Disable)
                    } else if bridge_ready {
                        Some(SdkAction::Enable)
                    } else {
                        None
                    }
                );
            }
        }
    }

    #[test]
    fn connection_entry_uses_the_gui_executable_and_private_descriptor_path() {
        let entry = mcp_server_entry(
            r"C:\Program Files\ProtoInputMultiPlex\protoinputmultiplex.exe",
            r"C:\Users\tester\AppData\Local\Temp\private descriptor.json",
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&entry).unwrap();
        assert_eq!(
            value["command"],
            r"C:\Program Files\ProtoInputMultiPlex\protoinputmultiplex.exe"
        );
        assert_eq!(
            value["args"],
            serde_json::json!([
                "mcp",
                "--gui-connection",
                r"C:\Users\tester\AppData\Local\Temp\private descriptor.json"
            ])
        );
        assert!(entry.contains("\\\\"));
    }

    #[test]
    fn connection_entry_is_unavailable_without_both_paths() {
        assert!(mcp_server_entry("", "C:\\descriptor.json").is_none());
        assert!(mcp_server_entry("C:\\app.exe", "").is_none());
        assert!(mcp_server_entry("  ", "  ").is_none());
    }

    #[test]
    fn descriptor_copy_feedback_requires_the_same_active_descriptor() {
        assert!(connection_feedback_is_current(
            true,
            r"C:\descriptor.json",
            Some(r"C:\descriptor.json")
        ));
        assert!(!connection_feedback_is_current(
            false,
            r"C:\descriptor.json",
            Some(r"C:\descriptor.json")
        ));
        assert!(!connection_feedback_is_current(
            true,
            "",
            Some(r"C:\descriptor.json")
        ));
        assert!(!connection_feedback_is_current(
            true,
            r"C:\new.json",
            Some(r"C:\old.json")
        ));
    }

    #[test]
    fn exact_path_text_preserves_valid_paths() {
        assert_eq!(
            exact_path_string(std::path::Path::new(
                r"C:\Program Files\ProtoInputMultiPlex\protoinputmultiplex.exe"
            )),
            Some(r"C:\Program Files\ProtoInputMultiPlex\protoinputmultiplex.exe".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn exact_path_text_rejects_non_unicode_paths() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        let path = std::path::Path::new(OsStr::from_bytes(b"/tmp/invalid-\xff"));
        assert!(exact_path_string(path).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn exact_path_text_rejects_unpaired_surrogates() {
        use std::{ffi::OsString, os::windows::ffi::OsStringExt};

        let path = std::path::PathBuf::from(OsString::from_wide(&[0xD800]));
        assert!(exact_path_string(&path).is_none());
    }

    #[test]
    fn desktop_capture_grant_is_separate_default_off_and_scriptable() {
        let mut page = AgentPage::default();
        assert!(!page.snapshot().desktop_capture);
        page.apply_script_fields(
            &BTreeMap::from([("agent_desktop_capture".to_owned(), "true".to_owned())]),
            &[],
        );
        assert!(page.snapshot().desktop_capture);
        assert!(!page.snapshot().capture);
        page.reset_session();
        assert!(!page.snapshot().desktop_capture);
    }

    #[test]
    fn passive_client_event_updates_cleanup_without_acknowledging_policy_draft() {
        let mut page = AgentPage {
            selected_users: BTreeSet::from(["draft-user".to_owned()]),
            allowed_pids: BTreeSet::from([321]),
            capture: true,
            desktop_capture: true,
            session_control: true,
            app_preferences: true,
            pending: true,
            ..AgentPage::default()
        };
        let cleanup = agent_runtime::ClientCleanupState {
            client: 47,
            pending_owners: 2,
            attempts: 1,
            last_error: "release is waiting for the device".to_owned(),
        };
        let state = agent_runtime::State {
            acknowledged: false,
            client: 0,
            policy: agent_runtime::Policy {
                enabled: true,
                user_ids: vec!["server-user".to_owned()],
                allowed_pids: vec![999],
                capture: false,
                desktop_capture: false,
                session_control: false,
                app_preferences: false,
            },
            cleanup: vec![cleanup.clone()],
            ..agent_runtime::State::default()
        };

        assert!(!page.receive(state));
        let snapshot = page.snapshot();

        assert!(snapshot.pending);
        assert_eq!(snapshot.selected_users, ["draft-user"]);
        assert_eq!(snapshot.allowed_pids, [321]);
        assert!(snapshot.capture);
        assert!(snapshot.desktop_capture);
        assert!(snapshot.session_control);
        assert!(snapshot.app_preferences);
        assert_eq!(snapshot.cleanup, [cleanup]);
    }
}
