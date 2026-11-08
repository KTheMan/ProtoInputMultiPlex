//! Select one control implementation before constructing it or attaching targets.
use anyhow::{Result, bail, ensure};
use mux_core::{DeviceKind, Profile, ResolvedSeat, emulated};
use std::ffi::OsStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Request {
    Default,
    Native,
    Rust,
}

impl Request {
    fn parse(raw: Option<&OsStr>) -> Result<Self> {
        match raw.map(OsStr::to_str) {
            None => Ok(Self::Default),
            Some(Some("native")) => Ok(Self::Native),
            Some(Some("rust")) => Ok(Self::Rust),
            _ => bail!("unsupported PROTOINPUTMULTIPLEX_CONTROL_BACKEND; expected native or rust"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    AllVirtual,
    IncludesPhysical,
}

/// A physical source path remains physical even if its transient handle resembles
/// a virtual tag. Canonical virtual paths must match the already-resolved handle.
fn virtual_device(path: &str, kind: DeviceKind, handle: u32) -> Result<bool> {
    ensure!(
        !path.is_empty(),
        "control selection has an empty device path"
    );
    ensure!(handle != 0, "control selection has a null device handle");
    let Some(device) = emulated::parse(path)? else {
        return Ok(false);
    };
    ensure!(
        device.kind == kind,
        "control selection device kind mismatch"
    );
    ensure!(
        device.handle == handle as usize,
        "control selection virtual handle differs from its canonical path"
    );
    Ok(true)
}

impl Scope {
    fn classify(profile: &Profile, resolved: &[ResolvedSeat]) -> Result<Self> {
        ensure!(!profile.seats.is_empty(), "profile has no seats");
        ensure!(
            profile.seats.len() == resolved.len(),
            "control selection resolved seat count differs from profile"
        );
        let mut all_virtual = true;
        for (seat, actual) in profile.seats.iter().zip(resolved) {
            // Deliberately inspect every pair, even after a physical source: a
            // malformed later pair cannot silently choose the native fallback.
            all_virtual &= virtual_device(&seat.mouse, DeviceKind::Mouse, actual.mouse)?;
            match (&seat.keyboard, actual.keyboard) {
                (Some(path), Some(handle)) => {
                    all_virtual &= virtual_device(path, DeviceKind::Keyboard, handle)?;
                }
                (None, None) => {}
                _ => bail!("control selection resolved keyboard differs from profile"),
            }
        }
        Ok(if all_virtual {
            Self::AllVirtual
        } else {
            Self::IncludesPhysical
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
    Native,
    RustVirtual,
}

impl Choice {
    fn select(request: Request, supervised: bool, scope: Scope) -> Result<Self> {
        match request {
            Request::Native => Ok(Self::Native),
            Request::Rust => {
                ensure!(
                    supervised,
                    "Rust virtual control requires a supervised session transaction"
                );
                ensure!(
                    scope == Scope::AllVirtual,
                    "Rust virtual control does not support physical or legacy sessions"
                );
                Ok(Self::RustVirtual)
            }
            Request::Default if supervised && scope == Scope::AllVirtual => Ok(Self::RustVirtual),
            Request::Default => Ok(Self::Native),
        }
    }
}

/// `supervised` describes the existing worker environment boundary only. The
/// selected backend still authenticates the actual Pending transaction at attach.
/// Constructor errors propagate directly; selection is never retried or switched.
pub(super) fn construct<T>(
    raw: Option<&OsStr>,
    supervised: bool,
    profile: &Profile,
    resolved: &[ResolvedSeat],
    native: impl FnOnce() -> Result<T>,
    rust: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let request = Request::parse(raw)?;
    let scope = Scope::classify(profile, resolved)?;
    match Choice::select(request, supervised, scope)? {
        Choice::Native => native(),
        Choice::RustVirtual => rust(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mux_core::{Device, Seat};
    use std::cell::Cell;

    fn seat(id: u32, keyboard: bool) -> Seat {
        serde_json::from_value(serde_json::json!({
            "name": format!("User {id}"),
            "mouse": format!("virtual://mouse/{id}"),
            "keyboard": keyboard.then(|| format!("virtual://keyboard/{id}")),
            "target_pid": 100 + id
        }))
        .unwrap()
    }

    fn profile() -> Profile {
        Profile {
            seats: vec![seat(1, true)],
            ..Default::default()
        }
    }

    fn choose(raw: Option<&OsStr>, supervised: bool, profile: &Profile) -> Result<Choice> {
        let resolved = profile.resolve(&[])?;
        construct(
            raw,
            supervised,
            profile,
            &resolved,
            || Ok(Choice::Native),
            || Ok(Choice::RustVirtual),
        )
    }

    fn assert_no_construction(
        raw: Option<&OsStr>,
        supervised: bool,
        profile: &Profile,
        resolved: &[ResolvedSeat],
    ) -> anyhow::Error {
        let calls = Cell::new(0);
        let result = construct(
            raw,
            supervised,
            profile,
            resolved,
            || {
                calls.set(calls.get() + 1);
                Ok(())
            },
            || {
                calls.set(calls.get() + 1);
                Ok(())
            },
        );
        assert_eq!(calls.get(), 0, "rejection constructed a backend");
        result.unwrap_err()
    }

    #[test]
    fn actual_factory_boundary_obeys_request_supervision_and_scope() {
        let physical_device = Device {
            path: "physical-mouse".into(),
            kind: DeviceKind::Mouse,
            handle: 7,
        };
        for physical in [false, true] {
            let mut profile = profile();
            if physical {
                profile.seats[0].mouse.clone_from(&physical_device.path);
            }
            let resolved = profile
                .resolve(std::slice::from_ref(&physical_device))
                .unwrap();
            for supervised in [false, true] {
                for raw in [None, Some(OsStr::new("native")), Some(OsStr::new("rust"))] {
                    let native_calls = Cell::new(0);
                    let rust_calls = Cell::new(0);
                    let result = construct(
                        raw,
                        supervised,
                        &profile,
                        &resolved,
                        || {
                            native_calls.set(native_calls.get() + 1);
                            Ok(Choice::Native)
                        },
                        || {
                            rust_calls.set(rust_calls.get() + 1);
                            Ok(Choice::RustVirtual)
                        },
                    );
                    if raw == Some(OsStr::new("rust")) && (!supervised || physical) {
                        let message = result.unwrap_err().to_string();
                        assert_eq!(
                            message,
                            if !supervised {
                                "Rust virtual control requires a supervised session transaction"
                            } else {
                                "Rust virtual control does not support physical or legacy sessions"
                            }
                        );
                        assert_eq!((native_calls.get(), rust_calls.get()), (0, 0));
                    } else {
                        let expected_rust =
                            raw != Some(OsStr::new("native")) && supervised && !physical;
                        assert_eq!(
                            result.unwrap(),
                            if expected_rust {
                                Choice::RustVirtual
                            } else {
                                Choice::Native
                            }
                        );
                        assert_eq!(
                            (native_calls.get(), rust_calls.get()),
                            if expected_rust { (0, 1) } else { (1, 0) }
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn every_supported_virtual_layout_uses_the_same_default() {
        for desktop_mode in [
            None,
            Some(mux_core::routing::Mode::Concurrent),
            Some(mux_core::routing::Mode::Switched),
        ] {
            for concurrent in [false, true] {
                for keyboard in [false, true] {
                    for users in [1, 2] {
                        let mut profile = Profile {
                            concurrent,
                            desktop: desktop_mode.map(|mode| mux_core::routing::DesktopRouting {
                                mode,
                                ..Default::default()
                            }),
                            seats: (1..=users).map(|id| seat(id, keyboard)).collect(),
                            ..Default::default()
                        };
                        if desktop_mode.is_some() {
                            for seat in &mut profile.seats {
                                seat.target_pid = 0;
                            }
                        }
                        assert_eq!(choose(None, true, &profile).unwrap(), Choice::RustVirtual);
                        assert_eq!(
                            choose(Some(OsStr::new("native")), true, &profile).unwrap(),
                            Choice::Native
                        );
                        assert_eq!(choose(None, false, &profile).unwrap(), Choice::Native);
                    }
                }
            }
        }
    }

    #[test]
    fn application_provider_settings_do_not_change_control_selection() {
        let mut profile = profile();
        profile.desktop = Some(Default::default());
        profile.seats[0].target_pid = 0;
        profile.seats[0].cursor.keyboard.application_focus =
            mux_core::keyboard::ApplicationFocus::NativeControls;
        profile.seats[0].cursor.keyboard.focus = mux_core::keyboard::KeyboardFocus::Focused;
        assert_eq!(choose(None, true, &profile).unwrap(), Choice::RustVirtual);
    }

    #[test]
    fn physical_keyboard_and_mixed_users_remain_native_even_with_virtual_tags() {
        for physical_keyboard in [false, true] {
            let mut profile = profile();
            profile.seats.push(seat(2, false));
            let (path, kind) = if physical_keyboard {
                profile.seats[0].keyboard = Some("physical-keyboard".into());
                ("physical-keyboard", DeviceKind::Keyboard)
            } else {
                profile.seats[1].mouse = "physical-mouse".into();
                ("physical-mouse", DeviceKind::Mouse)
            };
            let devices = [Device {
                path: path.into(),
                kind,
                handle: emulated::device(kind, 42).unwrap().handle,
            }];
            let resolved = profile.resolve(&devices).unwrap();
            assert_eq!(
                construct(
                    None,
                    true,
                    &profile,
                    &resolved,
                    || Ok(Choice::Native),
                    || Ok(Choice::RustVirtual)
                )
                .unwrap(),
                Choice::Native
            );
            assert_eq!(
                assert_no_construction(Some(OsStr::new("rust")), true, &profile, &resolved)
                    .to_string(),
                "Rust virtual control does not support physical or legacy sessions"
            );
        }
    }

    #[test]
    fn invalid_override_is_rejected_before_profile_or_factory_work() {
        for invalid in ["", "RUST", "Native", " rust", "rust ", "auto", "default"] {
            let error =
                assert_no_construction(Some(OsStr::new(invalid)), true, &Profile::default(), &[]);
            assert_eq!(
                error.to_string(),
                "unsupported PROTOINPUTMULTIPLEX_CONTROL_BACKEND; expected native or rust"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn nonunicode_override_is_not_absent_or_native() {
        use std::{ffi::OsString, os::windows::ffi::OsStringExt};
        let invalid = OsString::from_wide(&[0xd800]);
        let error = assert_no_construction(Some(&invalid), true, &Profile::default(), &[]);
        assert_eq!(
            error.to_string(),
            "unsupported PROTOINPUTMULTIPLEX_CONTROL_BACKEND; expected native or rust"
        );
    }

    #[test]
    fn malformed_source_or_resolved_pairs_fail_before_any_constructor() {
        let profile = profile();
        let resolved = profile.resolve(&[]).unwrap();
        assert_no_construction(None, true, &Profile::default(), &[]);
        assert_no_construction(None, true, &profile, &[]);
        for change in 0..5 {
            let mut actual = resolved.clone();
            match change {
                0 => actual[0].mouse = 0,
                1 => actual[0].mouse += 2,
                2 => actual[0].keyboard = None,
                3 => actual[0].keyboard = Some(0),
                4 => actual[0].keyboard = Some(actual[0].mouse),
                _ => unreachable!(),
            }
            assert_no_construction(None, true, &profile, &actual);
        }
        let mut no_keyboard = profile.clone();
        no_keyboard.seats[0].keyboard = None;
        assert_no_construction(None, true, &no_keyboard, &resolved);
        for path in ["", "virtual://mouse/01", "virtual://keyboard/1"] {
            let mut bad_source = profile.clone();
            bad_source.seats[0].mouse = path.into();
            assert_no_construction(None, true, &bad_source, &resolved);
        }
        let mut bad_keyboard = profile.clone();
        bad_keyboard.seats[0].keyboard = Some("virtual://mouse/1".into());
        assert_no_construction(None, true, &bad_keyboard, &resolved);
    }

    #[test]
    fn physical_source_does_not_short_circuit_later_pair_validation() {
        let mut profile = profile();
        profile.seats[0].mouse = "physical-mouse".into();
        let mut resolved = profile
            .resolve(&[Device {
                path: "physical-mouse".into(),
                kind: DeviceKind::Mouse,
                handle: 7,
            }])
            .unwrap();
        resolved[0].keyboard = Some(0);
        assert_no_construction(None, true, &profile, &resolved);
        resolved[0].keyboard = None;
        profile.seats[0].keyboard = None;
        resolved[0].mouse = 0;
        assert_no_construction(None, true, &profile, &resolved);
    }

    #[test]
    fn constructor_failure_preserves_error_without_trying_another_backend() {
        let profile = profile();
        let resolved = profile.resolve(&[]).unwrap();
        for raw in [None, Some(OsStr::new("native"))] {
            let native_calls = Cell::new(0);
            let rust_calls = Cell::new(0);
            let result: Result<()> = construct(
                raw,
                true,
                &profile,
                &resolved,
                || {
                    native_calls.set(native_calls.get() + 1);
                    bail!("selected native failure")
                },
                || {
                    rust_calls.set(rust_calls.get() + 1);
                    bail!("selected Rust failure")
                },
            );
            if raw.is_none() {
                assert_eq!(result.unwrap_err().to_string(), "selected Rust failure");
                assert_eq!((native_calls.get(), rust_calls.get()), (0, 1));
            } else {
                assert_eq!(result.unwrap_err().to_string(), "selected native failure");
                assert_eq!((native_calls.get(), rust_calls.get()), (1, 0));
            }
        }
    }
}
