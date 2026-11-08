//! Real nonpumping GUI-queue failure and late native-fence consumption.
//! The extra stdin reader and pump observations exist only in opted-in fixtures.
use super::*;
use mux_windows::protoinput::Session;
use std::{
    fs::File,
    os::windows::io::{AsRawHandle, FromRawHandle},
    sync::{Mutex, OnceLock},
};
use windows_sys::Win32::{
    Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, INVALID_HANDLE_VALUE},
    System::Memory::{FILE_MAP_READ, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile},
    UI::WindowsAndMessaging::{
        PM_NOREMOVE, PeekMessageW, PostMessageW, RegisterWindowMessageW, WM_NULL,
    },
};

pub(super) const PAUSE_CAPABLE: &str = "PROTOINPUTMULTIPLEX_MIGRATION_PAUSE_CAPABLE";
pub(super) const PAUSE_QUERY: usize = 37;
static ENABLED: AtomicBool = AtomicBool::new(false);
static PAUSE_REQUESTED: AtomicBool = AtomicBool::new(false);
static PAUSED: AtomicBool = AtomicBool::new(false);
static GUI_THREAD: OnceLock<std::thread::Thread> = OnceLock::new();
static FENCE_MESSAGE: OnceLock<u32> = OnceLock::new();
static PUMP_OUTPUT: OnceLock<Mutex<File>> = OnceLock::new();

fn retained_pipe(handle: HANDLE) -> Result<File> {
    ensure!(
        !handle.is_null() && handle != INVALID_HANDLE_VALUE,
        "pause fixture lacks its inherited pipe"
    );
    let mut retained = ptr::null_mut();
    ensure!(
        unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                handle,
                GetCurrentProcess(),
                &mut retained,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        } != 0,
        "retain owned fixture pipe"
    );
    Ok(unsafe { File::from_raw_handle(retained) })
}

fn pump_evidence(kind: &str) -> Result<()> {
    let mut output = PUMP_OUTPUT
        .get()
        .context("pause fixture output was not retained")?
        .lock()
        .map_err(|_| anyhow::anyhow!("pause fixture output lock poisoned"))?;
    writeln!(
        output,
        "{kind} {} {}",
        std::process::id(),
        FIXTURE_TOKEN.load(Ordering::Relaxed)
    )?;
    output.flush()?;
    Ok(())
}

pub(super) fn prepare_target() -> Result<()> {
    // The native DLL allocates a console and replaces standard handles. Keep
    // only this opted-in fixture's inherited pipes alive across that operation.
    let input = retained_pipe(std::io::stdin().as_raw_handle())?;
    PUMP_OUTPUT
        .set(Mutex::new(retained_pipe(
            std::io::stdout().as_raw_handle(),
        )?))
        .map_err(|_| anyhow::anyhow!("pause fixture output was already retained"))?;
    GUI_THREAD
        .set(std::thread::current())
        .map_err(|_| anyhow::anyhow!("pause fixture GUI was already registered"))?;
    let fence =
        unsafe { RegisterWindowMessageW(wide("ProtoInputMultiPlex.QueueFence.V1").as_ptr()) };
    ensure!(fence != 0, "register native fence observation message");
    FENCE_MESSAGE
        .set(fence)
        .map_err(|_| anyhow::anyhow!("fence message was already registered"))?;
    ENABLED.store(true, Ordering::Release);
    std::thread::spawn(move || {
        for command in BufReader::new(input).lines() {
            let Ok(command) = command else {
                break;
            };
            if command == format!("RESUME_PUMP {}", FIXTURE_TOKEN.load(Ordering::Relaxed))
                && PAUSED.swap(false, Ordering::AcqRel)
                && let Some(gui) = GUI_THREAD.get()
            {
                gui.unpark();
            }
        }
    });
    Ok(())
}

