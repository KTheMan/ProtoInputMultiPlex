//! Actual owned-window facts for the supported virtual input surface.
//! Horizontal delivery uses the existing Rust adapter used by desktop routers;
//! direct Session 0x800 packets do not promise WM_MOUSEHWHEEL in the native ABI.
use super::*;
use mux_core::desktop::{CursorSettings, HorizontalWheelPolicy};
use mux_windows::protoinput::Session;
use std::sync::OnceLock;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_XBUTTONDOWN, WM_XBUTTONUP,
};

pub(super) const SURFACE_FIXTURE: &str = "PROTOINPUTMULTIPLEX_MIGRATION_INPUT_SURFACE";
pub(super) const QUERY_BASE: usize = 100;
pub(super) const QUERY_END: usize = 133;
const V_COUNT: usize = 16;
const H_COUNT: usize = 20;
const RIGHT_DOWN: usize = 24;
const RIGHT_UP: usize = 25;
const INVALID_X: usize = 30;
const LAST_X_TAG: usize = 31;
const B_STATE_AT_ARROW_UP: usize = 32;
const B_STATE: usize = 33;
static OBSERVED: OnceLock<[AtomicUsize; 33]> = OnceLock::new();

pub(super) fn prepare_target() {
    let _ = OBSERVED.set(std::array::from_fn(|_| AtomicUsize::new(0)));
}

pub(super) fn reset() {
    if let Some(observed) = OBSERVED.get() {
        for field in observed {
            field.store(0, Ordering::Relaxed);
        }
    }
}

pub(super) fn query(field: usize) -> LRESULT {
    let Some(observed) = OBSERVED.get() else {
        return -1;
    };
    let field = field - QUERY_BASE;
    if field == B_STATE {
        return unsafe { GetKeyState(0x42) as u16 as isize };
    }
    observed[field].load(Ordering::Acquire) as isize
}

pub(super) fn observe(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) {
    // Ordinary performance/provider fixtures allocate no observer state and
    // return immediately. No extra reader, packet transformation or GUI work.
    let Some(observed) = OBSERVED.get() else {
        return;
    };
    let button = match message {
        WM_RBUTTONDOWN => Some(0),
        WM_RBUTTONUP => Some(1),
        WM_MBUTTONDOWN => Some(2),
        WM_MBUTTONUP => Some(3),
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            let tag = (wparam >> 16) & 0xffff;
            observed[LAST_X_TAG].store(tag, Ordering::Relaxed);
            match tag {
                1 => Some(4 + usize::from(message == WM_XBUTTONUP)),
                2 => Some(6 + usize::from(message == WM_XBUTTONUP)),
                _ => {
                    observed[INVALID_X].fetch_add(1, Ordering::Relaxed);
                    None
                }
            }
        }
        _ => None,
    };
    if let Some(index) = button {
        observed[8 + index].store(wparam as u32 as usize, Ordering::Relaxed);
        observed[index].fetch_add(1, Ordering::Release);
    } else if matches!(message, WM_MOUSEWHEEL | WM_MOUSEHWHEEL) {
        let index = if message == WM_MOUSEWHEEL {
            V_COUNT
        } else {
            H_COUNT
        };
        observed[index + 1].store((wparam >> 16) & 0xffff, Ordering::Relaxed);
        observed[index + 2].store(wparam & 0xffff, Ordering::Relaxed);
        observed[index + 3].store(hwnd as usize, Ordering::Relaxed);
        observed[index].fetch_add(1, Ordering::Release);
    } else if matches!(message, WM_KEYDOWN | WM_KEYUP) && wparam == 0x27 {
        let up = usize::from(message == WM_KEYUP);
        if up != 0 {
            // Capture while the session's synthetic API hook is still active;
            // querying current GetKeyState after retirement would sample OS state.
            observed[B_STATE_AT_ARROW_UP].store(
                unsafe { GetKeyState(0x42) as u16 as usize },
                Ordering::Relaxed,
            );
        }
        observed[26 + up].store(lparam as u32 as usize, Ordering::Relaxed);
        observed[28 + up].store(hwnd as usize, Ordering::Relaxed);
        observed[24 + up].fetch_add(1, Ordering::Release);
    }
}

