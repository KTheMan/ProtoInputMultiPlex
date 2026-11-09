//! Child of gui: integrates cooperative authority without exposing private host state.
use super::*;
use mux_application_sdk::{
    Failure, FailureCode, HostViewportAction, HostedAction, HostedMode, HostedPhase, HostedState,
};
use mux_core::routing::Mode as RoutingMode;

const HOST_GUI_TIMEOUT: Duration = Duration::from_secs(54);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum HostStage {
    Start,
    Stop,
    Worker,
    Viewport,
}

pub(super) struct HostedPending {
    pub ticket: u64,
    pub action: HostedAction,
    pub stage: HostStage,
    pub expected_generation: Option<u64>,
    pub started_frame: u64,
    pub deadline: Instant,
    pub previous_mode: Option<RoutingMode>,
    pub worker_complete: bool,
}

fn host_failure(code: FailureCode, message: &str) -> Failure {
    Failure {
        code,
        message: message.into(),
    }
}

fn virtual_observation_seat(seat: &mux_core::Seat) -> bool {
    matches!(mux_core::emulated::parse(&seat.mouse), Ok(Some(device)) if device.kind == mux_core::DeviceKind::Mouse)
        && seat.keyboard.as_ref().is_none_or(|path|
            matches!(mux_core::emulated::parse(path), Ok(Some(device)) if device.kind == mux_core::DeviceKind::Keyboard))
}

fn viewport_observed(
    action: HostViewportAction,
    minimized: Option<bool>,
    visible: Option<bool>,
) -> bool {
    match action {
        HostViewportAction::Minimize => minimized == Some(true),
        HostViewportAction::Restore => minimized == Some(false) && visible == Some(true),
    }
}

impl Dashboard {
    pub(super) fn all_sdks_retired(&self) -> bool {
        self.sdk_retired() && self.application_events.retired()
    }
    pub(super) fn stop_all_sdks(&mut self) {
        self.application_events.stop();
        self.application_events
            .poll_desktop_sources(self.session.as_ref());
        self.stop_sdk();
    }

    pub(super) fn poll_application_events(
        &mut self,
        ctx: &egui::Context,
        viewport_visible: Option<bool>,
    ) {
        if self.application_events.active() {
            self.hosted_visible = viewport_visible;
            self.publish_hosted_observation(ctx);
        } else {
            if let Some(pending) = self.hosted_pending.as_ref()
                && pending.stage == HostStage::Start
                && self.session.is_none()
            {
                self.restart_requested = false;
                if let (Some(previous), Some(options)) =
                    (pending.previous_mode, self.profile.desktop.as_mut())
                {
                    options.mode = previous;
                }
            }
            self.hosted_state = None;
            self.hosted_visible = None;
            self.hosted_pending = None;
        }
        self.application_events
            .poll_desktop_sources(self.session.as_ref());
        self.application_events.tick(ctx);
        self.poll_hosted_pending(ctx);
        if let Some(intent) = self.application_events.take_host_intent() {
            self.begin_hosted_intent(intent, ctx);
        }
        if self.hosted_pending.is_some() {
            ctx.request_repaint_after(Duration::from_millis(40));
        }
        self.app_request_handle
            .publish_application_events(self.application_events.status());
        // The private slot is armed only by the locally observed Copy action.
        if let Some(connection) = self.application_events.take_copy() {
            ctx.copy_text(connection);
            self.status =
                "Private application-event connection copied for the selected event receiver (native/browser)."
                    .into();
        }
    }

    pub(super) fn hosted_start_preserves_event(&self) -> bool {
        self.hosted_pending.as_ref().is_some_and(|pending| {
            pending.stage == HostStage::Start
                && self.application_events.host_action_pending(pending.ticket)
        })
    }

