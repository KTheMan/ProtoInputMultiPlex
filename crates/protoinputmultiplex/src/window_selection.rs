//! Transient, identity-bound selection. This is not a saved profile assignment.
use rmcp::schemars;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    /// Internal issuer/sequence correlation; not a routing_input payload field.
    pub request_id: String,
    pub user: usize,
    pub pid: u32,
    pub hwnd: usize,
    pub creation_token: String,
}

impl Selection {
    pub fn issue(&mut self) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        self.request_id = format!(
            "{}:{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Acknowledgement {
    pub selection: Selection,
    pub error: Option<String>,
}

pub fn apply(
    selection: &Selection,
    automation: &crate::macro_studio::Runtime,
    router: &mut dyn crate::routing_engine::Engine,
) -> anyhow::Result<()> {
    automation.require_selection_idle(selection.user)?;
    router.select_window(selection)
}

pub fn intersect_client(
    client: mux_core::desktop::Bounds,
    configured: mux_core::desktop::Bounds,
) -> anyhow::Result<mux_core::desktop::Bounds> {
    use anyhow::Context;
    client
        .intersection(configured)
        .context("Selected window does not intersect this user's configured bounds")
}

/// Selection is passive until one accepted input crosses the attachment boundary.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentState {
    #[default]
    Selected,
    Attached {
        positioned: bool,
    },
}
impl AttachmentState {
    pub fn unbound(self) -> bool {
        matches!(self, Self::Selected)
    }
    pub fn needs_position(self) -> bool {
        !matches!(self, Self::Attached { positioned: true })
    }
    pub fn permits_publication(self) -> bool {
        !self.unbound()
    }
    pub fn positioned(&mut self) {
        *self = Self::Attached { positioned: true };
    }
    pub fn on_real_input(
        &mut self,
        attach: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        if self.unbound() {
            attach()?;
            *self = Self::Attached { positioned: false };
        }
        Ok(())
    }
    pub fn prospective_point(
        self,
        current: (i32, i32),
        bounds: mux_core::desktop::Bounds,
    ) -> (i32, i32) {
        if self.unbound() {
            (
                current.0.clamp(bounds.left, bounds.right - 1),
                current.1.clamp(bounds.top, bounds.bottom - 1),
            )
        } else {
            current
        }
    }
}

