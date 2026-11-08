//! Provider cohort; separate from the compatibility-input Session pilot.
use super::*;
use cursor_snapshot_protocol::STATUS_VALID;
use mux_core::keyboard::ApplicationFocus;
use mux_windows::{app_cursor::AppCursorObservation, protoinput::DesktopBackend};

pub(super) const TABLE_A: usize = 20;
pub(super) const SECONDARY_HWND: usize = 21;
pub(super) const SHIFT_WORD: usize = 22;
pub(super) const LSHIFT_WORD: usize = 23;
pub(super) const RSHIFT_WORD: usize = 24;
pub(super) const CAPS_WORD: usize = 25;
pub(super) const CAPS_TABLE: usize = 26;

fn cursor(
    desktop: &DesktopBackend,
    index: usize,
    target: &SelectedTarget,
    expected_kind: u32,
) -> Result<AppCursorObservation> {
    let mut latest = None;
    wait_for("coherent selected application cursor", || {
        let observation = desktop.cursor_observation(index, target.hwnd, true);
        ensure!(observation.capability, "cursor capability disappeared");
        ensure!(
            observation.pid == target.pid && observation.hwnd == target.hwnd,
            "cursor observation changed its selected recipient"
        );
        let ready = observation.provider_status == Some(STATUS_VALID)
            && observation.visible == Some(true)
            && observation.kind == expected_kind
            && observation.attachment_epoch != 0
            && observation.association_epoch != 0
            && observation.sequence != 0;
        latest = Some(observation);
        Ok(ready)
    })
    .with_context(|| format!("last provider snapshot: {latest:?}"))?;
    latest.context("cursor provider did not return a snapshot")
}

fn key_word(target: &SelectedTarget, field: usize) -> Result<u16> {
    let value = query(target, field)?;
    ensure!(value <= u16::MAX as usize, "invalid fixture key-state word");
    Ok(value as u16)
}

fn table_byte(target: &SelectedTarget, field: usize) -> Result<u8> {
    let value = query(target, field)?;
    ensure!(value <= u8::MAX as usize, "fixture GetKeyboardState failed");
    Ok(value as u8)
}

fn key_a(target: &SelectedTarget, down: bool) -> Result<bool> {
    Ok(query(target, KEY_STATE)? == usize::from(down)
        && (table_byte(target, TABLE_A)? & 0x80 != 0) == down)
}

fn modifiers(
    desktop: &DesktopBackend,
    index: usize,
    keyboard: u32,
    target: &SelectedTarget,
) -> Result<()> {
    let expected = |left, right| -> Result<bool> {
        Ok((key_word(target, LSHIFT_WORD)? & 0x8000 != 0) == left
            && (key_word(target, RSHIFT_WORD)? & 0x8000 != 0) == right
            && (key_word(target, SHIFT_WORD)? & 0x8000 != 0) == (left || right))
    };
    ensure!(
        expected(false, false)?,
        "modifier state was not reset on start"
    );
    desktop.submit(index, Packet::key_scanned(keyboard, 0xa0, 0x2a, 0, false))?;
    wait_for("consumed left Shift", || expected(true, false))?;
    desktop.submit(index, Packet::key_scanned(keyboard, 0xa1, 0x36, 0, false))?;
    wait_for("consumed independent right Shift", || expected(true, true))?;
    desktop.submit(index, Packet::key_scanned(keyboard, 0xa0, 0x2a, 0, true))?;
    wait_for("generic Shift remains held by right Shift", || {
        expected(false, true)
    })?;
    desktop.submit(index, Packet::key_scanned(keyboard, 0xa1, 0x36, 0, true))?;
    wait_for("both Shift sides released", || expected(false, false))?;

    let caps = |down, toggle| -> Result<bool> {
        let word = key_word(target, CAPS_WORD)?;
        let table = table_byte(target, CAPS_TABLE)?;
        Ok((word & 0x8000 != 0) == down
            && (word & 1 != 0) == toggle
            && (table & 0x80 != 0) == down
            && (table & 1 != 0) == toggle)
    };
    ensure!(
        caps(false, false)?,
        "Caps Lock was not reset on provider start"
    );
    desktop.submit(index, Packet::key_scanned(keyboard, 0x14, 0x3a, 0, false))?;
    wait_for("consumed Caps Lock first edge", || caps(true, true))?;
    // A removed repeat must not toggle again. Waiting for the following key-up
    // proves the repeat ahead of it in the same GUI queue was also consumed.
    desktop.submit(index, Packet::key_scanned(keyboard, 0x14, 0x3a, 0, false))?;
    desktop.submit(index, Packet::key_scanned(keyboard, 0x14, 0x3a, 0, true))?;
    wait_for("Caps Lock repeat and release preserve toggle", || {
        caps(false, true)
    })?;
    desktop.submit(index, Packet::key_scanned(keyboard, 0x14, 0x3a, 0, false))?;
    desktop.submit(index, Packet::key_scanned(keyboard, 0x14, 0x3a, 0, true))?;
    wait_for("next Caps Lock edge toggles back", || caps(false, false))?;
    Ok(())
}

