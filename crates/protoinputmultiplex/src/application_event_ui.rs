//! Ouroboros UI for a separate default-off cooperative event authority.
use crate::{gui_application_events::Controller, ui_script::Script};
use eframe::egui;
use mux_application_sdk::{
    ClientCommand, CursorControl, Input as VirtualInput, MouseButton, WheelAxis,
};
use ouroboros_ui::{
    atoms::{Button, ButtonVariant, Checkbox, Divider, Heading, Input, Text},
    tokens::core,
};
use std::collections::BTreeSet;

pub(crate) enum Action {
    CreateSession,
    CreateUser,
    AddReceiver,
    SelectAllVisibleUsers,
    ApplyOwner,
    Apply,
    DisablePublication,
    StartListener,
    Issue,
    Copy,
    Revoke,
    Stop,
    Local(ClientCommand),
    Move,
    KeyPulse,
}

fn consume(ui: &egui::Ui, label: &str, frame: &mut u64, actions: &mut BTreeSet<String>) -> bool {
    let current = ui.ctx().cumulative_frame_nr();
    if *frame != current {
        *frame = current;
        actions.clear();
    }
    actions.insert(label.into())
}
fn button(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    script: &mut Option<&mut Script>,
    frame: &mut u64,
    actions: &mut BTreeSet<String>,
) -> bool {
    let response = Button::new(label)
        .sm()
        .variant(ButtonVariant::Outline)
        .enabled(enabled)
        .show(ui);
    let accepted = script
        .as_deref_mut()
        .is_none_or(|script| script.observe(label, &response));
    response.clicked() && accepted && consume(ui, label, frame, actions)
}
fn checkbox(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut bool,
    script: &mut Option<&mut Script>,
    frame: &mut u64,
    actions: &mut BTreeSet<String>,
) {
    let mut draft = *value;
    let response = Checkbox::new(&mut draft).label(label).show(ui);
    let accepted = script
        .as_deref_mut()
        .is_none_or(|script| script.observe(label, &response));
    if response.changed() && accepted && consume(ui, label, frame, actions) {
        *value = draft;
    }
}
fn input(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    enabled: bool,
    script: &mut Option<&mut Script>,
) {
    Text::new(label).caption().show(ui);
    ui.add_enabled_ui(enabled, |ui| {
        let response = Input::new(value).sm().id_source(label).show(ui);
        if let Some(script) = script.as_deref_mut() {
            script.observe(label, &response);
        }
    });
}

pub(crate) fn sdk_kind_tabs(
    ui: &mut egui::Ui,
    application: &mut bool,
    script: Option<&mut Script>,
    frame: &mut u64,
    actions: &mut BTreeSet<String>,
) {
    let mut script = script;
    ui.horizontal(|ui| {
        for (label, control, selected) in [
            ("Command connections", "Command SDK section", false),
            ("Application events", "Application event SDK section", true),
        ] {
            let response = Button::new(label)
                .sm()
                .variant(if *application == selected {
                    ButtonVariant::Secondary
                } else {
                    ButtonVariant::Ghost
                })
                .show(ui);
            let accepted = script
                .as_deref_mut()
                .is_none_or(|script| script.observe(control, &response));
            if response.clicked() && accepted && consume(ui, control, frame, actions) {
                *application = selected;
            }
        }
    });
}