pub fn require_independent_keyboards(enabled: bool) -> anyhow::Result<()> {
    anyhow::ensure!(
        enabled,
        "Enable independent keyboards before selecting a window"
    );
    Ok(())
}
pub fn validate_keyboard_mode(
    enabled: bool,
    users: impl IntoIterator<Item = (bool, AttachmentState)>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        enabled
            || !users
                .into_iter()
                .any(|(active, state)| active && state.unbound()),
        "Route input to the selected windows or Stop before turning off independent keyboards"
    );
    Ok(())
}
/// Both worker and connected mode changes use this ordering before cancelling programs.
pub fn change_mode(
    router: &mut dyn crate::routing_engine::Engine,
    mode: mux_core::routing::Mode,
    multi_keyboard: bool,
    before_change: impl FnOnce(&mut dyn crate::routing_engine::Engine) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    router.validate_mode_change(multi_keyboard)?;
    before_change(router)?;
    router.mode(mode, multi_keyboard)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mux_core::desktop::Bounds;
    #[test]
    fn selection_bounds_reject_disjoint_and_clamp_distant_cursor_inside_intersection() {
        let client = Bounds {
            left: 1000,
            top: 100,
            right: 1400,
            bottom: 600,
        };
        let configured = Bounds {
            left: 1100,
            top: 0,
            right: 1800,
            bottom: 450,
        };
        let intersection = intersect_client(client, configured).unwrap();
        assert!(intersection.contains(
            20.clamp(intersection.left, intersection.right - 1),
            30.clamp(intersection.top, intersection.bottom - 1)
        ));
        assert_eq!(intersection.left, 1100);
        assert_eq!(intersection.bottom, 450);
        assert!(
            intersect_client(
                client,
                Bounds {
                    left: 0,
                    top: 0,
                    right: 500,
                    bottom: 500
                }
            )
            .is_err()
        );
    }
    #[test]
    fn passive_selection_fences_publication_until_successful_real_input_and_keeps_retry_identity() {
        let mut state = AttachmentState::Selected;
        let original = state;
        let mut attempts = 0;
        for _ in 0..5 {
            assert!(!state.permits_publication());
            assert_eq!(state, original);
        }
        assert!(
            state
                .on_real_input(|| {
                    attempts += 1;
                    anyhow::bail!("expected original exited")
                })
                .is_err()
        );
        assert_eq!(state, original);
        state
            .on_real_input(|| {
                attempts += 1;
                Ok(())
            })
            .unwrap();
        assert!(state.permits_publication());
        assert!(state.needs_position());
        state
            .on_real_input(|| panic!("must not attach twice"))
            .unwrap();
        state.positioned();
        assert!(!state.needs_position());
        assert_eq!(attempts, 2);
    }
    #[test]
    fn prospective_mouse_point_uses_first_delivery_intersection_without_mutating_cursor() {
        let actual = (20, 30);
        let client = Bounds {
            left: 1000,
            top: 100,
            right: 1400,
            bottom: 600,
        };
        let bounds = intersect_client(
            client,
            Bounds {
                left: 1100,
                top: 0,
                right: 1800,
                bottom: 450,
            },
        )
        .unwrap();
        let prospective = AttachmentState::Selected.prospective_point(actual, bounds);
        assert_eq!(prospective, (1100, 100));
        assert_eq!(actual, (20, 30));
        assert!(client.contains(prospective.0, prospective.1));
        assert!(bounds.contains(prospective.0, prospective.1));
        assert_eq!(
            AttachmentState::Attached { positioned: true }.prospective_point(actual, bounds),
            actual
        );
    }
    #[test]
    fn independent_keyboard_gate_ignores_retired_selections_and_rejects_live_pending_users() {
        assert!(require_independent_keyboards(false).is_err());
        require_independent_keyboards(true).unwrap();
        validate_keyboard_mode(
            false,
            [
                (false, AttachmentState::Selected),
                (true, AttachmentState::Attached { positioned: true }),
            ],
        )
        .unwrap();
        assert!(validate_keyboard_mode(false, [(true, AttachmentState::Selected)]).is_err());
    }
    #[test]
    fn rejected_mode_change_does_not_cancel_release_or_stop_recording() {
        use crate::routing_engine::Engine;
        use std::{cell::RefCell, rc::Rc};
        struct Router {
            events: Rc<RefCell<Vec<&'static str>>>,
        }
        impl Engine for Router {
            fn validate_mode_change(&self, _: bool) -> anyhow::Result<()> {
                self.events.borrow_mut().push("validate");
                anyhow::bail!("pending selection")
            }
            fn mode(&mut self, _: mux_core::routing::Mode, _: bool) -> anyhow::Result<()> {
                self.events.borrow_mut().push("mode");
                Ok(())
            }
            fn input(&mut self, _: mux_core::emulated::Packet) -> anyhow::Result<()> {
                unreachable!()
            }
            fn key_held(&self, _: u32, _: u32) -> bool {
                false
            }
            fn button_held(&self, _: u32, _: u32) -> bool {
                false
            }
            fn paused(&self) -> bool {
                false
            }
            fn release_all(&mut self) -> anyhow::Result<()> {
                self.events.borrow_mut().push("release");
                Ok(())
            }
            fn pause(&mut self, _: bool) -> anyhow::Result<()> {
                unreachable!()
            }
            fn settings(
                &mut self,
                _: usize,
                _: mux_core::desktop::CursorSettings,
            ) -> anyhow::Result<()> {
                unreachable!()
            }
            fn tick(&mut self) -> anyhow::Result<(bool, Vec<crate::router::State>)> {
                unreachable!()
            }
        }
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut router = Router {
            events: events.clone(),
        };
        assert!(
            change_mode(
                &mut router,
                mux_core::routing::Mode::Concurrent,
                false,
                |router| {
                    events.borrow_mut().push("cancel/recording");
                    router.release_all()
                }
            )
            .is_err()
        );
        assert_eq!(*events.borrow(), ["validate"]);
    }
}
