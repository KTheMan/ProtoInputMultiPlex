//! Actual older paired loader/hooks rejection, before lifecycle Begin or setup.
use super::*;
use windows_sys::Win32::System::Memory::{
    FILE_MAP_READ, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile,
};

const STALE_RUNTIME: &str = "PROTOINPUTMULTIPLEX_MIGRATION_STALE_RUNTIME";
const CURRENT_BUILD: u64 = 0x2026_1107_0000_0001;

pub(super) fn controller(
    backend: Backend,
    seats: &[ResolvedSeat],
    targets: &[SelectedTarget],
    mode: &str,
) -> Result<()> {
    ensure!(
        targets.len() == 1,
        "stale-build fixture requires one original target"
    );
    let error = backend
        .attach(seats)
        .err()
        .context("older runtime unexpectedly attached")?;
    let detail = format!("{error:#}");
    ensure!(
        if mode == "native" {
            detail.contains("incompatible resident identity, architecture or exact build")
        } else {
            detail.contains("compatible target readiness timed out")
        },
        "unexpected rejection does not prove resident build checking: {detail}"
    );
    println!("MIGRATION_BUILD_REJECTED {mode} {}", std::env::var(TOKEN)?);
    std::io::stdout().flush()?;
    Ok(())
}

fn passive_record(target: &SelectedTarget) -> Result<[u8; 128]> {
    validate_owned_target(target, std::process::id())?;
    let name = wide(&format!(
        "Local\\ProtoInputMultiPlex.Lifecycle.{}.{}",
        target.pid, target.creation
    ));
    let mapping = unsafe { OpenFileMappingW(FILE_MAP_READ, 0, name.as_ptr()) };
    ensure!(
        !mapping.is_null(),
        "older resident never published readiness"
    );
    let view = unsafe { MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 128) };
    if view.Value.is_null() {
        unsafe {
            CloseHandle(mapping);
        }
        bail!("map owned older resident readiness");
    }
    let mut stable = None;
    for _ in 0..16 {
        let record = unsafe { ptr::read_volatile(view.Value.cast::<[u8; 128]>()) };
        std::sync::atomic::fence(Ordering::Acquire);
        let sequence = unsafe { ptr::read_volatile(view.Value.cast::<u8>().add(12).cast::<u32>()) };
        let first = u32::from_le_bytes([record[12], record[13], record[14], record[15]]);
        if first != 0 && first & 1 == 0 && first == sequence {
            stable = Some(record);
            break;
        }
        std::thread::yield_now();
    }
    unsafe {
        UnmapViewOfFile(view);
        CloseHandle(mapping);
    }
    stable.context("older resident readiness did not publish a coherent snapshot")
}

#[test]
#[ignore = "opt-in actual older paired runtime; injects only newly owned fixture children"]
fn rust_and_native_reject_stale_build_before_begin() -> Result<()> {
    let runtime = PathBuf::from(
        std::env::var_os(STALE_RUNTIME).context("set migration stale-runtime opt-in")?,
    )
    .canonicalize()?;
    for mode in ["rust", "native"] {
        let fixtures = vec![spawn_target()?];
        let mut rejected = start_controller(&runtime, mode, &fixtures, "build-reject")?;
        let line = rejected
            .process
            .line("MIGRATION_BUILD_REJECTED ", Instant::now() + PROCESS_LIMIT)?;
        ensure!(
            line == format!("MIGRATION_BUILD_REJECTED {mode} {}", rejected.token),
            "stale rejection correlation failed"
        );
        ensure!(
            rejected
                .process
                .wait(Instant::now() + POLL_LIMIT)?
                .success(),
            "stale rejection controller failed"
        );
        let target = &fixtures[0].selected;
        let record = passive_record(target)?;
        let word = |offset| u32::from_le_bytes(record[offset..offset + 4].try_into().unwrap());
        let wide = |offset| u64::from_le_bytes(record[offset..offset + 8].try_into().unwrap());
        ensure!(
            word(0) == 0x4d58_5244 && word(4) == 1 && word(8) == 128,
            "wrong passive readiness layout"
        );
        ensure!(
            wide(32) == target.creation && wide(56) != CURRENT_BUILD,
            "fixture did not load an older resident build"
        );
        ensure!(
            wide(40) == 0 && wide(48) == 0,
            "older resident received lifecycle Begin before rejection"
        );
        ensure!(
            targets::eligibility(target.pid).state == EligibilityState::Dirty,
            "rejected stale bootstrap did not remain quarantined"
        );
        // Its supervisor transaction is deliberately never committed.
        println!(
            "MIGRATION_STALE_BUILD_REJECTED mode={mode} original={} build=0x{:016x} generation=0 session=0; quarantine verified",
            target.pid,
            wide(56)
        );
    }
    Ok(())
}
