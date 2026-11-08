//! Actual pre-mutation scope rejection on fresh, explicitly owned recipients.
use super::*;
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_FILES, GetLastError, INVALID_HANDLE_VALUE},
    System::{
        Diagnostics::ToolHelp::{
            MODULEENTRY32W, Module32FirstW, Module32NextW, TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32,
        },
        Memory::{FILE_MAP_READ, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile},
    },
};

fn untouched(target: &SelectedTarget, supervisor: u32) -> Result<()> {
    validate_owned_target(target, supervisor)?;
    let ready = targets::eligibility(target.pid);
    ensure!(
        ready.state == EligibilityState::Fresh && ready.creation == Some(target.creation),
        "scope rejection changed original fresh readiness: {}",
        ready.reason
    );
    let name = format!(
        "Local\\ProtoInputMultiPlex.Lifecycle.{}.{}",
        target.pid, target.creation
    );
    let mapping = unsafe { OpenFileMappingW(FILE_MAP_READ, 0, wide(&name).as_ptr()) };
    if !mapping.is_null() {
        unsafe { CloseHandle(mapping) };
        bail!("scope rejection created target lifecycle mapping");
    }
    ensure!(
        unsafe { GetLastError() } == ERROR_FILE_NOT_FOUND,
        "cannot establish lifecycle mapping absence"
    );
    let snapshot =
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, target.pid) };
    ensure!(
        snapshot != INVALID_HANDLE_VALUE,
        "snapshot owned target modules"
    );
    let result = (|| {
        let mut module: MODULEENTRY32W = unsafe { std::mem::zeroed() };
        module.dwSize = std::mem::size_of::<MODULEENTRY32W>() as u32;
        let mut present = unsafe { Module32FirstW(snapshot, &mut module) };
        let mut status = unsafe { GetLastError() };
        let mut count = 0;
        while present != 0 {
            count += 1;
            let end = module
                .szModule
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(module.szModule.len());
            let name = String::from_utf16_lossy(&module.szModule[..end]);
            ensure!(
                !name.eq_ignore_ascii_case("ProtoInputHooks32.dll")
                    && !name.eq_ignore_ascii_case("ProtoInputHooks64.dll"),
                "scope rejection injected {name}"
            );
            present = unsafe { Module32NextW(snapshot, &mut module) };
            status = unsafe { GetLastError() };
        }
        ensure!(
            count > 0 && status == ERROR_NO_MORE_FILES,
            "incomplete owned target module inventory"
        );
        Ok(())
    })();
    unsafe { CloseHandle(snapshot) };
    result
}

fn rejected(kind: &str, targets: &[SelectedTarget]) -> Result<()> {
    let supervisor = std::env::var(SUPERVISOR)?.parse()?;
    for target in targets {
        untouched(target, supervisor)?;
    }
    assert_no_native_loader()?;
    println!("MIGRATION_SCOPE_REJECTED {kind} {}", std::env::var(TOKEN)?);
    std::io::stdout().flush()?;
    Ok(())
}

pub(super) fn physical_controller(backend: Backend, targets: &[SelectedTarget]) -> Result<()> {
    let supervisor = std::env::var(SUPERVISOR)?.parse()?;
    for target in targets {
        untouched(target, supervisor)?;
    }
    let seats = targets
        .iter()
        .map(|target| ResolvedSeat {
            name: "Rejected synthetic physical handles".into(),
            mouse: 0x1234,
            keyboard: Some(0x5678),
            target_pid: target.pid,
        })
        .collect::<Vec<_>>();
    let error = backend
        .attach(&seats)
        .err()
        .context("Rust accepted nonvirtual handles")?;
    ensure!(
        error.to_string() == "Rust virtual control does not support physical or legacy sessions",
        "wrong nonvirtual scope rejection: {error:#}"
    );
    rejected("scope-reject", targets)
}

pub(super) fn legacy_controller(runtime: &Path, targets: &[SelectedTarget]) -> Result<()> {
    ensure!(
        std::env::var_os(WORKER_ENV).is_none(),
        "legacy fixture inherited supervision"
    );
    let supervisor = std::env::var(SUPERVISOR)?.parse()?;
    for target in targets {
        untouched(target, supervisor)?;
    }
    let missing = runtime.join(format!("missing-legacy-runtime-{}", std::env::var(TOKEN)?));
    ensure!(!missing.exists(), "legacy guard fixture path exists");
    let error = Backend::load_rust_virtual(&missing, Arc::new(AtomicBool::new(false)))
        .err()
        .context("Rust accepted unsupervised legacy mode")?;
    ensure!(
        error.to_string() == "Rust virtual control requires a supervised session transaction",
        "legacy scope guard ran after runtime resolution: {error:#}"
    );
    rejected("legacy-scope-reject", targets)
}

fn pending(commit: &CommitOwner) -> Result<()> {
    let handle = unsafe { OpenFileMappingW(FILE_MAP_READ, 0, wide(commit.name()).as_ptr()) };
    ensure!(!handle.is_null(), "rejected transaction disappeared");
    let view = unsafe { MapViewOfFile(handle, FILE_MAP_READ, 0, 0, 64) };
    if view.Value.is_null() {
        unsafe { CloseHandle(handle) };
        bail!("map rejected supervisor transaction");
    }
    let record = unsafe { ptr::read_volatile(view.Value.cast::<[u32; 4]>()) };
    unsafe {
        UnmapViewOfFile(view);
        CloseHandle(handle);
    }
    ensure!(
        record == [0x4d58_434d, 1, 64, 0],
        "scope rejection committed or corrupted transaction"
    );
    Ok(())
}

#[test]
#[ignore = "opt-in actual fresh-target Rust physical/unsupervised scope rejection"]
fn rust_virtual_scope_rejections_precede_target_mutation() -> Result<()> {
    let runtime = PathBuf::from(std::env::var_os(RUNTIME).context("native runtime opt-in")?)
        .canonicalize()?;
    let mut fixtures = vec![spawn_target()?];
    for kind in ["scope-reject", "legacy-scope-reject"] {
        untouched(&fixtures[0].selected, std::process::id())?;
        let mut controller = start_controller(&runtime, "rust", &fixtures, kind)?;
        pending(&controller.commit)?;
        let evidence = controller
            .process
            .line("MIGRATION_SCOPE_REJECTED ", Instant::now() + POLL_LIMIT)?;
        ensure!(
            evidence == format!("MIGRATION_SCOPE_REJECTED {kind} {}", controller.token),
            "uncorrelated scope rejection"
        );
        ensure!(
            controller
                .process
                .wait(Instant::now() + POLL_LIMIT)?
                .success(),
            "scope rejection controller failed"
        );
        pending(&controller.commit)?;
        ensure!(
            fixtures[0].process.child.try_wait()?.is_none(),
            "scope rejection terminated original target"
        );
        untouched(&fixtures[0].selected, std::process::id())?;
        println!(
            "MIGRATION_SCOPE_REJECTION_VERIFIED {kind}: original alive/Fresh, no lifecycle mapping/hooks, supervisor transaction Pending"
        );
        // Deliberately no CommitOwner::commit: neither rejected attempt stopped a session.
    }
    Ok(())
}