pub(crate) fn show(
    ui: &mut egui::Ui,
    controller: &mut Controller,
    allowed: bool,
    script: Option<&mut Script>,
    frame: &mut u64,
    actions: &mut BTreeSet<String>,
) -> Vec<Action> {
    let mut script = script;
    let mut result = Vec::new();
    let status = controller.status();
    Heading::new("Application event SDK").show(ui);
    Text::new("A separate virtual-only cooperative session. Event export, keyboard observation and every remote control start off. Command SDK permissions do not grant these approvals.").caption().wrap().show(ui);
    let network = match (status.connected_clients, status.cleanup_pending) {
        (Some(connected), Some(pending)) => {
            format!("{connected} event receiver connections · {pending} pending cleanup")
        }
        _ => "Listener metrics unavailable".into(),
    };
    let summary = Text::new(format!(
        "{} · {} local users · {} native observed users · sequence {} · {}",
        status.phase,
        status.local_virtual_users,
        status.native_observed_users,
        status.native_sequence,
        network
    ))
    .caption()
    .wrap()
    .show(ui);
    if let Some(script) = script.as_deref_mut() {
        script.observe("Application event SDK status", &summary);
    }
    if let Some(error) = &controller.error {
        Text::new(error)
            .caption()
            .color(core::RED_500)
            .wrap()
            .show(ui);
    }
    if button(
        ui,
        "Create cooperative event session",
        allowed && controller.retired(),
        &mut script,
        frame,
        actions,
    ) {
        result.push(Action::CreateSession);
    }
    if button(
        ui,
        "End cooperative event session",
        controller.active() || !controller.retired(),
        &mut script,
        frame,
        actions,
    ) {
        result.push(Action::Stop);
    }
    Divider::horizontal().show(ui);
    let edit_user_name = allowed && controller.active();
    input(
        ui,
        "Application event user name",
        &mut controller.draft.name,
        edit_user_name,
        &mut script,
    );
    if button(
        ui,
        "Create cooperative virtual user",
        allowed && controller.active() && controller.users.len() < 64,
        &mut script,
        frame,
        actions,
    ) {
        result.push(Action::CreateUser);
    }
    Divider::horizontal().show(ui);
    Text::new("Master local-owner consent · applies to all locally owned users")
        .caption()
        .wrap()
        .show(ui);
    ui.add_enabled_ui(allowed && controller.active(), |ui| {
        for (label, value) in [
            (
                "Master pointer observation",
                &mut controller.owner_consent.pointer_events,
            ),
            (
                "Master keyboard observation separately",
                &mut controller.owner_consent.key_events,
            ),
            (
                "Master lifecycle controls",
                &mut controller.owner_consent.lifecycle,
            ),
            (
                "Master cursor controls",
                &mut controller.owner_consent.cursor,
            ),
            (
                "Master pointer input",
                &mut controller.owner_consent.pointer_control,
            ),
            (
                "Master keyboard input separately",
                &mut controller.owner_consent.key_control,
            ),
        ] {
            checkbox(ui, label, value, &mut script, frame, actions);
        }
    });
    Text::new("Applying master consent revokes every event receiver before changing owner authority. Reapprove receivers explicitly afterward.")
        .caption().wrap().show(ui);
    if button(
        ui,
        "Apply master event consent",
        allowed && controller.active(),
        &mut script,
        frame,
        actions,
    ) {
        result.push(Action::ApplyOwner);
    }
    if button(
        ui,
        "Disable application event publication",
        controller.active() && (status.pointer_export || status.keyboard_export),
        &mut script,
        frame,
        actions,
    ) {
        result.push(Action::DisablePublication);
    }
    Divider::horizontal().show(ui);
    Text::new(format!(
        "Event receivers (native/browser) · {} approved · up to 8 rows",
        status.approved_receivers
    ))
    .caption()
    .wrap()
    .show(ui);
    if button(
        ui,
        "Add event receiver",
        allowed
            && controller.active()
            && controller.origin_editable()
            && controller.receiver_rows().len() < 8,
        &mut script,
        frame,
        actions,
    ) {
        result.push(Action::AddReceiver);
    }
    let selected_row = controller.selected_row();
    ui.horizontal_wrapped(|ui| {
        for (row, approved) in controller.receiver_rows() {
            let label = format!(
                "Event receiver {row} · {}{}",
                if approved { "approved" } else { "draft" },
                if selected_row == Some(row) {
                    " · selected"
                } else {
                    ""
                }
            );
            if button(
                ui,
                &label,
                allowed && controller.active(),
                &mut script,
                frame,
                actions,
            ) && let Err(error) = controller.select_receiver(row)
            {
                controller.error = Some(error.to_string());
            }
        }
    });
    Text::new("Selected receiver details. Each row has independent origin, exact user scope, observation, delegation and control approvals. Configure all receiver rows and origins before starting the listener; adding origins or rows then requires a new session.")
        .caption().wrap().show(ui);
    let editable = allowed
        && controller.active()
        && controller.selected_row().is_some()
        && controller.origin_editable();
    input(
        ui,
        "Exact application event receiver origin",
        &mut controller.draft.origin,
        editable,
        &mut script,
    );
    Text::new("Select only users this event receiver (native/browser) may observe. Creation never selects or grants a user automatically.").caption().wrap().show(ui);
    if button(
        ui,
        "Select all visible users",
        allowed && controller.active() && controller.selected_row().is_some(),
        &mut script,
        frame,
        actions,
    ) {
        result.push(Action::SelectAllVisibleUsers);
    }
    Text::new("Selects only the current users in this draft. New users stay unselected; consent and Apply are still required.")
        .caption().wrap().show(ui);
    for user in &controller.users {
        let label = format!(
            "Export cooperative user {}:{} {}",
            user.id.epoch, user.id.local, user.name
        );
        let mut selected = controller.draft.users.contains(&user.id);
        checkbox(ui, &label, &mut selected, &mut script, frame, actions);
        if selected {
            controller.draft.users.insert(user.id);
        } else {
            controller.draft.users.remove(&user.id);
        }
        let focus = format!(
            "Control cooperative user {}:{}",
            user.id.epoch, user.id.local
        );
        if button(ui, &focus, controller.active(), &mut script, frame, actions) {
            controller.draft.focus = Some(user.id);
        }
    }
    checkbox(
        ui,
        "Approve pointer event observation",
        &mut controller.draft.pointer_events,
        &mut script,
        frame,
        actions,
    );
    checkbox(
        ui,
        "Approve keyboard event observation separately",
        &mut controller.draft.key_events,
        &mut script,
        frame,
        actions,
    );
    checkbox(
        ui,
        "Allow delegated event receiver controls for selected users",
        &mut controller.draft.delegated,
        &mut script,
        frame,
        actions,
    );
    checkbox(
        ui,
        "Allow cooperative user lifecycle controls",
        &mut controller.draft.lifecycle,
        &mut script,
        frame,
        actions,
    );
    checkbox(
        ui,
        "Allow cooperative cursor controls",
        &mut controller.draft.cursor,
        &mut script,
        frame,
        actions,
    );
    checkbox(
        ui,
        "Allow cooperative pointer input",
        &mut controller.draft.pointer_control,
        &mut script,
        frame,
        actions,
    );
    checkbox(
        ui,
        "Allow cooperative keyboard input separately",
        &mut controller.draft.key_control,
        &mut script,
        frame,
        actions,
    );
    Text::new("Draft checkboxes change no live authority. Apply replaces only the selected receiver and retires its previous connection. Other receiver approvals, master consent and local virtual user IDs remain stable.").caption().wrap().show(ui);
    if button(
        ui,
        "Apply selected event receiver approvals",
        allowed && controller.active() && controller.selected_row().is_some(),
        &mut script,
        frame,
        actions,
    ) {
        result.push(Action::Apply);
    }
    if button(
        ui,
        "Revoke selected event receiver",
        controller.selected_approved(),
        &mut script,
        frame,
        actions,
    ) {
        result.push(Action::Revoke);
    }
    Divider::horizontal().show(ui);
    input(
        ui,
        "Event connection lifetime in seconds (1-300)",
        &mut controller.draft.ttl,
        true,
        &mut script,
    );
    if button(
        ui,
        "Start application event listener",
        allowed && controller.can_start_listener(),
        &mut script,
        frame,
        actions,
    ) {
        result.push(Action::StartListener);
    }
    if button(
        ui,
        "Issue private event-only connection",
        allowed && controller.can_issue(),
        &mut script,
        frame,
        actions,
    ) {
        result.push(Action::Issue);
    }
    if button(
        ui,
        "Copy private application event connection",
        allowed
            && controller.selected_approved()
            && controller.listener_active()
            && controller.connection_issued(),
        &mut script,
        frame,
        actions,
    ) {
        result.push(Action::Copy);
    }
    Text::new("Connection details are returned only to the explicit GUI Copy action. Status, Script observations and MCP contain no event credential. Reconnect requires a fresh approved receiver.").caption().wrap().show(ui);
    Divider::horizontal().show(ui);
    Text::new("Explicit native cooperative input (select a Control cooperative user button first)")
        .caption()
        .wrap()
        .show(ui);
    let pointer = allowed && controller.can_control(false, false);
    let keys = allowed && controller.can_control(true, false);
    let cursor = allowed && controller.can_control(false, true);
    if button(
        ui,
        "Move cooperative virtual pointer",
        pointer,
        &mut script,
        frame,
        actions,
    ) {
        result.push(Action::Move);
    }
    if let Some(user) = controller.draft.focus {
        if button(
            ui,
            "Click cooperative primary button",
            pointer,
            &mut script,
            frame,
            actions,
        ) {
            result.push(Action::Local(ClientCommand::Click {
                user,
                button: MouseButton::Primary,
            }));
        }
        for (label, axis) in [
            ("Cooperative vertical wheel +120", WheelAxis::Vertical),
            ("Cooperative horizontal wheel +120", WheelAxis::Horizontal),
        ] {
            if button(ui, label, pointer, &mut script, frame, actions) {
                result.push(Action::Local(ClientCommand::Input {
                    user,
                    input: VirtualInput::Wheel { axis, units: 120 },
                }));
            }
        }
        for (label, color) in [
            ("Set cooperative cursor blue", [40, 150, 255, 255]),
            ("Set cooperative cursor orange", [255, 150, 40, 255]),
        ] {
            if button(ui, label, cursor, &mut script, frame, actions) {
                result.push(Action::Local(ClientCommand::Cursor {
                    user,
                    control: CursorControl::Color(color),
                }));
            }
        }
    }
    if button(
        ui,
        "Send cooperative Key A pulse",
        keys,
        &mut script,
        frame,
        actions,
    ) {
        result.push(Action::KeyPulse);
    }
    for user in controller.native.users().values() {
        let color = user
            .cursor
            .as_ref()
            .map_or([160, 160, 160, 255], |cursor| cursor.color);
        Text::new(format!(
            "{} [{}:{}] · {:?} · {} held keys",
            user.name,
            user.id.epoch,
            user.id.local,
            user.pointer.as_ref().map(|pointer| pointer.position),
            user.held_keys.as_ref().map_or(0, Vec::len)
        ))
        .caption()
        .color(egui::Color32::from_rgba_unmultiplied(
            color[0], color[1], color[2], color[3],
        ))
        .wrap()
        .show(ui);
    }
    ui.add_enabled_ui(false, |ui| {
        let _ = Button::new("Event receiver control of GUI native windows unavailable")
            .sm()
            .enabled(false)
            .show(ui);
        let _ = Button::new("Physical input, pen, touch and target injection unavailable")
            .sm()
            .enabled(false)
            .show(ui);
    });
    result
}