pub(super) fn controller(
    backend: Backend,
    seats: &[ResolvedSeat],
    targets: &[SelectedTarget],
) -> Result<()> {
    ensure!(
        seats.len() == targets.len(),
        "provider seat/target mismatch"
    );
    let mut desktop = backend.desktop_session(seats.to_vec())?;
    ensure!(
        desktop.cursor_capability(),
        "provider runtime lacks cursor capability"
    );
    for (index, target) in targets.iter().enumerate() {
        ensure!(
            desktop.open_expected_with_focus(
                target.pid,
                target.creation,
                ApplicationFocus::NativeControls,
            )? == index,
            "provider target attachment index changed"
        );
        // NativeControls startup requires an actual keyboard-provider reply
        // before the backend unfreezes virtual input.
        ensure!(
            key_a(target, false)?,
            "provider start inherited a held A key"
        );
        reset(target)?;
    }
    for (index, target) in targets.iter().enumerate() {
        let seat = &seats[index];
        let keyboard = seat.keyboard.context("provider fixture keyboard")?;
        let bounds = targets::client_bounds(target.hwnd, target.pid)?;
        let client = (17 + index as i32 * 7, 21 + index as i32 * 5);
        desktop.position(
            index,
            target.hwnd,
            seat.mouse,
            bounds.left + client.0,
            bounds.top + client.1,
            bounds,
        )?;
        wait_for("provider cohort motion and cursor API", || {
            Ok(query(target, 0)? > 0
                && query(target, CURSOR_X)? as isize == client.0 as isize
                && query(target, CURSOR_Y)? as isize == client.1 as isize)
        })?;
        let first = cursor(&desktop, index, target, 32512)?; // IDC_ARROW.
        desktop.submit(index, Packet::button(seat.mouse, 0, false))?;
        wait_for("provider cohort button-down", || Ok(query(target, 1)? == 1))?;
        desktop.submit(index, Packet::key(keyboard, 0x41, false))?;
        wait_for("consumed A down visible in both keyboard APIs", || {
            Ok(query(target, 3)? == 1 && key_a(target, true)?)
        })?;
        desktop.submit(index, Packet::text_unit(keyboard, 0x03a9))?;
        wait_for("provider cohort authored Unicode", || {
            Ok(query(target, 5)? == 1)
        })?;
        desktop.submit(index, Packet::button(seat.mouse, 0, true))?;
        desktop.submit(index, Packet::key(keyboard, 0x41, true))?;
        wait_for("consumed releases visible in both keyboard APIs", || {
            Ok(evidence(target)? == expected_evidence() && key_a(target, false)?)
        })?;
        modifiers(&desktop, index, keyboard, target)?;

        let secondary = query(target, SECONDARY_HWND)?;
        ensure!(
            secondary != 0 && secondary != target.hwnd,
            "missing second owned window"
        );
        let secondary = SelectedTarget {
            hwnd: secondary,
            ..target.clone()
        };
        validate_owned_target(&secondary, std::env::var(SUPERVISOR)?.parse()?)?;
        let bounds = targets::client_bounds(secondary.hwnd, target.pid)?;
        desktop.position(
            index,
            secondary.hwnd,
            seat.mouse,
            bounds.left + client.0,
            bounds.top + client.1,
            bounds,
        )?;
        let associated = cursor(&desktop, index, &secondary, 32515)?; // IDC_CROSS.
        ensure!(
            associated.attachment_epoch == first.attachment_epoch
                && associated.association_epoch > first.association_epoch
                && associated.sequence > first.sequence,
            "reassociation did not invalidate the old window snapshot"
        );
        let disabled = desktop.cursor_observation(index, secondary.hwnd, false);
        ensure!(
            disabled.provider_status.is_none() && disabled.image.is_none(),
            "disabled observer retained a usable image"
        );
        let restarted = cursor(&desktop, index, &secondary, 32515)?;
        ensure!(
            restarted.attachment_epoch > associated.attachment_epoch,
            "cursor re-enable reused the stopped attachment generation"
        );
        for untouched in targets.iter().skip(index + 1) {
            ensure!(
                evidence(untouched)?
                    == MessageEvidence {
                        buttons_down: 0,
                        buttons_up: 0,
                        keys_down: 0,
                        keys_up: 0,
                        authored_text: 0,
                    }
                    && key_a(untouched, false)?,
                "provider input leaked into another selected fixture"
            );
        }
        println!(
            "MIGRATION_PROVIDERS_VERIFIED PID {} cursor generations {}/{}/{} keyboard A, Shift aliases, Caps repeat/toggle and release verified",
            target.pid,
            first.attachment_epoch,
            associated.association_epoch,
            restarted.attachment_epoch
        );
    }
    // This submits Stop. The all-target shutdown below separately proves queue
    // draining, keyboard Stop acknowledgement and resident provider quiescence.
    desktop.stop_cursor_observers()?;
    let report = desktop.shutdown()?;
    ensure!(
        report.acknowledged == targets.len() && report.targets_exited == 0,
        "provider cleanup did not acknowledge each original GUI queue: {report:?}"
    );
    Ok(())
}