pub(super) fn request_pause(hwnd: HWND, authentication: LPARAM) -> LRESULT {
    if !ENABLED.load(Ordering::Acquire)
        || authentication as usize != FIXTURE_TOKEN.load(Ordering::Relaxed)
        || PAUSED.load(Ordering::Acquire)
        || PAUSE_REQUESTED.swap(true, Ordering::AcqRel)
    {
        return 0;
    }
    // A sent query can be dispatched inside a currently blocked GetMessage.
    // Wake that real retrieval so the GUI reaches its outside-pump pause point.
    if unsafe { PostMessageW(hwnd, PUMP_WAKE, FIXTURE_TOKEN.load(Ordering::Relaxed), 0) } == 0 {
        PAUSE_REQUESTED.store(false, Ordering::Release);
        return 0;
    }
    1
}

fn private_fence_pending() -> Result<bool> {
    let fence = *FENCE_MESSAGE
        .get()
        .context("native fence message not registered")?;
    let mut marker: MSG = unsafe { std::mem::zeroed() };
    // PM_NOREMOVE never acknowledges or removes the marker. The native hook
    // may hide the returned copy; the successful range lookup still proves the
    // private registered message remains in this original GUI thread's queue.
    Ok(unsafe { PeekMessageW(&mut marker, ptr::null_mut(), fence, fence, PM_NOREMOVE) } != 0)
}

pub(super) fn target_pump() -> Result<()> {
    let result = pump_loop();
    if let Err(error) = &result {
        let _ = pump_evidence(&format!("MIGRATION_PUMP_ERROR {error:#}"));
    }
    result
}

fn pump_loop() -> Result<()> {
    let mut observe_late_fence = false;
    loop {
        if PAUSE_REQUESTED.swap(false, Ordering::AcqRel) {
            PAUSED.store(true, Ordering::Release);
            pump_evidence("MIGRATION_PUMP_PAUSED")?;
            while PAUSED.load(Ordering::Acquire) {
                std::thread::park();
            }
            ensure!(
                private_fence_pending()?,
                "resumed queue lacks the original timed-out native fence"
            );
            observe_late_fence = true;
            pump_evidence("MIGRATION_LATE_FENCE_PENDING")?;
        }
        let mut message: MSG = unsafe { std::mem::zeroed() };
        // This is the same real, unfiltered GetMessage pump as normal fixtures.
        let result = unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) };
        ensure!(result != -1, "pause fixture GetMessage failed");
        if result == 0 {
            break;
        }
        if observe_late_fence && !private_fence_pending()? {
            // Only this GUI thread consumes its queue. Since the PM_NOREMOVE
            // observation proved the marker was pending before retrieval, its
            // disappearance and the native hook's hidden return identify real
            // fence removal through the resident hook, rather than a timed delay
            // or mock. The original timed-out host waiter no longer exists.
            ensure!(
                message.hwnd.is_null()
                    && message.message == WM_NULL
                    && message.wParam == 0
                    && message.lParam == 0
                    && message.time == 0
                    && message.pt.x == 0
                    && message.pt.y == 0,
                "late fence bypassed its native removal hook"
            );
            observe_late_fence = false;
            pump_evidence("MIGRATION_LATE_FENCE_CONSUMED")?;
        }
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

