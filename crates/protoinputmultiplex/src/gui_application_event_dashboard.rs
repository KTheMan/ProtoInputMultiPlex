//! Child of gui: integrates cooperative authority without exposing private host state.
use super::*;

impl Dashboard {
    pub(super) fn all_sdks_retired(&self) -> bool {
        self.sdk_retired() && self.application_events.retired()
    }
    pub(super) fn stop_all_sdks(&mut self) {
        self.application_events.stop();
        self.stop_sdk();
    }

    pub(super) fn poll_application_events(&mut self, ctx: &egui::Context) {
        self.application_events.tick(ctx);
        self.app_request_handle
            .publish_application_events(self.application_events.status());
        // The private slot is armed only by the locally observed Copy action.
        if let Some(connection) = self.application_events.take_copy() {
            ctx.copy_text(connection);
            self.status =
                "Private application-event connection copied for the approved browser origin."
                    .into();
        }
    }

    pub(super) fn application_event_controls(&mut self, ui: &mut egui::Ui) {
        let allowed = !self.quit_after_stop
            && !self.preferences_quit_waiting
            && !self.session_stopping
            && !self.restart_requested
            && !self.allow_close;
        let actions = crate::application_event_ui::show(
            ui,
            &mut self.application_events,
            allowed,
            self.script.as_mut(),
            &mut self.action_frame,
            &mut self.frame_actions,
        );
        for action in actions {
            use crate::application_event_ui::Action;
            let result = match action {
                Action::CreateSession => self
                    .desktop_bounds
                    .context("Desktop bounds are not available yet")
                    .and_then(|bounds| self.application_events.create_session(bounds)),
                Action::CreateUser => self.application_events.create_user(),
                Action::Apply => self.application_events.approve(),
                Action::DisablePublication => self.application_events.disable_publication(),
                Action::StartListener => self
                    .app_preferences_path
                    .parent()
                    .context("Private application data directory is unavailable")
                    .and_then(|parent| {
                        self.application_events
                            .start_listener(parent.to_path_buf(), Arc::clone(&self._runtime_guards))
                    }),
                Action::Issue => self.application_events.issue(),
                Action::Copy => self.application_events.request_copy(),
                Action::Revoke => {
                    self.application_events.revoke();
                    Ok(())
                }
                Action::Stop => {
                    self.application_events.stop();
                    Ok(())
                }
                Action::Local(command) => self.application_events.local_command(command),
                Action::Move => self.application_events.move_focused(),
                Action::KeyPulse => self.application_events.key_pulse(),
            };
            if let Err(error) = result {
                let message = format!("Application event SDK: {error:#}");
                self.application_events.error = Some(message.clone());
                self.status = message;
            } else {
                self.application_events.error = None;
            }
        }
    }
}