    fn publish_hosted_observation(&mut self, ctx: &egui::Context) {
        let recovery = self.recovery.snapshot();
        let (phase, mode, multi_keyboard) =
            if recovery.phase == crate::session_recovery::Phase::Quarantined {
                (HostedPhase::Quarantined, None, None)
            } else if self.session_stopping
                || recovery.phase == crate::session_recovery::Phase::Stopping
            {
                (HostedPhase::Stopping, None, None)
            } else if self.session.is_none() {
                if recovery.can_start {
                    (HostedPhase::Native, Some(HostedMode::Native), None)
                } else {
                    (HostedPhase::Unavailable, None, None)
                }
            } else if !self.session_ready {
                (HostedPhase::Starting, None, None)
            } else if let Some(options) = self.profile.desktop.as_ref() {
                let mode = match options.mode {
                    RoutingMode::Switched => HostedMode::Switched,
                    RoutingMode::Concurrent => HostedMode::Concurrent,
                };
                (HostedPhase::Ready, Some(mode), Some(options.multi_keyboard))
            } else {
                (HostedPhase::Assigned, None, None)
            };
        let mut next = HostedState {
            revision: self.hosted_state.as_ref().map_or(1, |state| state.revision),
            routing_generation: self.agent_session_epoch,
            phase,
            mode,
            multi_keyboard,
            minimized: ctx.input(|input| input.viewport().minimized),
        };
        if self.hosted_state.as_ref() == Some(&next) {
            return;
        }
        if let Some(previous) = &self.hosted_state {
            let Some(revision) = previous.revision.checked_add(1) else {
                self.application_events.stop();
                self.hosted_state = None;
                self.status = "Host state sequence is exhausted; event connections ended.".into();
                return;
            };
            next.revision = revision;
        }
        if let Err(error) = self.application_events.publish_hosted_state(next.clone()) {
            self.application_events.stop();
            self.hosted_state = None;
            self.status = format!("Host state publication ended: {error:#}");
            return;
        }
        self.hosted_state = Some(next);
    }

    fn poll_hosted_pending(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.hosted_pending.take() else {
            return;
        };
        if !self.application_events.host_action_pending(pending.ticket) {
            if pending.stage == HostStage::Start && self.session.is_none() {
                self.restart_requested = false;
                if let (Some(previous), Some(options)) =
                    (pending.previous_mode, self.profile.desktop.as_mut())
                {
                    options.mode = previous;
                }
            }
            return;
        }
        if Instant::now() >= pending.deadline {
            if pending.stage == HostStage::Start && self.session.is_none() {
                self.restart_requested = false;
            }
            self.application_events.complete_host_action(
                pending.ticket,
                Err(host_failure(
                    FailureCode::Uncertain,
                    "Host action outcome is unconfirmed; inspect fresh state",
                )),
            );
            return;
        }
        let Some(state) = self.hosted_state.clone() else {
            self.hosted_pending = Some(pending);
            return;
        };
        let completed = match (pending.stage, pending.action) {
            (HostStage::Viewport, HostedAction::Viewport(action)) => {
                ctx.cumulative_frame_nr() > pending.started_frame
                    && viewport_observed(action, state.minimized, self.hosted_visible)
            }
            (HostStage::Stop, HostedAction::SetMode(HostedMode::Native)) => {
                pending.expected_generation == Some(state.routing_generation)
                    && state.phase == HostedPhase::Native
                    && state.mode == Some(HostedMode::Native)
            }
            (HostStage::Start, HostedAction::SetMode(mode)) => {
                pending.expected_generation == Some(state.routing_generation)
                    && state.phase == HostedPhase::Ready
                    && state.mode == Some(mode)
            }
            (HostStage::Worker, HostedAction::SetMode(mode)) => {
                pending.worker_complete
                    && pending.expected_generation == Some(state.routing_generation)
                    && state.phase == HostedPhase::Ready
                    && state.mode == Some(mode)
            }
            _ => false,
        };
        if completed {
            self.application_events
                .complete_host_action(pending.ticket, Ok(state));
        } else if matches!(
            state.phase,
            HostedPhase::Quarantined | HostedPhase::Unavailable
        ) && pending.stage != HostStage::Viewport
        {
            self.application_events.complete_host_action(
                pending.ticket,
                Err(host_failure(
                    FailureCode::Uncertain,
                    "Routing cleanup or startup is unconfirmed; inspect recovery",
                )),
            );
        } else {
            self.hosted_pending = Some(pending);
        }
    }