pub(super) fn controller(
    mut session: Session,
    targets: &[SelectedTarget],
    mode: &str,
) -> Result<()> {
    ensure!(
        targets.len() == 2,
        "cleanup failure cohort needs two original recipients"
    );
    for target in targets {
        ensure!(
            evidence(target)? == expected_evidence(),
            "input/release evidence is incomplete"
        );
    }
    let first = &targets[0];
    let mut accepted = 0;
    ensure!(
        unsafe {
            SendMessageTimeoutW(
                first.hwnd as HWND,
                QUERY,
                PAUSE_QUERY,
                first.token as usize as isize,
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                QUERY_TIMEOUT_MS,
                &mut accepted,
            )
        } != 0
            && accepted == 1,
        "original fixture rejected authenticated outside-pump pause"
    );
    let correlation = std::env::var(TOKEN)?;
    println!(
        "MIGRATION_PAUSE_REQUESTED {mode} {} {correlation}",
        first.pid
    );
    std::io::stdout().flush()?;
    let mut confirmed = String::new();
    std::io::stdin().read_line(&mut confirmed)?;
    ensure!(
        confirmed.trim() == format!("PAUSE_CONFIRMED {correlation}"),
        "supervisor did not confirm the real GUI pause"
    );
    let started = Instant::now();
    let error = session
        .shutdown()
        .err()
        .context("nonpumping recipient unexpectedly completed cleanup")?;
    ensure!(
        started.elapsed() < POLL_LIMIT,
        "partial cleanup exceeded its bounded fault-test budget"
    );
    let detail = format!("{error:#}");
    ensure!(
        detail.contains("target 0 cleanup Drain")
            && detail.contains(&format!("PID {}", first.pid))
            && (detail.contains("queue-fence timed out")
                || detail.contains("queue-fence deadline expired")),
        "cleanup failed for a reason other than the original nonpumping GUI queue: {detail}"
    );
    // Before any Session drop/EOF cleanup, the healthy second target must have
    // progressed through cursor hook removal despite the first stream's failed
    // fence. This does not certify lifecycle Prepare or callback quiescence.
    // Its selected synthetic client position was (24,26); an installed cursor
    // detour always returns that value until removed. No further input is sent.
    wait_for("healthy second target cursor hook removal", || {
        let point = (
            query(&targets[1], CURSOR_X)? as isize,
            query(&targets[1], CURSOR_Y)? as isize,
        );
        ensure!(
            point.0 != isize::MIN && point.1 != isize::MIN,
            "healthy target cursor API query failed"
        );
        Ok(point != (24, 26))
    })?;
    drop(session);
    if mode == "rust" {
        assert_no_native_loader()?;
    }
    println!("MIGRATION_CLEANUP_REJECTED {mode} 2 {correlation}");
    std::io::stdout().flush()?;
    Ok(())
}

fn assert_pending(commit: &CommitOwner) -> Result<()> {
    let handle = unsafe { OpenFileMappingW(FILE_MAP_READ, 0, wide(commit.name()).as_ptr()) };
    ensure!(!handle.is_null(), "supervisor transaction disappeared");
    let view = unsafe { MapViewOfFile(handle, FILE_MAP_READ, 0, 0, 64) };
    if view.Value.is_null() {
        unsafe {
            CloseHandle(handle);
        }
        bail!("map supervisor transaction");
    }
    let record = unsafe { ptr::read_volatile(view.Value.cast::<[u32; 4]>()) };
    unsafe {
        UnmapViewOfFile(view);
        CloseHandle(handle);
    }
    ensure!(
        record == [0x4d58_434d, 1, 64, 0],
        "failed cleanup transaction was committed or corrupted"
    );
    Ok(())
}

fn assert_dirty_originals(fixtures: &mut [OwnedTarget]) -> Result<()> {
    for fixture in fixtures {
        ensure!(
            fixture.process.child.try_wait()?.is_none(),
            "failed controller terminated an original recipient"
        );
        let readiness = targets::eligibility(fixture.selected.pid);
        ensure!(
            readiness.creation == Some(fixture.selected.creation),
            "original target identity changed"
        );
        ensure!(
            readiness.state == EligibilityState::Dirty,
            "partial cleanup granted target reuse: {}",
            readiness.reason
        );
    }
    Ok(())
}

