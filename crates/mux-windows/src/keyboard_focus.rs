//! Read-only, application-scoped keyboard recipient selection.
use anyhow::{Context, Result, ensure};
use mux_core::{
    keyboard::{KeyboardFocus, KeyboardSettings},
    routing::Window,
};
use serde::{Deserialize, Serialize};
use windows_sys::Win32::{
    Foundation::HWND,
    UI::{
        Input::KeyboardAndMouse::IsWindowEnabled,
        WindowsAndMessaging::{
            GA_ROOT, GUITHREADINFO, GW_OWNER, GetAncestor, GetGUIThreadInfo, GetWindow,
            GetWindowThreadProcessId,
        },
    },
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingState {
    pub settings: KeyboardSettings,
    pub clicked: Option<Window>,
    pub resolved: Option<Window>,
    pub fallback: bool,
    pub status: String,
}

impl RoutingState {
    /// A queried candidate is diagnostic state, never a delivery acknowledgement.
    pub fn observe(settings: KeyboardSettings, clicked: Option<Window>) -> Self {
        let result = clicked
            .context("Click an eligible window before typing")
            .and_then(|window| resolve(window, settings.focus));
        match result {
            Ok(resolution) => Self {
                settings,
                clicked,
                resolved: Some(resolution.resolved),
                fallback: resolution.fallback.is_some(),
                status: format!(
                    "Read-only recipient candidate; {}{}",
                    if settings.enabled {
                        "keyboard source enabled"
                    } else {
                        "keyboard source disabled"
                    },
                    resolution
                        .fallback
                        .map_or_else(String::new, |reason| format!("; {reason}"))
                ),
            },
            Err(error) => Self {
                settings,
                clicked,
                resolved: None,
                fallback: false,
                status: format!("Keyboard recipient unavailable: {error:#}"),
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct Resolution {
    pub clicked: Window,
    pub resolved: Window,
    pub fallback: Option<String>,
    pub attempts: usize,
}

#[derive(Clone, Copy, Default)]
struct ThreadWindows {
    focused: usize,
    active: usize,
    caret: usize,
}

/// Keep the original keyboard recipient across attachment effects and, for a
/// logical Text action, across all of its UTF-16 units.
pub struct RetainedRecipient {
    clicked: Window,
    resolved: Window,
    process: crate::targets::ProcessIdentity,
}

impl RetainedRecipient {
    pub fn capture(clicked: Window, policy: KeyboardFocus) -> Result<Self> {
        let process = crate::targets::ProcessIdentity::open(clicked.pid)?;
        let resolved = resolve(clicked, policy)?.resolved;
        let recipient = Self {
            clicked,
            resolved,
            process,
        };
        recipient.validate()?;
        Ok(recipient)
    }

    /// Revalidate the retained recipient; never resolve a replacement on error.
    pub fn validate(&self) -> Result<Window> {
        self.process.ensure_alive()?;
        let resolved = validate_candidate(self.clicked, self.resolved.hwnd)?;
        ensure!(
            resolved == self.resolved,
            "Keyboard recipient identity changed"
        );
        self.process.ensure_alive()?;
        Ok(resolved)
    }
}

/// Every query addresses the clicked window's own GUI thread. This never reads
/// the foreground thread, activates a window, or changes Windows keyboard focus.
pub fn resolve(clicked: Window, policy: KeyboardFocus) -> Result<Resolution> {
    resolve_with(
        clicked,
        policy,
        || {
            validate_clicked(clicked)?;
            let thread =
                unsafe { GetWindowThreadProcessId(clicked.hwnd as HWND, std::ptr::null_mut()) };
            ensure!(thread != 0, "clicked window has no live GUI thread");
            let mut info: GUITHREADINFO = unsafe { std::mem::zeroed() };
            info.cbSize = std::mem::size_of_val(&info) as u32;
            ensure!(
                unsafe { GetGUIThreadInfo(thread, &mut info) } != 0,
                "GUI thread information unavailable: {}",
                std::io::Error::last_os_error()
            );
            Ok(ThreadWindows {
                focused: info.hwndFocus as usize,
                active: info.hwndActive as usize,
                caret: info.hwndCaret as usize,
            })
        },
        |candidate| validate_candidate(clicked, candidate),
        || validate_clicked(clicked),
    )
}

fn validate_clicked(clicked: Window) -> Result<()> {
    ensure!(
        clicked.pid != 0 && clicked.hwnd != 0,
        "clicked window is unavailable"
    );
    crate::targets::client_bounds(clicked.hwnd, clicked.pid)?;
    Ok(())
}

fn validate_candidate(clicked: Window, candidate: usize) -> Result<Window> {
    ensure!(candidate != 0, "requested control is unavailable");
    validate_clicked(clicked)?;
    // client_bounds verifies live PID, visibility, non-minimized root and usable bounds.
    crate::targets::client_bounds(candidate, clicked.pid)?;
    ensure!(
        unsafe { IsWindowEnabled(candidate as HWND) } != 0,
        "requested control is disabled"
    );
    let mut candidate_pid = 0;
    unsafe {
        GetWindowThreadProcessId(candidate as HWND, &mut candidate_pid);
    }
    let candidate = Window {
        hwnd: candidate,
        pid: candidate_pid,
    };
    let clicked_root = unsafe { GetAncestor(clicked.hwnd as HWND, GA_ROOT) };
    let mut root = unsafe { GetAncestor(candidate.hwnd as HWND, GA_ROOT) };
    ensure!(
        !clicked_root.is_null() && !root.is_null(),
        "window root is unavailable"
    );
    // An owned popup is eligible only when its owner chain reaches the clicked
    // root without crossing a process boundary. Sibling application roots fail.
    let mut owners = Vec::new();
    for _ in 0..32 {
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(root, &mut pid);
        }
        owners.push(Window {
            hwnd: root as usize,
            pid,
        });
        if root == clicked_root || pid != clicked.pid {
            break;
        }
        let owner = unsafe { GetWindow(root, GW_OWNER) };
        if owner.is_null() {
            break;
        }
        root = unsafe { GetAncestor(owner, GA_ROOT) };
        if root.is_null() {
            break;
        }
    }
    validate_relationship(clicked, clicked_root as usize, candidate, &owners)?;
    // Recheck live identity and visibility after walking the owner chain.
    crate::targets::client_bounds(candidate.hwnd, clicked.pid)?;
    validate_clicked(clicked)?;
    Ok(candidate)
}

fn validate_relationship(
    clicked: Window,
    clicked_root: usize,
    candidate: Window,
    owners: &[Window],
) -> Result<()> {
    ensure!(
        candidate.hwnd != 0 && candidate.pid == clicked.pid,
        "requested control belongs to a foreign process or is stale"
    );
    for owner in owners {
        ensure!(
            owner.pid == clicked.pid,
            "requested control owner belongs to a foreign process"
        );
        if owner.hwnd == clicked_root {
            return Ok(());
        }
    }
    anyhow::bail!("requested control is outside the clicked window and its owned popups")
}

fn resolve_with(
    clicked: Window,
    policy: KeyboardFocus,
    mut query: impl FnMut() -> Result<ThreadWindows>,
    mut validate: impl FnMut(usize) -> Result<Window>,
    mut validate_base: impl FnMut() -> Result<()>,
) -> Result<Resolution> {
    validate_base()?;
    if policy == KeyboardFocus::Clicked {
        return Ok(Resolution {
            clicked,
            resolved: clicked,
            fallback: None,
            attempts: 0,
        });
    }
    let limit = if policy == KeyboardFocus::Focused {
        3
    } else {
        1
    };
    let mut reason = String::new();
    for attempt in 1..=limit {
        let info = match query() {
            Ok(info) => info,
            Err(error) => {
                reason = format!("GUI lookup failed: {error:#}");
                continue;
            }
        };
        let candidate = match policy {
            KeyboardFocus::Focused | KeyboardFocus::Native => info.focused,
            KeyboardFocus::Active => info.active,
            KeyboardFocus::Caret => info.caret,
            KeyboardFocus::Clicked => unreachable!(),
        };
        match validate(candidate) {
            Ok(resolved) => {
                return Ok(Resolution {
                    clicked,
                    resolved,
                    fallback: None,
                    attempts: attempt,
                });
            }
            Err(error) => reason = format!("requested recipient unavailable: {error:#}"),
        }
        if policy == KeyboardFocus::Caret {
            match validate(info.focused) {
                Ok(resolved) => {
                    return Ok(Resolution {
                        clicked,
                        resolved,
                        fallback: Some(format!("{reason}; using focused control")),
                        attempts: attempt,
                    });
                }
                Err(error) => {
                    reason.push_str(&format!("; focused fallback unavailable: {error:#}"))
                }
            }
        }
    }
    // Revalidate after all observations; a disappearing base must never redirect
    // input to a new foreground process or another application window.
    validate_base()?;
    Ok(Resolution {
        clicked,
        resolved: clicked,
        fallback: Some(format!("{reason}; using clicked window")),
        attempts: limit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const CLICKED: Window = Window { hwnd: 1, pid: 10 };

    #[test]
    fn clicked_uses_no_gui_queries() {
        let result = resolve_with(
            CLICKED,
            KeyboardFocus::Clicked,
            || panic!("Clicked must never query GUI focus"),
            |_| panic!(),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(result.resolved, CLICKED);
        assert_eq!(result.attempts, 0);
        assert!(result.fallback.is_none());
    }

    #[test]
    fn focused_retries_are_bounded_and_native_is_immediate() {
        for (policy, expected) in [(KeyboardFocus::Focused, 3), (KeyboardFocus::Native, 1)] {
            let mut queries = 0;
            let result = resolve_with(
                CLICKED,
                policy,
                || {
                    queries += 1;
                    Ok(ThreadWindows::default())
                },
                |_| anyhow::bail!("foreign or stale control"),
                || Ok(()),
            )
            .unwrap();
            assert_eq!(queries, expected);
            assert_eq!(result.resolved, CLICKED);
            assert!(result.fallback.unwrap().contains("foreign or stale"));
        }
    }

    #[test]
    fn focused_stops_at_first_eligible_control() {
        let mut queries = 0;
        let result = resolve_with(
            CLICKED,
            KeyboardFocus::Focused,
            || {
                queries += 1;
                Ok(ThreadWindows {
                    focused: if queries == 1 { 0 } else { 2 },
                    ..Default::default()
                })
            },
            |hwnd| {
                ensure!(hwnd != 0, "focused control not yet available");
                Ok(Window {
                    hwnd,
                    pid: CLICKED.pid,
                })
            },
            || Ok(()),
        )
        .unwrap();
        assert_eq!(queries, 2);
        assert_eq!(result.resolved.hwnd, 2);
        assert!(result.fallback.is_none());
    }

    #[test]
    fn foreign_candidate_rejection_never_expands_scope() {
        let result = resolve_with(
            CLICKED,
            KeyboardFocus::Active,
            || {
                Ok(ThreadWindows {
                    active: 99,
                    ..Default::default()
                })
            },
            |candidate| {
                let candidate = Window {
                    hwnd: candidate,
                    pid: 99,
                };
                validate_relationship(CLICKED, CLICKED.hwnd, candidate, &[CLICKED])?;
                Ok(candidate)
            },
            || Ok(()),
        )
        .unwrap();
        assert_eq!(result.resolved, CLICKED);
        assert!(result.fallback.unwrap().contains("foreign process"));
    }

    #[test]
    fn only_same_root_or_owned_popup_chain_is_eligible() {
        let candidate = Window {
            hwnd: 2,
            pid: CLICKED.pid,
        };
        assert!(validate_relationship(CLICKED, 1, candidate, &[CLICKED]).is_ok());
        assert!(validate_relationship(CLICKED, 1, candidate, &[candidate, CLICKED]).is_ok());
        assert!(validate_relationship(CLICKED, 1, candidate, &[candidate]).is_err());
        assert!(
            validate_relationship(
                CLICKED,
                1,
                candidate,
                &[Window { hwnd: 3, pid: 99 }, CLICKED]
            )
            .is_err()
        );
    }

    #[test]
    fn caret_uses_valid_focused_fallback_with_reason() {
        let result = resolve_with(
            CLICKED,
            KeyboardFocus::Caret,
            || {
                Ok(ThreadWindows {
                    caret: 99,
                    focused: 2,
                    ..Default::default()
                })
            },
            |candidate| {
                ensure!(candidate == 2, "caret belongs to foreign PID");
                Ok(Window {
                    hwnd: candidate,
                    pid: CLICKED.pid,
                })
            },
            || Ok(()),
        )
        .unwrap();
        assert_eq!(result.resolved.hwnd, 2);
        assert!(result.fallback.unwrap().contains("using focused control"));
    }

    #[test]
    fn disappearing_clicked_window_fails_instead_of_falling_back() {
        let mut validations = 0;
        assert!(
            resolve_with(
                CLICKED,
                KeyboardFocus::Native,
                || anyhow::bail!("query unavailable"),
                |_| panic!(),
                || {
                    validations += 1;
                    ensure!(validations == 1, "clicked window disappeared");
                    Ok(())
                }
            )
            .is_err()
        );
    }
}
