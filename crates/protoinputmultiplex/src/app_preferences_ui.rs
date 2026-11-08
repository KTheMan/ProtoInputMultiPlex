//! Application-scoped launch, tray, and shortcut preferences.
use crate::{app_preferences, ui_script::Script};
use eframe::egui;
use ouroboros_ui::{
    Theme,
    atoms::{Button, ButtonVariant, Checkbox, Divider, Heading, Input, Text},
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub struct PreferencesPage {
    draft: Option<app_preferences::Preferences>,
    shortcut_text: BTreeMap<app_preferences::Action, String>,
    enabled_actions: BTreeSet<app_preferences::Action>,
    error: Option<String>,
    notice: Option<String>,
    pending: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EditorFingerprint {
    draft: Option<app_preferences::Preferences>,
    shortcut_text: BTreeMap<app_preferences::Action, String>,
    enabled_actions: BTreeSet<app_preferences::Action>,
}

/// Snapshot of editor state associated with one submitted preferences update.
/// This is deliberately local UI state and is never serialized.
#[derive(Clone, Debug)]
pub struct PreferencesEditorSubmission {
    fingerprint: EditorFingerprint,
    was_clean: bool,
}

impl PreferencesPage {
    pub fn reset(&mut self, preferences: &app_preferences::Preferences) {
        self.draft = Some(preferences.clone());
        self.enabled_actions = preferences
            .shortcuts
            .iter()
            .map(|shortcut| shortcut.action)
            .collect();
        self.error = None;
        self.notice = None;
        self.pending = false;
        self.sync_shortcut_text();
    }

    fn fingerprint(&self) -> EditorFingerprint {
        EditorFingerprint {
            draft: self.draft.clone(),
            shortcut_text: self.shortcut_text.clone(),
            enabled_actions: self.enabled_actions.clone(),
        }
    }

    /// Capture the page before submitting an update against `old_committed`.
    pub fn capture_for_submit(
        &self,
        old_committed: &app_preferences::Preferences,
    ) -> PreferencesEditorSubmission {
        let fingerprint = self.fingerprint();
        let baseline = Self::fingerprint_for(old_committed);
        PreferencesEditorSubmission {
            was_clean: fingerprint == baseline,
            fingerprint,
        }
    }

    fn fingerprint_for(preferences: &app_preferences::Preferences) -> EditorFingerprint {
        let mut raw_shortcuts = BTreeMap::new();
        for action in actions() {
            let text = preferences
                .shortcuts
                .iter()
                .find(|shortcut| shortcut.action == action)
                .map(shortcut_text)
                .unwrap_or_default();
            raw_shortcuts.insert(action, text);
        }
        EditorFingerprint {
            draft: Some(preferences.clone()),
            shortcut_text: raw_shortcuts,
            enabled_actions: preferences
                .shortcuts
                .iter()
                .map(|shortcut| shortcut.action)
                .collect(),
        }
    }

    /// Apply a completed update without discarding edits made after submission.
    pub fn completed(
        &mut self,
        candidate: &app_preferences::Preferences,
        submitted: &PreferencesEditorSubmission,
        local_origin: bool,
    ) {
        self.pending = false;
        self.error = None;
        if self.fingerprint() == submitted.fingerprint && (local_origin || submitted.was_clean) {
            self.reset(candidate);
        } else {
            self.notice = Some(
                "Saved preferences changed while this update was in progress. Your edits are preserved; Revert loads the latest preferences."
                    .to_owned(),
            );
        }
    }

    pub fn set_pending(&mut self, pending: bool) {
        self.pending = pending;
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Return the complete editor state for the Home/other-page raw UI snapshot.
    /// The field names are stable for snapshot consumers; this contains no tokens.
    pub fn raw_state(&self) -> serde_json::Value {
        serde_json::json!({
            "draft": self.draft,
            "shortcut_text": self.shortcut_text,
            "enabled_actions": self.enabled_actions,
            "notice": self.notice,
        })
    }

    pub fn draft(&self) -> Option<&app_preferences::Preferences> {
        self.draft.as_ref()
    }

    pub fn set_error(&mut self, error: impl Into<String>) {
        self.error = Some(error.into());
    }

    fn sync_shortcut_text(&mut self) {
        self.shortcut_text.clear();
        if let Some(draft) = &self.draft {
            self.enabled_actions = draft
                .shortcuts
                .iter()
                .map(|shortcut| shortcut.action)
                .collect();
            for action in actions() {
                let text = draft
                    .shortcuts
                    .iter()
                    .find(|shortcut| shortcut.action == action)
                    .map(shortcut_text)
                    .unwrap_or_default();
                self.shortcut_text.insert(action, text);
            }
        }
    }

    pub fn apply_script_fields(&mut self, fields: &BTreeMap<String, String>) {
        let Some(draft) = self.draft.as_mut() else {
            return;
        };
        for (key, value) in fields {
            match key.as_str() {
                "app_show_app_launcher" => {
                    draft.show_app_launcher = parse_bool(value, draft.show_app_launcher)
                }
                "app_launch_minimized" => {
                    draft.launch_minimized = parse_bool(value, draft.launch_minimized)
                }
                "app_minimize_to_tray" => {
                    draft.minimize_to_tray = parse_bool(value, draft.minimize_to_tray)
                }
                "app_close_to_tray" => draft.close_to_tray = parse_bool(value, draft.close_to_tray),
                "app_launch_at_login" => {
                    draft.launch_at_login = parse_bool(value, draft.launch_at_login)
                }
                "app_load_profile_at_startup" => {
                    draft.load_profile_at_startup = parse_bool(value, draft.load_profile_at_startup)
                }
                "app_save_profile_on_exit" => {
                    draft.save_profile_on_exit = parse_bool(value, draft.save_profile_on_exit)
                }
                "app_appearance_mode" => match value.to_ascii_lowercase().as_str() {
                    "dark" => draft.appearance_mode = app_preferences::AppearanceMode::Dark,
                    "light" => draft.appearance_mode = app_preferences::AppearanceMode::Light,
                    _ => (),
                },
                "app_zoom_percent" => {
                    if let Ok(zoom) = value.parse() {
                        draft.zoom_percent = zoom;
                    }
                }
                name if name.starts_with("app_shortcut_") => {
                    if let Some(action) = action_by_name(&name[13..]) {
                        self.shortcut_text.insert(action, value.clone());
                    }
                }
                _ => (),
            }
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        accepted: (
            &app_preferences::Preferences,
            &crate::app_lifecycle::ShortcutStatus,
        ),
        can_edit: bool,
        script: Option<&mut Script>,
        action_frame: &mut u64,
        frame_actions: &mut BTreeSet<String>,
    ) -> Option<app_preferences::Preferences> {
        let (committed, shortcut_status) = accepted;
        if self.draft.is_none() {
            self.reset(committed);
        }
        let mut script = script;
        let mut apply = None;
        let mut revert = false;
        let mut candidate = self
            .draft
            .as_ref()
            .cloned()
            .unwrap_or_else(|| committed.clone());
        candidate.shortcuts.clear();
        let mut validation_error = None;
        let available_height = ui.available_height();
        egui::ScrollArea::vertical().id_salt("app-preferences-body")
            .max_height((available_height - 88.0).max(96.0))
            .show(ui, |ui| {
            Heading::new("Application preferences").heading().show(ui);
            Text::new("Window behavior and global shortcuts for ProtoInputMultiPlex. These settings are separate from user profiles and never start routing automatically.")
                .caption().wrap().show(ui);
            if !shortcut_status.active {
                Text::new("Saved shortcuts are inactive. Retry from Home, or correct the shortcuts below and Apply.")
                    .wrap().show(ui);
                if let Some(error) = &shortcut_status.error {
                    ui.colored_label(Theme::get(ui).error, error);
                }
            }
            if let Some(error) = &self.error {
                ui.colored_label(Theme::get(ui).error, error);
            }
            if let Some(notice) = &self.notice {
                Text::new(notice).caption().wrap().show(ui);
            }
            let Some(draft) = self.draft.as_mut() else { return };
            Heading::new("Appearance").heading().show(ui);
            ui.horizontal(|ui| {
                for (mode, label) in [
                    (app_preferences::AppearanceMode::Dark, "Dark"),
                    (app_preferences::AppearanceMode::Light, "Light"),
                ] {
                    let observer = format!("Appearance mode {label}");
                    let variant = if draft.appearance_mode == mode {
                        ButtonVariant::Default
                    } else {
                        ButtonVariant::Secondary
                    };
                    let response = Button::new(label)
                        .variant(variant)
                        .enabled(can_edit)
                        .show(ui);
                    let accepted = script
                        .as_deref_mut()
                        .is_none_or(|s| s.observe(&observer, &response));
                    if response.clicked()
                        && accepted
                        && consume(action_frame, frame_actions, ui.ctx(), &observer)
                    {
                        draft.appearance_mode = mode;
                    }
                }
            });
            ui.horizontal(|ui| {
                for (label, observer, zoom) in [
                    ("−10%", "Zoom minus 10", None),
                    ("75%", "Zoom preset 75%", Some(75)),
                    ("+10%", "Zoom plus 10", None),
                ] {
                    let response = Button::new(label)
                        .variant(ButtonVariant::Secondary)
                        .enabled(can_edit)
                        .show(ui);
                    let accepted = script
                        .as_deref_mut()
                        .is_none_or(|s| s.observe(observer, &response));
                    if response.clicked()
                        && accepted
                        && consume(action_frame, frame_actions, ui.ctx(), observer)
                    {
                        draft.zoom_percent = match (observer, zoom) {
                            (_, Some(value)) => value,
                            ("Zoom minus 10", _) => draft
                                .zoom_percent
                                .saturating_sub(10)
                                .max(app_preferences::MIN_ZOOM_PERCENT),
                            _ => draft
                                .zoom_percent
                                .saturating_add(10)
                                .min(app_preferences::MAX_ZOOM_PERCENT),
                        };
                    }
                }
                ui.label(format!("{}%", draft.zoom_percent));
            });
            Text::new("Zoom changes the interface scale. A small window may grow to keep controls usable.")
                .caption()
                .wrap()
                .show(ui);

            checkbox(
                ui,
                "Show Apps launcher",
                &mut draft.show_app_launcher,
                can_edit,
                script.as_deref_mut(),
                action_frame,
                frame_actions,
            );
            checkbox(
                ui,
                "Launch minimized",
                &mut draft.launch_minimized,
                can_edit,
                script.as_deref_mut(),
                action_frame,
                frame_actions,
            );
            checkbox(
                ui,
                "Minimize to tray",
                &mut draft.minimize_to_tray,
                can_edit,
                script.as_deref_mut(),
                action_frame,
                frame_actions,
            );
            checkbox(
                ui,
                "Close to tray",
                &mut draft.close_to_tray,
                can_edit,
                script.as_deref_mut(),
                action_frame,
                frame_actions,
            );
            let response = Checkbox::new(&mut draft.launch_at_login)
                .label("Launch at login (local only)")
                .enabled(can_edit)
                .show(ui);
            let label = "Launch at login (local only)";
            let accepted = script.as_deref_mut().is_none_or(|s| s.observe(label, &response));
            if response.changed() && accepted && consume(action_frame, frame_actions, ui.ctx(), label) {
                self.error = None;
            }
            Text::new("Local Apply updates this user's startup registration. Connected agents can inspect it but cannot change it.")
                .caption().wrap().show(ui);

            Divider::horizontal().show(ui);
            Heading::new("Profile persistence").heading().show(ui);
            checkbox(
                ui,
                "Load saved profile at startup",
                &mut draft.load_profile_at_startup,
                can_edit,
                script.as_deref_mut(),
                action_frame,
                frame_actions,
            );
            checkbox(
                ui,
                "Save applied profile on exit",
                &mut draft.save_profile_on_exit,
                can_edit,
                script.as_deref_mut(),
                action_frame,
                frame_actions,
            );
            Text::new("Save applied settings to the active profile when the app closes. Unsubmitted edits are excluded. Hide and Minimize do not save.")
                .caption().wrap().show(ui);
            if !draft.load_profile_at_startup && draft.save_profile_on_exit {
                Text::new("With startup loading off, exit replaces the active file with this session's setup. Previous contents are backed up.")
                    .caption().wrap().show(ui);
            }

            Divider::horizontal().show(ui);
            Heading::new("Keyboard shortcuts").heading().show(ui);
            Text::new("Ctrl+Alt+Shift+F11 is the shared emergency Stop shortcut and stops all participating ProtoInputMultiPlex sessions. Other shortcuts are exclusive to this app. Each action has one unique chord; use Ctrl, Alt, or Windows with one key.")
                .caption().wrap().show(ui);
            for action in actions() {
                let mut enabled = self.enabled_actions.contains(&action);
                let action_label = action_label(action);
                let compact = ui.available_width() < 510.0;
                let mut row = |ui: &mut egui::Ui| {
                    let control = format!("Shortcut {action_label}");
                    let response = if compact {
                        Checkbox::new(&mut enabled)
                            .label(action_label)
                            .enabled(can_edit)
                            .show(ui)
                    } else {
                        ui.allocate_ui_with_layout(
                            egui::vec2(300.0, 32.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                // Child layouts otherwise shrink to each label's width.
                                ui.set_min_width(300.0);
                                Checkbox::new(&mut enabled)
                                    .label(action_label)
                                    .enabled(can_edit)
                                    .show(ui)
                            },
                        )
                        .inner
                    };
                    let accepted = script.as_deref_mut().is_none_or(|s| s.observe(&control, &response));
                    if response.changed() && accepted && consume(action_frame, frame_actions, ui.ctx(), &control) {
                        if enabled {
                            self.enabled_actions.insert(action);
                            let text = self.shortcut_text.entry(action).or_default();
                            if text.is_empty() { *text = "Ctrl+Alt+F10".into(); }
                        } else {
                            self.enabled_actions.remove(&action);
                        }
                        self.error = None;
                    }
                    let text = self.shortcut_text.entry(action).or_default();
                    let input_changed = if compact {
                        ui.add_enabled_ui(can_edit && enabled, |ui| {
                            Input::new(text)
                                .sm()
                                .id_source(("app_shortcut", action))
                                .show(ui)
                        })
                        .inner
                        .changed()
                    } else {
                        ui.allocate_ui_with_layout(
                            egui::vec2(184.0, 32.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                ui.set_min_width(184.0);
                                ui.add_enabled_ui(can_edit && enabled, |ui| {
                                    Input::new(text)
                                        .sm()
                                        .id_source(("app_shortcut", action))
                                        .show(ui)
                                })
                                .inner
                                .changed()
                            },
                        )
                        .inner
                    };
                    if input_changed {
                        self.error = None;
                    }
                    if enabled {
                        match parse_shortcut(action, text) {
                            Ok(shortcut) => candidate.shortcuts.push(shortcut),
                            Err(error) => validation_error = Some(format!("{error:#}")),
                        }
                    }
                };
                if compact {
                    ui.vertical(&mut row);
                } else {
                    ui.horizontal(&mut row);
                }
            }
            if !can_edit {
                ui.weak(if self.pending {
                    "Application preference operation in progress."
                } else {
                    "Wait for the pending application action or routing shutdown to finish."
                });
            }
        });
        // The candidate is assembled above from parsed shortcut fields. Copy the
        // current window flags after widgets mutate them so an Apply click in this
        // frame cannot commit stale values.
        if let Some(draft) = self.draft.as_ref() {
            candidate.appearance_mode = draft.appearance_mode;
            candidate.zoom_percent = draft.zoom_percent;
            candidate.show_app_launcher = draft.show_app_launcher;
            candidate.launch_minimized = draft.launch_minimized;
            candidate.minimize_to_tray = draft.minimize_to_tray;
            candidate.close_to_tray = draft.close_to_tray;
            candidate.launch_at_login = draft.launch_at_login;
            candidate.load_profile_at_startup = draft.load_profile_at_startup;
            candidate.save_profile_on_exit = draft.save_profile_on_exit;
        }
        if validation_error.is_none() {
            validation_error = candidate.validate().err().map(|error| format!("{error:#}"));
        }
        if let Some(error) = validation_error.as_ref().or(self.error.as_ref()) {
            ui.add(
                egui::Label::new(egui::RichText::new(error).color(Theme::get(ui).error)).truncate(),
            )
            .on_hover_text(error);
        }
        ui.add_space(4.0);
        Divider::horizontal().show(ui);
        ui.horizontal(|ui| {
            let observer = "Apply application preferences";
            let response = Button::new("Apply")
                .variant(ButtonVariant::Secondary)
                .enabled(can_edit && validation_error.is_none())
                .show(ui);
            let accepted = script
                .as_deref_mut()
                .is_none_or(|s| s.observe(observer, &response));
            if response.clicked()
                && accepted
                && consume(action_frame, frame_actions, ui.ctx(), observer)
            {
                apply = Some(candidate.clone());
            }
            let observer = "Revert application preferences";
            let response = Button::new("Revert").enabled(can_edit).show(ui);
            let accepted = script.take().is_none_or(|s| s.observe(observer, &response));
            if response.clicked()
                && accepted
                && consume(action_frame, frame_actions, ui.ctx(), observer)
            {
                revert = true;
            }
        });
        if revert {
            self.reset(committed);
        }
        apply
    }
}

fn actions() -> [app_preferences::Action; 7] {
    use app_preferences::Action::*;
    [
        Stop,
        PauseResume,
        ToggleRouting,
        Switched,
        Concurrent,
        ToggleMode,
        ToggleKeyboard,
    ]
}
fn action_label(action: app_preferences::Action) -> &'static str {
    use app_preferences::Action::*;
    match action {
        Stop => "Stop routing",
        PauseResume => "Pause or resume",
        ToggleMode => "Toggle switched/concurrent",
        ToggleRouting => "Toggle routing",
        Switched => "Use switched mode",
        Concurrent => "Use concurrent mode",
        ToggleKeyboard => "Toggle multi-keyboard",
    }
}
fn action_by_name(value: &str) -> Option<app_preferences::Action> {
    actions()
        .into_iter()
        .find(|action| format!("{action:?}").eq_ignore_ascii_case(value))
}
fn shortcut_text(shortcut: &app_preferences::Shortcut) -> String {
    let mut parts = Vec::new();
    if shortcut.modifiers.control {
        parts.push("Ctrl".to_owned());
    }
    if shortcut.modifiers.alt {
        parts.push("Alt".to_owned());
    }
    if shortcut.modifiers.shift {
        parts.push("Shift".to_owned());
    }
    if shortcut.modifiers.windows {
        parts.push("Win".to_owned());
    }
    parts.push(format!("{:?}", shortcut.key));
    parts.join("+")
}
fn parse_shortcut(
    action: app_preferences::Action,
    text: &str,
) -> anyhow::Result<app_preferences::Shortcut> {
    let mut modifiers = app_preferences::Modifiers::default();
    let mut key = None;
    for part in text
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers.control = true,
            "alt" => modifiers.alt = true,
            "shift" => modifiers.shift = true,
            "win" | "windows" => modifiers.windows = true,
            name => {
                anyhow::ensure!(key.is_none(), "Enter exactly one non-modifier shortcut key");
                key = Some(
                    parse_key(name)
                        .ok_or_else(|| anyhow::anyhow!("Unsupported shortcut key: {part}"))?,
                )
            }
        }
    }
    let key = key.ok_or_else(|| anyhow::anyhow!("Enter one key with Ctrl, Alt, or Windows"))?;
    anyhow::ensure!(
        modifiers.control || modifiers.alt || modifiers.windows,
        "Include Ctrl, Alt, or Windows in the shortcut"
    );
    Ok(app_preferences::Shortcut {
        action,
        modifiers,
        key,
    })
}
fn parse_key(name: &str) -> Option<app_preferences::Key> {
    use app_preferences::Key::*;
    let name = name.to_ascii_lowercase();
    Some(match name.as_str() {
        "a" => A,
        "b" => B,
        "c" => C,
        "d" => D,
        "e" => E,
        "f" => F,
        "g" => G,
        "h" => H,
        "i" => I,
        "j" => J,
        "k" => K,
        "l" => L,
        "m" => M,
        "n" => N,
        "o" => O,
        "p" => P,
        "q" => Q,
        "r" => R,
        "s" => S,
        "t" => T,
        "u" => U,
        "v" => V,
        "w" => W,
        "x" => X,
        "y" => Y,
        "z" => Z,
        "0" | "digit0" => Digit0,
        "1" | "digit1" => Digit1,
        "2" | "digit2" => Digit2,
        "3" | "digit3" => Digit3,
        "4" | "digit4" => Digit4,
        "5" | "digit5" => Digit5,
        "6" | "digit6" => Digit6,
        "7" | "digit7" => Digit7,
        "8" | "digit8" => Digit8,
        "9" | "digit9" => Digit9,
        "f1" => F1,
        "f2" => F2,
        "f3" => F3,
        "f4" => F4,
        "f5" => F5,
        "f6" => F6,
        "f7" => F7,
        "f8" => F8,
        "f9" => F9,
        "f10" => F10,
        "f11" => F11,
        "f12" => F12,
        "f13" => F13,
        "f14" => F14,
        "f15" => F15,
        "f16" => F16,
        "f17" => F17,
        "f18" => F18,
        "f19" => F19,
        "f20" => F20,
        "f21" => F21,
        "f22" => F22,
        "f23" => F23,
        "f24" => F24,
        "escape" => Escape,
        "enter" => Enter,
        "space" => Space,
        "tab" => Tab,
        "backspace" => Backspace,
        "delete" => Delete,
        "insert" => Insert,
        "home" => Home,
        "end" => End,
        "pageup" => PageUp,
        "pagedown" => PageDown,
        "left" => Left,
        "right" => Right,
        "up" => Up,
        "down" => Down,
        _ => return None,
    })
}
fn parse_bool(value: &str, previous: bool) -> bool {
    value.parse().unwrap_or(previous)
}
fn checkbox(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut bool,
    enabled: bool,
    script: Option<&mut Script>,
    frame: &mut u64,
    actions: &mut BTreeSet<String>,
) {
    let response = Checkbox::new(value).label(label).enabled(enabled).show(ui);
    let accepted = script.is_none_or(|s| s.observe(label, &response));
    if response.changed() && accepted {
        consume(frame, actions, ui.ctx(), label);
    }
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
    let consumed = actions.insert(label.to_owned());
    if consumed {
        ctx.request_repaint();
    }
    consumed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preferences_with_launch_minimized(value: bool) -> app_preferences::Preferences {
        app_preferences::Preferences {
            launch_minimized: value,
            ..Default::default()
        }
    }

    #[test]
    fn shortcut_parser_requires_exactly_one_key_and_a_safe_modifier() {
        assert!(parse_shortcut(app_preferences::Action::Stop, "Ctrl+Alt+F11").is_ok());
        assert!(parse_shortcut(app_preferences::Action::Stop, "Ctrl+A+B").is_err());
        assert!(parse_shortcut(app_preferences::Action::Stop, "F11").is_err());
    }

    #[test]
    fn clean_page_adopts_external_completion() {
        let old = app_preferences::Preferences::default();
        let latest = preferences_with_launch_minimized(true);
        let mut page = PreferencesPage::default();
        page.reset(&old);
        let submitted = page.capture_for_submit(&old);

        page.completed(&latest, &submitted, false);

        assert_eq!(page.draft(), Some(&latest));
        assert!(page.notice().is_none());
    }

    #[test]
    fn dirty_valid_editor_state_survives_external_completion() {
        let old = app_preferences::Preferences::default();
        let latest = preferences_with_launch_minimized(true);
        let mut page = PreferencesPage::default();
        page.reset(&old);
        page.draft.as_mut().unwrap().zoom_percent = 125;
        page.shortcut_text
            .insert(app_preferences::Action::Stop, "Ctrl+Alt+F10".into());
        let submitted = page.capture_for_submit(&old);

        page.completed(&latest, &submitted, false);

        assert_eq!(page.draft().unwrap().zoom_percent, 125);
        assert_eq!(
            page.shortcut_text.get(&app_preferences::Action::Stop),
            Some(&"Ctrl+Alt+F10".to_owned())
        );
        assert!(page.notice().is_some());
    }

    #[test]
    fn invalid_raw_shortcut_survives_external_completion() {
        let old = app_preferences::Preferences::default();
        let latest = preferences_with_launch_minimized(true);
        let mut page = PreferencesPage::default();
        page.reset(&old);
        page.shortcut_text
            .insert(app_preferences::Action::Stop, "Ctrl+".into());
        page.set_error("obsolete transaction error");
        let submitted = page.capture_for_submit(&old);

        page.completed(&latest, &submitted, false);

        assert_eq!(
            page.shortcut_text.get(&app_preferences::Action::Stop),
            Some(&"Ctrl+".to_owned())
        );
        assert_eq!(page.draft(), Some(&old));
        assert!(page.notice().is_some());
        assert!(page.error.is_none());
    }

    #[test]
    fn raw_shortcut_edit_during_local_pending_ack_survives() {
        let old = app_preferences::Preferences::default();
        let latest = preferences_with_launch_minimized(true);
        let mut page = PreferencesPage::default();
        page.reset(&old);
        let submitted = page.capture_for_submit(&old);
        page.shortcut_text
            .insert(app_preferences::Action::Stop, "Ctrl+".into());

        page.completed(&latest, &submitted, true);

        assert_eq!(
            page.shortcut_text.get(&app_preferences::Action::Stop),
            Some(&"Ctrl+".to_owned())
        );
        assert!(page.notice().is_some());
    }

    #[test]
    fn reset_reverts_to_latest_preferences_and_clears_notice() {
        let old = app_preferences::Preferences::default();
        let latest = preferences_with_launch_minimized(true);
        let mut page = PreferencesPage::default();
        page.reset(&old);
        page.shortcut_text
            .insert(app_preferences::Action::Stop, "Ctrl+".into());
        let submitted = page.capture_for_submit(&old);
        page.completed(&latest, &submitted, false);

        page.reset(&latest);

        assert_eq!(page.draft(), Some(&latest));
        assert!(page.notice().is_none());
        assert_eq!(
            page.shortcut_text.get(&app_preferences::Action::Stop),
            Some(&"Ctrl+Alt+Shift+F11".to_owned())
        );
    }
}