fn wait_for_rejected_exit(running: &mut RunningController, mode: &str) -> Result<()> {
    let deadline = Instant::now() + PROCESS_LIMIT;
    let expected = format!("MIGRATION_CLEANUP_REJECTED {mode} 2 {}", running.token);
    let mut rejected = false;
    let mut disconnected = false;
    loop {
        match running
            .process
            .output
            .recv_timeout(Duration::from_millis(10))
        {
            Ok(line) => {
                println!("fixture PID {}: {line}", running.process.child.id());
                ensure!(
                    !line.contains("MIGRATION_STOPPED"),
                    "failed cleanup emitted successful Stopped evidence"
                );
                if let Some(offset) = line.find("MIGRATION_CLEANUP_REJECTED ") {
                    ensure!(
                        line[offset..] == expected,
                        "cleanup rejection correlation failed"
                    );
                    ensure!(!rejected, "duplicate cleanup rejection");
                    rejected = true;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => disconnected = true,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some(status) = running.process.child.try_wait()?
            && disconnected
        {
            ensure!(
                status.success() && rejected,
                "fault controller did not exit with correlated expected-failure evidence"
            );
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "cleanup fault controller did not finish"
        );
    }
}

#[test]
#[ignore = "opt-in actual two-target nonpumping cleanup failure and late native-fence quarantine"]
fn rust_and_native_nonpumping_cleanup_never_grants_reuse() -> Result<()> {
    let runtime = PathBuf::from(std::env::var_os(RUNTIME).context("native runtime opt-in")?)
        .canonicalize()?;
    for mode in ["rust", "native"] {
        let mut fixtures = vec![
            spawn_target_configured(false, true)?,
            spawn_target_configured(false, true)?,
        ];
        let mut running = start_controller(&runtime, mode, &fixtures, "fence-failure")?;
        let requested = running
            .process
            .line("MIGRATION_PAUSE_REQUESTED ", Instant::now() + PROCESS_LIMIT)?;
        ensure!(
            requested
                == format!(
                    "MIGRATION_PAUSE_REQUESTED {mode} {} {}",
                    fixtures[0].selected.pid, running.token
                ),
            "pause request correlation failed"
        );
        let first = fixtures[0].selected.clone();
        let paused = fixtures[0]
            .process
            .line("MIGRATION_PUMP_PAUSED ", Instant::now() + POLL_LIMIT)?;
        ensure!(
            paused == format!("MIGRATION_PUMP_PAUSED {} {}", first.pid, first.token),
            "original GUI pause correlation failed"
        );
        writeln!(
            running
                .process
                .child
                .stdin
                .as_mut()
                .context("controller pause confirmation pipe")?,
            "PAUSE_CONFIRMED {}",
            running.token
        )?;
        wait_for_rejected_exit(&mut running, mode)?;
        drop(running.job);
        wait_for("dirty originals after partial cleanup", || {
            for fixture in &mut fixtures {
                ensure!(
                    fixture.process.child.try_wait()?.is_none(),
                    "original recipient exited"
                );
                let readiness = targets::eligibility(fixture.selected.pid);
                ensure!(
                    readiness.creation == Some(fixture.selected.creation),
                    "original creation changed"
                );
                if readiness.state != EligibilityState::Dirty {
                    return Ok(false);
                }
            }
            Ok(true)
        })?;
        assert_dirty_originals(&mut fixtures)?;
        assert_pending(&running.commit)?;
        writeln!(
            fixtures[0]
                .process
                .child
                .stdin
                .as_mut()
                .context("owned target resume pipe")?,
            "RESUME_PUMP {}",
            first.token
        )?;
        let pending = fixtures[0]
            .process
            .line("MIGRATION_LATE_FENCE_PENDING ", Instant::now() + POLL_LIMIT)?;
        ensure!(
            pending == format!("MIGRATION_LATE_FENCE_PENDING {} {}", first.pid, first.token),
            "late fence observation correlation failed"
        );
        let consumed = fixtures[0].process.line(
            "MIGRATION_LATE_FENCE_CONSUMED ",
            Instant::now() + POLL_LIMIT,
        )?;
        ensure!(
            consumed
                == format!(
                    "MIGRATION_LATE_FENCE_CONSUMED {} {}",
                    first.pid, first.token
                ),
            "late fence consumption correlation failed"
        );
        for fixture in &fixtures {
            validate_owned_target(&fixture.selected, std::process::id())?;
        }
        assert_dirty_originals(&mut fixtures)?;
        assert_pending(&running.commit)?;
        println!(
            "MIGRATION_PARTIAL_CLEANUP_QUARANTINED mode={mode} two original targets alive/dirty; healthy cursor hook removal verified; real late fence consumed; transaction remains pending"
        );
        // No Stopped evidence was accepted, so this transaction is never committed.
    }
    Ok(())
}
