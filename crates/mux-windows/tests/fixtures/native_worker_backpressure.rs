//! Rust production Stop while its real control pipe cannot make progress.
use super::*;
use std::{collections::BTreeSet, os::windows::io::AsRawHandle};
use windows_sys::Win32::{
    Foundation::{FILETIME, HANDLE, INVALID_HANDLE_VALUE, WAIT_TIMEOUT},
    System::{
        Diagnostics::ToolHelp::{TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next},
        Threading::{
            GetProcessId, GetProcessIdOfThread, GetProcessTimes, GetThreadIOPendingFlag,
            GetThreadTimes, OpenThread, ResumeThread, SuspendThread, THREAD_QUERY_INFORMATION,
            THREAD_SUSPEND_RESUME, WaitForSingleObject,
        },
    },
};

struct Handle(HANDLE);

impl Handle {
    fn checked(handle: HANDLE, operation: &str) -> Result<Self> {
        ensure!(
            !handle.is_null() && handle != INVALID_HANDLE_VALUE,
            "{operation}: {}",
            std::io::Error::last_os_error()
        );
        Ok(Self(handle))
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

fn thread_ids(pids: &[u32]) -> Result<Vec<(u32, u32)>> {
    let snapshot = Handle::checked(
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) },
        "snapshot threads",
    )?;
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    ensure!(
        unsafe { Thread32First(snapshot.0, &mut entry) } != 0,
        "first thread in owned snapshot"
    );
    let mut result = Vec::new();
    loop {
        if pids.contains(&entry.th32OwnerProcessID) {
            result.push((entry.th32OwnerProcessID, entry.th32ThreadID));
        }
        if unsafe { Thread32Next(snapshot.0, &mut entry) } == 0 {
            ensure!(
                unsafe { windows_sys::Win32::Foundation::GetLastError() }
                    == windows_sys::Win32::Foundation::ERROR_NO_MORE_FILES,
                "enumerate owned threads: {}",
                std::io::Error::last_os_error()
            );
            break;
        }
    }
    Ok(result)
}

fn open_thread(pid: u32, id: u32, suspend: bool) -> Result<Handle> {
    let access = THREAD_QUERY_INFORMATION | if suspend { THREAD_SUSPEND_RESUME } else { 0 };
    let handle = Handle::checked(
        unsafe { OpenThread(access, 0, id) },
        "open retained owned thread",
    )?;
    ensure!(
        unsafe { GetProcessIdOfThread(handle.0) } == pid,
        "retained thread does not belong to original PID {pid}"
    );
    Ok(handle)
}