#[test]
#[ignore = "opt-in providers in newly spawned owned fixture processes only"]
fn rust_virtual_native_provider_pilot() -> Result<()> {
    let runtime = PathBuf::from(
        std::env::var_os(RUNTIME).context("set PROTOINPUTMULTIPLEX_MIGRATION_RUNTIME to opt in")?,
    )
    .canonicalize()?;
    for count in [1, 2] {
        let mut reference = None;
        for mode in ["rust", "native"] {
            let mut fixtures = (0..count)
                .map(|_| spawn_target())
                .collect::<Result<Vec<_>>>()?;
            for cycle in 1..=2 {
                let observed = run_cycle(&runtime, mode, &mut fixtures, "providers")?.messages;
                if let Some(expected) = &reference {
                    ensure!(
                        &observed == expected,
                        "native/Rust provider message comparison differed"
                    );
                } else {
                    reference = Some(observed);
                }
                println!(
                    "MIGRATION_PROVIDER_CLEAN_CYCLE mode={mode} targets={count} cycle={cycle} committed reuse verified"
                );
            }
        }
    }
    println!(
        "MIGRATION_PROVIDER_PILOT_PASSED controller={}bit target={}bit; ordinary consumed-message, cursor association and clean lifecycle scope; PM_NOREMOVE/filter-failure scenarios remain separate gates",
        usize::BITS,
        if std::env::var_os(TARGET_FIXTURE).is_some() {
            32
        } else {
            usize::BITS
        }
    );
    Ok(())
}