    fn begin_hosted_intent(
        &mut self,
        intent: crate::application_event_transport::HostIntent,
        ctx: &egui::Context,
    ) {
        let reject = |self_: &mut Self, code, message| {
            self_
                .application_events
                .complete_host_action(intent.ticket, Err(host_failure(code, message)));
        };
        let Some(state) = self.hosted_state.clone() else {
            reject(
                self,
                FailureCode::Unsupported,
                "Host observation is unavailable",
            );
            return;
        };
        if self.hosted_pending.is_some() {
            reject(self, FailureCode::Limit, "Another host action is pending");
            return;
        }
        if intent.reference.revision != state.revision
            || intent.reference.epoch != self.application_events.epoch()
        {
            reject(self, FailureCode::StaleRevision, "Host observation changed");
            return;
        }
        if self.quit_after_stop || self.preferences_quit_waiting || self.allow_close {
            reject(self, FailureCode::Ended, "This window is closing");
            return;
        }
        let pending = HostedPending {
            ticket: intent.ticket,
            action: intent.action,
            stage: HostStage::Viewport,
            expected_generation: None,
            started_frame: ctx.cumulative_frame_nr(),
            deadline: Instant::now() + HOST_GUI_TIMEOUT,
            previous_mode: None,
            worker_complete: false,
        };
        match intent.action {
            HostedAction::Viewport(action) => {
                if viewport_observed(action, state.minimized, self.hosted_visible) {
                    self.application_events
                        .complete_host_action(intent.ticket, Ok(state));
                    return;
                }
                self.hosted_pending = Some(pending);
                match action {
                    HostViewportAction::Minimize => {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                    }
                    HostViewportAction::Restore => self.handle_lifecycle_action(
                        crate::app_lifecycle::LifecycleAction::Restore,
                        ctx,
                    ),
                }
            }
            HostedAction::SetMode(HostedMode::Native) => {
                if state.phase == HostedPhase::Native {
                    self.application_events
                        .complete_host_action(intent.ticket, Ok(state));
                } else if self.session.is_some()
                    && !self.session_stopping
                    && state.phase != HostedPhase::Quarantined
                {
                    if self
                        .application_events
                        .retain_only_host_action_receiver(intent.ticket)
                        .is_err()
                    {
                        reject(self, FailureCode::Ended, "Host action receiver retired");
                        return;
                    }
                    self.hosted_pending = Some(HostedPending {
                        stage: HostStage::Stop,
                        expected_generation: Some(self.agent_session_epoch),
                        ..pending
                    });
                    self.request_routing_stop_with_event(false, true);
                } else {
                    reject(self, FailureCode::Denied, "Routing is not ready to stop");
                }
            }
            HostedAction::SetMode(mode) => {
                let requested = match mode {
                    HostedMode::Switched => RoutingMode::Switched,
                    HostedMode::Concurrent => RoutingMode::Concurrent,
                    HostedMode::Native => unreachable!(),
                };
                let Some(options) = self.profile.desktop.as_ref() else {
                    reject(
                        self,
                        FailureCode::Unsupported,
                        "Automatic desktop routing is not configured",
                    );
                    return;
                };
                if state.phase == HostedPhase::Ready && options.mode == requested {
                    self.application_events
                        .complete_host_action(intent.ticket, Ok(state));
                    return;
                }
                let multi_keyboard = options.multi_keyboard;
                if self.session.is_some() {
                    if !self.session_ready || self.session_busy || self.session_stopping {
                        reject(self, FailureCode::Denied, "Routing is busy");
                        return;
                    }
                    self.hosted_pending = Some(HostedPending {
                        stage: HostStage::Worker,
                        expected_generation: Some(self.agent_session_epoch),
                        ..pending
                    });
                    if let Some(session) = &self.session {
                        session.request(Request::HostedMode {
                            ticket: intent.ticket,
                            mode: requested,
                            multi_keyboard,
                        });
                        self.session_busy = true;
                    }
                } else if state.phase == HostedPhase::Native {
                    if self
                        .application_events
                        .retain_only_host_action_receiver(intent.ticket)
                        .is_err()
                    {
                        reject(self, FailureCode::Ended, "Host action receiver retired");
                        return;
                    }
                    let previous = options.mode;
                    self.profile.desktop.as_mut().unwrap().mode = requested;
                    self.hosted_pending = Some(HostedPending {
                        stage: HostStage::Start,
                        previous_mode: Some(previous),
                        ..pending
                    });
                    self.start_routing_with_event(true);
                    if self.session.is_none() && !self.restart_requested {
                        self.profile.desktop.as_mut().unwrap().mode = previous;
                        self.hosted_pending = None;
                        reject(
                            self,
                            FailureCode::Uncertain,
                            "Routing did not reach startup; inspect current state",
                        );
                    }
                } else {
                    reject(
                        self,
                        FailureCode::Denied,
                        "Routing is not ready to change mode",
                    );
                }
            }
        }
    }