fn creation(handle: &Handle) -> Result<u64> {
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    ensure!(
        unsafe { GetThreadTimes(handle.0, &mut created, &mut exited, &mut kernel, &mut user) } != 0,
        "query retained thread creation"
    );
    Ok((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

fn primary_thread(worker: &ProductionWorker) -> Result<Handle> {
    let pid = worker.process.child.id();
    let mut threads = thread_ids(&[pid])?
        .into_iter()
        .map(|(_, id)| -> Result<_> {
            let thread = open_thread(pid, id, false)?;
            Ok((creation(&thread)?, thread))
        })
        .collect::<Result<Vec<_>>>()?;
    threads.sort_unstable_by_key(|(created, _)| *created);
    ensure!(
        !threads.is_empty(),
        "production worker has no retained thread"
    );
    ensure!(
        threads.len() == 1 || threads[0].0 != threads[1].0,
        "oldest production worker thread creation times tied"
    );
    let (_, thread) = threads.remove(0);
    ensure!(
        !io_pending(&thread)?,
        "production worker primary thread already has pending I/O"
    );
    Ok(thread)
}

fn io_pending(thread: &Handle) -> Result<bool> {
    let mut pending = 0;
    ensure!(
        unsafe { GetThreadIOPendingFlag(thread.0, &mut pending) } != 0,
        "query retained worker thread I/O flag"
    );
    Ok(pending != 0)
}

struct SuspendedThread {
    handle: Handle,
    resume: bool,
}

struct SuspendedTargets {
    threads: Vec<SuspendedThread>,
}

fn retained_original(fixture: &OwnedTarget) -> Result<()> {
    let handle = fixture.process.child.as_raw_handle() as HANDLE;
    ensure!(
        unsafe { GetProcessId(handle) } == fixture.selected.pid,
        "owned child handle changed PID"
    );
    let mut times = [FILETIME::default(); 4];
    ensure!(
        unsafe {
            GetProcessTimes(
                handle,
                &mut times[0],
                &mut times[1],
                &mut times[2],
                &mut times[3],
            )
        } != 0,
        "read retained owned process creation"
    );
    let created = (u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime);
    ensure!(
        created == fixture.selected.creation,
        "retained original creation changed"
    );
    ensure!(
        unsafe { WaitForSingleObject(handle, 0) } == WAIT_TIMEOUT,
        "retained original process exited"
    );
    Ok(())
}

impl SuspendedTargets {
    fn own(fixtures: &[OwnedTarget]) -> Result<Self> {
        for fixture in fixtures {
            validate_owned_target(&fixture.selected, std::process::id())?;
            retained_original(fixture)?;
        }
        let pids = fixtures
            .iter()
            .map(|fixture| fixture.selected.pid)
            .collect::<Vec<_>>();
        let mut guard = Self {
            threads: Vec::new(),
        };
        let mut retained = BTreeSet::new();
        for _ in 0..8 {
            for fixture in fixtures {
                retained_original(fixture)?;
            }
            let current = thread_ids(&pids)?;
            ensure!(!current.is_empty(), "owned target threads disappeared");
            for (pid, id) in current {
                if retained.contains(&(pid, id)) {
                    continue;
                }
                let thread = open_thread(pid, id, true)?;
                let previous = unsafe { SuspendThread(thread.0) };
                ensure!(
                    previous != u32::MAX,
                    "suspend retained owned target thread: {}",
                    std::io::Error::last_os_error()
                );
                // Own the successful increment before any further fallible
                // identity, suspend-count or snapshot check can unwind.
                guard.threads.push(SuspendedThread {
                    handle: thread,
                    resume: true,
                });
                ensure!(previous == 0, "owned target thread was already suspended");
                retained.insert((pid, id));
            }
            for fixture in fixtures {
                retained_original(fixture)?;
            }
            // Once every thread in the current snapshot is suspended, those
            // owned processes cannot create another receiver thread themselves.
            let current = thread_ids(&pids)?;
            if pids
                .iter()
                .all(|pid| current.iter().any(|(owner, _)| owner == pid))
                && current.iter().all(|thread| retained.contains(thread))
            {
                return Ok(guard);
            }
        }
        bail!("owned target thread set did not stabilize within eight snapshots")
    }

    fn resume(&mut self) -> Result<()> {
        let mut errors = Vec::new();
        for thread in self.threads.iter_mut().rev() {
            if thread.resume {
                // Release only this fixture's increment, exactly once. A failed
                // OS resume rejects the gate; owned fixture Drop still contains
                // the target process after this guard is unwound.
                thread.resume = false;
                let previous = unsafe { ResumeThread(thread.handle.0) };
                if previous != 1 {
                    errors.push(format!(
                        "resume owned thread returned {previous}: {}",
                        std::io::Error::last_os_error()
                    ));
                }
            }
        }
        ensure!(errors.is_empty(), "{}", errors.join("; "));
        Ok(())
    }
}

impl Drop for SuspendedTargets {
    fn drop(&mut self) {
        let _ = self.resume();
    }
}

#[test]
#[ignore = "opt-in Rust production worker Stop during a real pending control-pipe write"]
fn rust_stop_cancels_pending_control_pipe_without_commit() -> Result<()> {
    let image = production_executable()?;
    let runtime = PathBuf::from(std::env::var_os(RUNTIME).context("native runtime opt-in")?)
        .canonicalize()?;
    for count in [1, 2] {
        let mut fixtures = (0..count)
            .map(|_| spawn_target_with_inventory(true))
            .collect::<Result<Vec<_>>>()?;
        let (mut worker, watch) = hold_inputs(&image, &runtime, "rust", &mut fixtures)?;
        let primary = primary_thread(&worker)?;
        let mut suspended = SuspendedTargets::own(&fixtures)?;
        ensure!(
            !io_pending(&primary)?,
            "worker primary thread had pending I/O before flood"
        );
        let started = Instant::now();
        let trigger_due = started + Duration::from_millis(750);
        // 512 noncoalesced edges can produce 16 KiB of real 8-byte-header +
        // 24-byte Input bodies, exceeding the actual 8192-byte pipe capacity.
        // The finite two-line batch budget avoids an unbounded stdin backlog.
        // A flag during an individual InputSent stdout write is not the trigger:
        // require the budget queued and continuous pending I/O over two 10 ms
        // Operation::wait slices. The final Failed detail still proves which
        // pending operation was cancelled, rather than the flag or elapsed time.
        let keyboard = emulated::device(DeviceKind::Keyboard, 1)?.handle as u32;
        let batch = (0..256)
            .map(|edge| Packet::key_scanned(keyboard, 0x42, 0x30, 0, edge % 2 != 0))
            .collect::<Vec<_>>();
        for _ in 0..2 {
            worker.send(&json!({"Batch": batch}), trigger_due)?;
        }
        let submitted = batch.len() * 2;
        let mut pending_since = None;
        loop {
            ensure!(
                Instant::now() < trigger_due,
                "bounded 16 KiB input budget did not trigger pending worker I/O"
            );
            if io_pending(&primary)? {
                let since = pending_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_millis(20) {
                    break;
                }
            } else {
                pending_since = None;
            }
            std::thread::yield_now();
        }
        worker.events.stop_requested = true;
        worker.send(&json!("Stop"), started + Duration::from_millis(1000))?;
        let terminal = terminal_exit(&mut worker, Instant::now() + Duration::from_secs(5), false)?;
        ensure!(
            terminal
                .actual_failed
                .iter()
                .any(|detail| detail.contains("control-pipe transfer uncertain")
                    && detail.contains("control-pipe operation cancelled")
                    && !detail.contains("cancelled before transfer")),
            "Failed did not prove cancellation inside the pending control-pipe wait: {:?}",
            terminal.failures
        );
        // No query, target message, eligibility read or restart is attempted
        // while any target thread remains suspended by this fixture.
        suspended.resume()?;
        assert_originals(&mut fixtures, &watch)?;
        assert_pending(&worker.commit)?;
        quarantined_survivors(&image, &runtime, "rust", &mut fixtures)?;
        println!(
            "MIGRATION_PRODUCTION_WORKER_BACKPRESSURE mode=rust targets={count} submitted_B_edges={submitted} failures={:?}; actual pending-wait cancellation; no Stopped or Commit; resumed original survivors Dirty and restart rejected",
            terminal.failures
        );
    }
    Ok(())
}