fn final_surface_evidence(target: &SelectedTarget) -> Result<()> {
    // Called only after the controller's real GUI fences and lifecycle Prepare,
    // successful exit and supervisor commit. Recheck every original recipient:
    // later-target leakage and delayed duplicates cannot hide behind prior waits.
    let secondary = super::query(target, provider_pilot::SECONDARY_HWND)?;
    ensure!(
        secondary != 0 && secondary != target.hwnd,
        "final secondary recipient disappeared"
    );
    for (index, expected_word) in [
        0x0000_0002,
        0,
        0x0000_0010,
        0,
        0x0001_0020,
        0x0001_0000,
        0x0002_0040,
        0x0002_0000,
    ]
    .into_iter()
    .enumerate()
    {
        ensure!(
            field(target, index)? == 1,
            "final button edge count changed for PID {} index {index}",
            target.pid
        );
        ensure!(
            field(target, 8 + index)? == expected_word,
            "final button flags/tag changed for PID {} index {index}",
            target.pid
        );
    }
    for axis in [V_COUNT, H_COUNT] {
        ensure!(
            wheel_observed(target, axis, 2, -120)?
                && field(target, axis + 2)? == 0
                && field(target, axis + 3)? == secondary,
            "final wheel count/delta/modifiers/recipient changed for PID {} axis {axis}",
            target.pid
        );
    }
    ensure!(
        field(target, RIGHT_DOWN)? == 1 && field(target, RIGHT_UP)? == 1,
        "final extended-key edge count changed for PID {}",
        target.pid
    );
    ensure!(
        native_word(target, 26)? == 0x014d_0001
            && native_word(target, 27)? == 0xc14d_0001
            && field(target, 28)? == target.hwnd
            && field(target, 29)? == target.hwnd,
        "final extended-key flags/original recipient changed for PID {}",
        target.pid
    );
    ensure!(
        field(target, INVALID_X)? == 0 && field(target, LAST_X_TAG)? == 2,
        "final X button tag evidence changed for PID {}",
        target.pid
    );
    ensure!(
        super::query(target, 6)? == 0
            && super::query(target, 7)? == 0
            && field(target, B_STATE_AT_ARROW_UP)? == 0,
        "suppressed B appeared or remained pressed for PID {}",
        target.pid
    );
    ensure!(
        super::query(target, 31)? == secondary
            && super::query(target, 32)? == secondary
            && super::query(target, 33)? == target.hwnd
            && super::query(target, 34)? == target.hwnd,
        "final keyboard/mouse original recipients changed for PID {}",
        target.pid
    );
    Ok(())
}

fn field(target: &SelectedTarget, field: usize) -> Result<usize> {
    super::query(target, QUERY_BASE + field)
}

fn native_word(target: &SelectedTarget, index: usize) -> Result<u32> {
    // Captured WPARAM/LPARAM words are explicitly u32. Cross-width LRESULT
    // marshalling may sign-extend a 32-bit target's high transition bit.
    Ok(field(target, index)? as u32)
}

fn pointer(target: &SelectedTarget) -> Result<(isize, isize)> {
    let point = (
        super::query(target, CURSOR_X)? as isize,
        super::query(target, CURSOR_Y)? as isize,
    );
    ensure!(
        point.0 != isize::MIN && point.1 != isize::MIN,
        "surface fixture cursor API failed"
    );
    Ok(point)
}

fn wheel_observed(target: &SelectedTarget, index: usize, count: usize, delta: i16) -> Result<bool> {
    let word = field(target, index + 1)?;
    ensure!(
        word <= u16::MAX as usize,
        "wheel high word exceeded its native width"
    );
    Ok(field(target, index)? == count && word as u16 as i16 == delta)
}

