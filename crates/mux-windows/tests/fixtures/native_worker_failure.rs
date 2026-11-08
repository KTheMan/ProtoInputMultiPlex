//! Actual production-worker EOF and original-target exit with held input.
use super::*;
use windows_sys::Win32::System::Memory::{
    FILE_MAP_READ, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile,
};

#[path = "native_worker_backpressure.rs"]
mod backpressure;

fn assert_pending(commit: &CommitOwner) -> Result<()> {
    let handle = unsafe { OpenFileMappingW(FILE_MAP_READ, 0, wide(commit.name()).as_ptr()) };
    ensure!(!handle.is_null(), "worker commitment mapping disappeared");
    let view = unsafe { MapViewOfFile(handle, FILE_MAP_READ, 0, 0, 64) };
    if view.Value.is_null() {
        unsafe { CloseHandle(handle) };
        bail!("map worker commitment");
    }
    let record = unsafe { ptr::read_volatile(view.Value.cast::<[u32; 4]>()) };
    unsafe {
        UnmapViewOfFile(view);
        CloseHandle(handle);
    }
    ensure!(
        record == [0x4d58_434d, 1, 64, 0],
        "failed worker commitment was committed or corrupted"
    );
    Ok(())
}

// Read through the actual stdout EOF, not a temporarily empty receiver. Keep the
// Job alive until the worker itself exits; timeout cleanup is only the Drop path.
struct Terminal {
    stopped: bool,
    failures: Vec<String>,
    actual_failed: Vec<String>,
}