    pub(super) fn receive_hosted_mode_completion(
        &mut self,
        ticket: u64,
        mode: RoutingMode,
        multi_keyboard: bool,
        error: Option<String>,
        uncertain: bool,
    ) {
        if error.is_none()
            && let Some(options) = self.profile.desktop.as_mut()
        {
            options.mode = mode;
            options.multi_keyboard = multi_keyboard;
        }
        let Some(pending) = self.hosted_pending.as_mut() else {
            return;
        };
        if pending.ticket != ticket
            || pending.stage != HostStage::Worker
            || pending.expected_generation != Some(self.agent_session_epoch)
        {
            return;
        }
        if let Some(error) = error {
            self.application_events.complete_host_action(
                ticket,
                Err(host_failure(
                    if uncertain {
                        FailureCode::Uncertain
                    } else {
                        FailureCode::Denied
                    },
                    &error,
                )),
            );
            self.hosted_pending = None;
        } else {
            pending.worker_complete = true;
        }
    }

    pub(super) fn application_event_controls(&mut self, ui: &mut egui::Ui) {
        let allowed = !self.quit_after_stop
            && !self.preferences_quit_waiting
            && !self.session_stopping
            && !self.restart_requested
            && !self.allow_close;
        let desktop_available = allowed
            && self.session.is_some()
            && self.session_ready
            && self.profile.desktop.is_some();
        let desktop_candidates =
            self.virtual_users_page
                .snapshot()
                .state
                .map_or_else(Vec::new, |state| {
                    state
                        .slots
                        .into_iter()
                        .filter(|slot| slot.active && virtual_observation_seat(&slot.seat))
                        .map(|slot| (slot.seat.identity(), slot.seat.name))
                        .collect::<Vec<_>>()
                });
        let actions = crate::application_event_ui::show(
            ui,
            &mut self.application_events,
            allowed,
            crate::application_event_ui::DesktopSelection {
                candidates: &desktop_candidates,
                available: desktop_available,
            },
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
                    .and_then(|bounds| {
                        let mut screens = self
                            .monitors
                            .iter()
                            .map(|monitor| monitor.bounds)
                            .collect::<Vec<_>>();
                        if screens.is_empty() {
                            screens.push(bounds);
                        }
                        screens.sort_by_key(|bounds| {
                            (bounds.left, bounds.top, bounds.right, bounds.bottom)
                        });
                        self.application_events.create_session_with_screens(
                            mux_application_sdk::ScreenState {
                                revision: 1,
                                desktop: bounds,
                                screens,
                            },
                        )
                    }),
                Action::BindDesktop { identity, name } => {
                    if desktop_available {
                        self.session
                            .as_ref()
                            .context("Desktop routing is unavailable")
                            .and_then(|worker| {
                                self.application_events
                                    .bind_desktop_source(worker, identity, name)
                            })
                    } else {
                        Err(anyhow::anyhow!(
                            "Start a ready virtual desktop routing session first"
                        ))
                    }
                }
                Action::UnbindDesktop(token) => self
                    .session
                    .as_ref()
                    .context("Desktop routing is unavailable")
                    .and_then(|worker| {
                        self.application_events.unbind_desktop_source(worker, token)
                    }),
                Action::CreateUser => self.application_events.create_user(),
                Action::AddReceiver => self.application_events.add_receiver(),
                Action::SelectAllVisibleUsers => self.application_events.select_all_visible_users(),
                Action::ApplyOwner => self.application_events.apply_owner_consent(),
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
                Action::CopyWindowReference => self
                    .application_events
                    .selected_window_reference()
                    .map(|reference| ui.ctx().copy_text(reference)),
                Action::Revoke => {
                    self.application_events.revoke();
                    Ok(())
                }
                Action::Stop => {
                    self.application_events.stop();
                    self.application_events
                        .poll_desktop_sources(self.session.as_ref());
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

#[cfg(test)]
mod hosted_viewport_tests {
    use super::*;

    #[test]
    fn restore_requires_observed_visibility_even_when_not_minimized() {
        assert!(!viewport_observed(
            HostViewportAction::Restore,
            Some(false),
            Some(false),
        ));
        assert!(!viewport_observed(
            HostViewportAction::Restore,
            Some(false),
            None,
        ));
        assert!(viewport_observed(
            HostViewportAction::Restore,
            Some(false),
            Some(true),
        ));
    }
}