pub(super) fn exercise(
    session: &Session,
    seats: &[ResolvedSeat],
    targets: &[SelectedTarget],
    mode: &str,
) -> Result<()> {
    let mut secondary_targets = Vec::new();
    for (index, target) in targets.iter().enumerate() {
        let seat = &seats[index];
        let keyboard = seat.keyboard.context("surface keyboard missing")?;
        let secondary_hwnd = super::query(target, provider_pilot::SECONDARY_HWND)?;
        ensure!(
            secondary_hwnd != 0 && secondary_hwnd != target.hwnd,
            "surface secondary window missing"
        );
        let secondary = SelectedTarget {
            hwnd: secondary_hwnd,
            ..target.clone()
        };
        validate_owned_target(&secondary, std::env::var(SUPERVISOR)?.parse()?)?;
        let bounds = targets::client_bounds(secondary.hwnd, target.pid)?;
        let point = (37 + index as i32 * 5, 49 + index as i32 * 3);
        session.position(
            index,
            secondary.hwnd,
            seat.mouse,
            bounds.left + point.0,
            bounds.top + point.1,
            bounds,
        )?;
        wait_for("surface selected secondary pointer", || {
            Ok(pointer(&secondary)? == (point.0 as isize, point.1 as isize)
                && super::query(&secondary, 36)? == secondary.hwnd
                && super::query(&secondary, 0)? > 0)
        })?;
        ensure!(
            session
                .position(
                    index,
                    secondary.hwnd,
                    seat.mouse,
                    bounds.right,
                    bounds.top + point.1,
                    bounds
                )
                .is_err(),
            "outside position unexpectedly succeeded"
        );
        ensure!(
            pointer(&secondary)? == (point.0 as isize, point.1 as isize),
            "rejected outside position moved the pointer"
        );

        for (button, down_index, down_word, up_word) in [
            (1, 0, 0x0000_0002, 0),
            (2, 2, 0x0000_0010, 0),
            (3, 4, 0x0001_0020, 0x0001_0000),
            (4, 6, 0x0002_0040, 0x0002_0000),
        ] {
            session.submit(Packet::button(seat.mouse, button, false))?;
            wait_for("surface button down", || {
                ensure!(
                    field(target, INVALID_X)? == 0,
                    "invalid X button tag {}",
                    field(target, LAST_X_TAG)?
                );
                Ok(field(target, down_index)? == 1)
            })?;
            ensure!(
                field(target, 8 + down_index)? == down_word,
                "wrong native button-down flags/tag for button {button}"
            );
            session.submit(Packet::button(seat.mouse, button, true))?;
            wait_for("surface button up", || {
                Ok(field(target, down_index + 1)? == 1)
            })?;
            ensure!(
                field(target, 9 + down_index)? == up_word,
                "wrong native button-up flags/tag for button {button}"
            );
        }
        for (number, delta) in [120, -120].into_iter().enumerate() {
            session.submit(Packet::wheel(seat.mouse, delta))?;
            wait_for("surface signed vertical wheel", || {
                wheel_observed(target, V_COUNT, number + 1, delta)
            })?;
            ensure!(
                field(target, V_COUNT + 2)? == 0 && field(target, V_COUNT + 3)? == secondary.hwnd,
                "vertical wheel changed modifiers or original recipient"
            );
        }

        // A goes to the explicitly selected keyboard recipient while the mouse
        // retains its independently selected secondary window and client point.
        session.submit_to(
            index,
            target.hwnd,
            Packet::key_scanned(keyboard, 0x41, 0x1e, 0, false),
        )?;
        wait_for(
            "surface original A held and mouse selection restored",
            || {
                Ok(super::query(target, 3)? == 1
                    && super::query(target, KEY_STATE)? == 1
                    && super::query(target, 33)? == target.hwnd
                    && super::query(target, 36)? == secondary.hwnd)
            },
        )?;
        session.pause(true)?;
        session.submit_to(
            index,
            target.hwnd,
            Packet::key_scanned(keyboard, 0x42, 0x30, 0, false),
        )?;
        session.submit_to(
            index,
            target.hwnd,
            Packet::key_scanned(keyboard, 0x41, 0x1e, 0, true),
        )?;
        wait_for("preheld A releases during external freeze", || {
            Ok(super::query(target, 4)? == 1
                && super::query(target, KEY_STATE)? == 0
                && super::query(target, 34)? == target.hwnd)
        })?;
        ensure!(
            super::query(target, 6)? == 0 && field(target, B_STATE)? == 0,
            "freeze admitted a new B-down ahead of the observed A release"
        );
        session.pause(false)?;
        for released in [false, true] {
            session.submit_to(
                index,
                target.hwnd,
                Packet::key_scanned(keyboard, 0x27, 0x4d, 2, released),
            )?;
            wait_for("resumed E0 right-arrow message", || {
                Ok(field(target, if released { RIGHT_UP } else { RIGHT_DOWN })? == 1)
            })?;
        }
        ensure!(
            native_word(target, 26)? == 0x014d_0001 && native_word(target, 27)? == 0xc14d_0001,
            "right-arrow scan/E0/repeat/previous/up LPARAM bits differ from Win32"
        );
        ensure!(
            field(target, 28)? == target.hwnd && field(target, 29)? == target.hwnd,
            "extended key changed its original recipient"
        );
        ensure!(
            super::query(target, 6)? == 0
                && super::query(target, 7)? == 0
                && field(target, B_STATE)? == 0,
            "suppressed B reappeared after resume"
        );
        wait_for("keyboard retarget restores secondary mouse pointer", || {
            Ok(super::query(&secondary, 36)? == secondary.hwnd
                && pointer(&secondary)? == (point.0 as isize, point.1 as isize))
        })?;
        session.submit(Packet::button(seat.mouse, 0, false))?;
        session.submit(Packet::button(seat.mouse, 0, true))?;
        wait_for("retained secondary mouse click", || {
            Ok(super::query(target, 1)? == 1 && super::query(target, 2)? == 1)
        })?;
        ensure!(
            super::query(target, 31)? == secondary.hwnd
                && super::query(target, 32)? == secondary.hwnd,
            "keyboard retarget displaced the original mouse recipient"
        );
        session.submit_to(index, target.hwnd, Packet::text_unit(keyboard, 0x03a9))?;
        wait_for("surface common text/count evidence", || {
            Ok(evidence(target)? == expected_evidence())
        })?;
        for untouched in targets.iter().skip(index + 1) {
            ensure!(
                evidence(untouched)?
                    == MessageEvidence {
                        buttons_down: 0,
                        buttons_up: 0,
                        keys_down: 0,
                        keys_up: 0,
                        authored_text: 0
                    },
                "surface input leaked into another selected process"
            );
            for count in (0..8).chain([V_COUNT, H_COUNT, RIGHT_DOWN, RIGHT_UP]) {
                ensure!(
                    field(untouched, count)? == 0,
                    "extended surface input leaked into another selected process"
                );
            }
        }
        println!(
            "MIGRATION_INPUT_SURFACE_CORE mode={mode} PID {} right/middle/X1/X2 flags, signed vertical wheel, scan/E0/up bits, frozen new-down suppression/preheld release, outside-position rejection and keyboard/mouse recipient restoration verified",
            target.pid
        );
        secondary_targets.push((secondary, bounds, point));
    }

    // Preserve/conditional H-wheel delivery intentionally bypasses native packet
    // conversion in both production routers. Exercise that existing Rust adapter,
    // with actual target messages; don't infer WM_HWHEEL support from wire0x800.
    let settings = CursorSettings {
        horizontal_wheel: HorizontalWheelPolicy::Preserve,
        ..CursorSettings::default()
    };
    for (index, (secondary, bounds, point)) in secondary_targets.iter().enumerate() {
        for (number, delta) in [120, -120].into_iter().enumerate() {
            let packet = settings
                .transform_wheel_packet(Packet::horizontal_wheel(seats[index].mouse, delta))
                .context("Preserve dropped horizontal input")?;
            ensure!(packet.flags == 0x800, "Preserve changed horizontal axis");
            mux_windows::wheel::post_horizontal_wheel(
                secondary.hwnd,
                secondary.pid,
                (bounds.left + point.0, bounds.top + point.1),
                packet.data as u16 as i16,
                0,
            )?;
            wait_for("surface signed horizontal adapter wheel", || {
                wheel_observed(&targets[index], H_COUNT, number + 1, delta)
            })?;
            ensure!(
                field(&targets[index], H_COUNT + 2)? == 0
                    && field(&targets[index], H_COUNT + 3)? == secondary.hwnd,
                "horizontal adapter changed modifiers or original recipient"
            );
        }
        ensure!(
            evidence(&targets[index])? == expected_evidence(),
            "surface supplement changed common input facts"
        );
        println!(
            "MIGRATION_INPUT_SURFACE_HORIZONTAL mode={mode} PID {} Preserve±120 signed WM_MOUSEHWHEEL via existing validated Rust adapter; direct Session0x800 message synthesis outside native contract",
            secondary.pid
        );
    }
    Ok(())
}

#[test]
#[ignore = "opt-in actual native supported-input surface on one/two owned targets"]
fn rust_and_native_supported_input_surface() -> Result<()> {
    let runtime = PathBuf::from(std::env::var_os(RUNTIME).context("native runtime opt-in")?)
        .canonicalize()?;
    for count in [1, 2] {
        for mode in ["rust", "native"] {
            let mut targets = (0..count)
                .map(|_| spawn_target_features(false, false, true))
                .collect::<Result<Vec<_>>>()?;
            let observed = run_cycle(&runtime, mode, &mut targets, "input-surface")?;
            ensure!(
                observed.messages == vec![expected_evidence(); count],
                "surface cohort lacks common committed evidence"
            );
            for target in &targets {
                final_surface_evidence(&target.selected)?;
            }
            println!(
                "MIGRATION_INPUT_SURFACE_COMMITTED mode={mode} targets={count}; final per-target supplemental facts and committed reusable readiness verified"
            );
        }
    }
    Ok(())
}