fn terminal_exit(
    worker: &mut ProductionWorker,
    deadline: Instant,
    allow_stopped: bool,
) -> Result<Terminal> {
    let mut failures = Vec::new();
    let mut actual_failed = Vec::new();
    let mut stopped = false;
    let mut eof = false;
    let mut configured = worker.events.configured.is_some();
    loop {
        match worker.process.output.recv_timeout(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(10)),
        ) {
            Ok(line) => {
                if let Some(encoded) = line.strip_prefix(EVENT_PREFIX) {
                    let event: Value = serde_json::from_str(encoded)?;
                    if let Some(detail) = event
                        .as_object()
                        .and_then(|value| value.get("Failed"))
                        .and_then(Value::as_str)
                    {
                        actual_failed.push(detail.to_owned());
                    }
                }
                println!(
                    "failure fixture worker PID {}: {line}",
                    worker.process.child.id()
                );
                match decode_event(&line)? {
                    Some(WorkerEvent::Stopped) => {
                        ensure!(
                            allow_stopped && worker.events.stop_requested,
                            "failed worker emitted Stopped without valid Stop"
                        );
                        ensure!(!stopped, "duplicate Stopped");
                        stopped = true;
                    }
                    Some(WorkerEvent::Failed(detail)) => failures.push(detail),
                    Some(WorkerEvent::Configured(_)) => {
                        ensure!(!configured, "failed worker duplicated Configured");
                        configured = true;
                        worker.events.configured = Some(0);
                    }
                    _ => {}
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => eof = true,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some(status) = worker.process.child.try_wait()?
            && eof
        {
            if stopped {
                ensure!(
                    status.success() && failures.is_empty(),
                    "Stopped lacked successful error-free exit"
                );
            } else {
                ensure!(!status.success(), "failed worker exited successfully");
                ensure!(
                    !actual_failed.is_empty(),
                    "failed worker lacked terminal Failed evidence"
                );
            }
            worker.writer.take();
            if let Some(writer) = worker.writer_thread.take() {
                writer
                    .join()
                    .map_err(|_| anyhow::anyhow!("failure writer panicked"))?;
            }
            if let Some(reader) = worker.reader_thread.take() {
                reader
                    .join()
                    .map_err(|_| anyhow::anyhow!("failure reader panicked"))?;
            }
            worker.job.take();
            assert_pending(&worker.commit)?;
            return Ok(Terminal {
                stopped,
                failures,
                actual_failed,
            });
        }
        ensure!(
            Instant::now() < deadline,
            "failed worker exit/EOF deadline expired"
        );
    }
}

fn rejected_exit(worker: &mut ProductionWorker, deadline: Instant) -> Result<Vec<String>> {
    Ok(terminal_exit(worker, deadline, false)?.failures)
}

fn hold_inputs(
    image: &Path,
    runtime: &Path,
    mode: &str,
    fixtures: &mut [OwnedTarget],
) -> Result<(ProductionWorker, targets::ProcessWatch)> {
    let watch = targets::ProcessWatch::open(fixtures.iter().map(|fixture| fixture.selected.pid))?;
    assert_originals(fixtures, &watch)?;
    for fixture in fixtures.iter() {
        reset(&fixture.selected)?;
    }
    let mut worker = ProductionWorker::spawn(image, runtime, mode)?;
    let startup = Instant::now() + Duration::from_secs(45 * fixtures.len() as u64);
    worker.send(&serde_json::to_value(assigned_profile(fixtures)?)?, startup)?;
    while worker.events.configured.is_none() {
        worker.poll_event(startup, &watch)?;
    }
    ensure!(
        worker.events.configured == Some(fixtures.len()),
        "wrong Configured count"
    );
    let deadline = Instant::now() + COMMAND_LIMIT;
    // Match the measured worker fixture's receiver-ordered warm-up. The native
    // reference does not certify cold first-key delivery from Configured alone.
    for (index, fixture) in fixtures.iter().enumerate() {
        let mouse = emulated::device(DeviceKind::Mouse, index as u32 + 1)?.handle as u32;
        for released in [false, true] {
            worker.send(
                &json!({"Input": Packet::button(mouse, 0, released)}),
                deadline,
            )?;
        }
        wait_for("fault fixture receiver-ordered warm-up", || {
            worker.drain_events()?;
            Ok(query(&fixture.selected, 1)? == 1 && query(&fixture.selected, 2)? == 1)
        })?;
    }
    for fixture in fixtures.iter() {
        ensure!(
            unsafe {
                PostMessageW(
                    fixture.selected.hwnd as HWND,
                    PUMP_WAKE,
                    fixture.selected.token as usize,
                    0,
                )
            } != 0,
            "post owned fault fixture pump marker"
        );
        wait_for("fault fixture post-setup pump turn", || {
            Ok(query(&fixture.selected, 35)? == fixture.selected.token as usize)
        })?;
        reset(&fixture.selected)?;
    }
    for index in 0..fixtures.len() {
        let mouse = emulated::device(DeviceKind::Mouse, index as u32 + 1)?.handle as u32;
        let keyboard = emulated::device(DeviceKind::Keyboard, index as u32 + 1)?.handle as u32;
        for packet in [
            Packet::key(keyboard, 0x41, false),
            Packet::button(mouse, 0, false),
        ] {
            worker.send(&json!({"Input": packet}), deadline)?;
        }
    }
    loop {
        worker.drain_events()?;
        ensure!(
            !worker.events.stopped,
            "worker stopped before fault injection"
        );
        ensure!(
            worker.events.input_sent <= fixtures.len() * 4,
            "extra input acknowledgements"
        );
        let mut held = worker.events.input_sent == fixtures.len() * 4;
        for fixture in fixtures.iter() {
            let observed = evidence(&fixture.selected)?;
            ensure!(
                observed.keys_up == 0 && observed.buttons_up == 0,
                "input released before fault"
            );
            held &= observed.keys_down == 1 && observed.buttons_down == 1;
        }
        if held {
            break;
        }
        worker.poll_event(deadline, &watch)?;
    }
    assert_originals(fixtures, &watch)?;
    for fixture in fixtures.iter() {
        for recipient in [query(&fixture.selected, 31)?, query(&fixture.selected, 33)?] {
            ensure!(recipient != 0, "held input lacked original recipient HWND");
            validate_owned_target(
                &SelectedTarget {
                    hwnd: recipient,
                    ..fixture.selected.clone()
                },
                std::process::id(),
            )?;
        }
    }
    assert_pending(&worker.commit)?;
    Ok((worker, watch))
}

fn commit_clean_survivors(worker: &ProductionWorker, fixtures: &mut [OwnedTarget]) -> Result<()> {
    let watch = targets::ProcessWatch::open(fixtures.iter().map(|fixture| fixture.selected.pid))?;
    assert_originals(fixtures, &watch)?;
    for fixture in fixtures.iter() {
        let ready = targets::eligibility(fixture.selected.pid);
        ensure!(
            ready.creation == Some(fixture.selected.creation),
            "survivor identity changed before commit"
        );
        ensure!(
            ready.state != EligibilityState::Reusable,
            "survivor reused before commit"
        );
        ensure!(
            evidence(&fixture.selected)?
                == MessageEvidence {
                    buttons_down: 1,
                    buttons_up: 1,
                    keys_down: 1,
                    keys_up: 1,
                    authored_text: 0
                },
            "Stop after peer exit lacked exact healthy-recipient releases"
        );
        for (down, up) in [(31, 32), (33, 34)] {
            let recipient = query(&fixture.selected, down)?;
            ensure!(
                recipient != 0 && query(&fixture.selected, up)? == recipient,
                "Stop after peer exit released to a different HWND"
            );
            validate_owned_target(
                &SelectedTarget {
                    hwnd: recipient,
                    ..fixture.selected.clone()
                },
                std::process::id(),
            )?;
        }
    }
    assert_pending(&worker.commit)?;
    // Only error-free Stopped plus successful exit/EOF reaches this function.
    // Production Stopped follows complete router.shutdown, including all live
    // recipient fences. An exited original contributes exit, not reuse evidence.
    worker.commit.commit()?;
    wait_for("valid Stop commits only healthy original survivors", || {
        assert_originals(fixtures, &watch)?;
        let mut reusable = true;
        for fixture in fixtures.iter() {
            let ready = targets::eligibility(fixture.selected.pid);
            ensure!(
                ready.creation == Some(fixture.selected.creation),
                "survivor identity changed after commit"
            );
            reusable &= ready.state == EligibilityState::Reusable;
        }
        Ok(reusable)
    })?;
    Ok(())
}

fn quarantined_survivors(
    image: &Path,
    runtime: &Path,
    mode: &str,
    fixtures: &mut [OwnedTarget],
) -> Result<()> {
    if fixtures.is_empty() {
        return Ok(());
    }
    let watch = targets::ProcessWatch::open(fixtures.iter().map(|fixture| fixture.selected.pid))?;
    wait_for(
        "surviving originals become Dirty after worker failure",
        || {
            assert_originals(fixtures, &watch)?;
            let mut dirty = true;
            for fixture in fixtures.iter() {
                let ready = targets::eligibility(fixture.selected.pid);
                ensure!(
                    ready.creation == Some(fixture.selected.creation),
                    "survivor identity changed"
                );
                ensure!(
                    ready.state != EligibilityState::Reusable,
                    "failure granted survivor reuse"
                );
                dirty &= ready.state == EligibilityState::Dirty;
            }
            Ok(dirty)
        },
    )?;
    // Retry through the same actual production-worker entry point and backend,
    // rather than just asserting a readiness enum from the failed controller.
    let mut retry = ProductionWorker::spawn(image, runtime, mode)?;
    let deadline = Instant::now() + PROCESS_LIMIT;
    retry.send(
        &serde_json::to_value(assigned_profile(fixtures)?)?,
        deadline,
    )?;
    let failures = rejected_exit(&mut retry, deadline)?;
    ensure!(
        retry.events.configured.is_none(),
        "restart configured a Dirty original"
    );
    assert_originals(fixtures, &watch)?;
    for fixture in fixtures.iter() {
        ensure!(
            targets::eligibility(fixture.selected.pid).state == EligibilityState::Dirty,
            "restart changed survivor quarantine"
        );
    }
    println!(
        "MIGRATION_WORKER_FAILURE_RESTART mode={mode} survivors={} failures={failures:?}",
        fixtures.len()
    );
    Ok(())
}

#[test]
#[ignore = "opt-in production worker original-target exit with actually held input"]
fn rust_and_native_target_exit_during_held_input_preserves_stop_contract() -> Result<()> {
    let image = production_executable()?;
    let runtime = PathBuf::from(std::env::var_os(RUNTIME).context("native runtime opt-in")?)
        .canonicalize()?;
    for mode in ["rust", "native"] {
        for count in [1, 2] {
            let mut fixtures = (0..count)
                .map(|_| spawn_target_with_inventory(true))
                .collect::<Result<Vec<_>>>()?;
            let (mut worker, watch) = hold_inputs(&image, &runtime, mode, &mut fixtures)?;
            let exited = fixtures[0].selected.clone();
            fixtures[0].process.child.kill()?;
            let exit = fixtures[0].process.wait(Instant::now() + EXIT_LIMIT)?;
            ensure!(
                !exit.success(),
                "terminated owned target exited successfully"
            );
            ensure!(
                watch.exited() == Some(exited.pid),
                "retained original process handle did not report exit"
            );
            worker.events.stop_requested = true;
            let deadline = Instant::now() + Duration::from_secs(30);
            // A worker that already detected this same exit may close stdin
            // first. Either write outcome still requires actual exit and EOF;
            // only a successful Stop can authorize the clean commitment path.
            let stop_write = worker.send(&json!("Stop"), deadline);
            let terminal = terminal_exit(&mut worker, deadline, true)?;
            if terminal.stopped {
                ensure!(stop_write.is_ok(), "Stopped without accepted Stop write");
                commit_clean_survivors(&worker, &mut fixtures[1..])?;
            } else {
                quarantined_survivors(&image, &runtime, mode, &mut fixtures[1..])?;
                assert_pending(&worker.commit)?;
            }
            ensure!(
                watch.exited() == Some(exited.pid),
                "dead original was replaced or called reusable"
            );
            println!(
                "MIGRATION_PRODUCTION_WORKER_TARGET_EXIT mode={mode} targets={count} exited_pid={} Stop_write={stop_write:?} stopped={} failures={:?}; original exit is not reuse; healthy survivors require valid Stop or remain quarantined",
                exited.pid, terminal.stopped, terminal.failures
            );
        }
    }
    Ok(())
}

#[test]
#[ignore = "opt-in production worker real stdin EOF with actually held input"]
fn rust_and_native_stdin_eof_during_held_input_never_commits() -> Result<()> {
    let image = production_executable()?;
    let runtime = PathBuf::from(std::env::var_os(RUNTIME).context("native runtime opt-in")?)
        .canonicalize()?;
    for mode in ["rust", "native"] {
        for count in [1, 2] {
            let mut fixtures = (0..count)
                .map(|_| spawn_target_with_inventory(true))
                .collect::<Result<Vec<_>>>()?;
            let (mut worker, watch) = hold_inputs(&image, &runtime, mode, &mut fixtures)?;
            worker.writer.take();
            worker
                .writer_thread
                .take()
                .context("owned stdin writer")?
                .join()
                .map_err(|_| anyhow::anyhow!("EOF writer panicked"))?;
            let failures = rejected_exit(&mut worker, Instant::now() + Duration::from_secs(30))?;
            assert_originals(&mut fixtures, &watch)?;
            quarantined_survivors(&image, &runtime, mode, &mut fixtures)?;
            assert_pending(&worker.commit)?;
            println!(
                "MIGRATION_PRODUCTION_WORKER_STDIN_EOF mode={mode} targets={count} failures={failures:?}; no Stopped or Commit; original survivors quarantined and restart rejected"
            );
        }
    }
    Ok(())
}
