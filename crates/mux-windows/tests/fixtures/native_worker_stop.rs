//! Opt-in Stop comparison through the actual production session worker.
//! Results stay on stdout. This does not exercise the GUI or emergency broker.
use super::*;
use mux_core::{Profile, Seat};
use serde_json::{Value, json};
use std::{os::windows::io::AsRawHandle, sync::mpsc::Sender, thread::JoinHandle};
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

#[path = "native_worker_failure.rs"]
mod failure;

const WORKER_IMAGE: &str = "PROTOINPUTMULTIPLEX_MIGRATION_WORKER";
const BACKEND_CHOICE: &str = "PROTOINPUTMULTIPLEX_CONTROL_BACKEND";
const EVENT_PREFIX: &str = "MUX_EVENT:";
const HANDLE_COUNT: usize = 30;
const COMMAND_LIMIT: Duration = Duration::from_secs(10);
const EXIT_LIMIT: Duration = Duration::from_secs(3);
const CYCLES: usize = 30;

enum WorkerEvent {
    Configured(usize),
    InputSent,
    Stopped,
    Failed(String),
    Other,
}

fn decode_event(line: &str) -> Result<Option<WorkerEvent>> {
    let Some(encoded) = line.strip_prefix(EVENT_PREFIX) else {
        return Ok(None);
    };
    let event: Value = serde_json::from_str(encoded).context("decode production worker event")?;
    if let Some(unit) = event.as_str() {
        return match unit {
            "Stopped" => Ok(Some(WorkerEvent::Stopped)),
            "InputSent" => Ok(Some(WorkerEvent::InputSent)),
            "Paused" | "Resumed" => Ok(Some(WorkerEvent::Other)),
            _ => bail!("unexpected production worker unit event {unit}"),
        };
    }
    let object = event.as_object().context("worker event is not an enum")?;
    ensure!(object.len() == 1, "worker event has multiple variants");
    let (variant, value) = object.iter().next().context("empty worker event")?;
    match variant.as_str() {
        "Configured" => Ok(Some(WorkerEvent::Configured(
            value
                .as_u64()
                .context("invalid Configured seat count")?
                .try_into()?,
        ))),
        "Failed" | "InputRejected" | "CommandRejected" | "EmergencyUnavailable" => {
            Ok(Some(WorkerEvent::Failed(format!(
                "{variant}: {}",
                value.as_str().context("invalid worker failure detail")?
            ))))
        }
        "SettingsAcknowledged"
        | "WindowSelected"
        | "Output"
        | "Agent"
        | "Capture"
        | "VirtualUsers"
        | "MacroStudio"
        | "Magnifier"
        | "Annotation"
        | "Automation"
        | "Attaching"
        | "AttachmentFinished"
        | "ModeChanged"
        | "State" => {
            reject_reported_errors(value)?;
            Ok(Some(WorkerEvent::Other))
        }
        _ => bail!("unexpected production worker event variant {variant}"),
    }
}

fn reject_reported_errors(value: &Value) -> Result<()> {
    match value {
        Value::Object(object) => {
            if let Some(error) = object.get("error") {
                ensure!(error.is_null(), "worker state reported an error: {error}");
            }
            for value in object.values() {
                reject_reported_errors(value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                reject_reported_errors(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[derive(Default)]
struct EventState {
    configured: Option<usize>,
    input_sent: usize,
    stop_requested: bool,
    stopped: bool,
}

impl EventState {
    fn observe(&mut self, line: &str) -> Result<()> {
        let Some(event) = decode_event(line)? else {
            return Ok(());
        };
        match event {
            WorkerEvent::Configured(count) => {
                ensure!(self.configured.is_none(), "duplicate Configured event");
                ensure!(!self.stop_requested, "Configured after Stop request");
                self.configured = Some(count);
            }
            WorkerEvent::InputSent => self.input_sent += 1,
            WorkerEvent::Stopped => {
                ensure!(
                    self.stop_requested,
                    "worker stopped before the measured Stop"
                );
                ensure!(!self.stopped, "duplicate Stopped event");
                self.stopped = true;
            }
            WorkerEvent::Failed(detail) => bail!("production worker rejected the cycle: {detail}"),
            WorkerEvent::Other => {}
        }
        Ok(())
    }
}

struct WriteFrame {
    line: String,
    completed: Sender<std::io::Result<()>>,
}

struct ProductionWorker {
    process: OwnedProcess,
    job: Option<Job>,
    commit: CommitOwner,
    writer: Option<Sender<WriteFrame>>,
    writer_thread: Option<JoinHandle<()>>,
    reader_thread: Option<JoinHandle<()>>,
    events: EventState,
}

impl ProductionWorker {
    fn spawn(image: &Path, runtime: &Path, mode: &str) -> Result<Self> {
        ensure!(
            matches!(mode, "rust" | "native" | "default"),
            "invalid worker backend"
        );
        let commit = CommitOwner::create()?;
        let mut command = Command::new(image);
        command
            .arg("session-worker")
            .arg("--runtime")
            .arg(runtime)
            .arg("--controller-pid")
            .arg(std::process::id().to_string());
        if mode == "default" {
            command.env_remove(BACKEND_CHOICE);
        } else {
            command.env(BACKEND_CHOICE, mode);
        }
        let child = command
            .env(WORKER_ENV, commit.name())
            .creation_flags(0x0800_0000)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("spawn explicitly selected production session worker")?;
        let (output_sender, output) = mpsc::channel();
        let mut process = OwnedProcess { child, output };
        let stdout = process
            .child
            .stdout
            .take()
            .context("production worker stdout")?;
        // Establish the same kill-on-close containment as the real supervisor
        // before supplying the first profile line that can trigger attachment.
        let job = Job::own(&process.child)?;
        let mut worker = Self {
            process,
            job: Some(job),
            commit,
            writer: None,
            writer_thread: None,
            reader_thread: None,
            events: EventState::default(),
        };
        worker.reader_thread = Some(std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if output_sender.send(line).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = output_sender.send(format!(
                            "{EVENT_PREFIX}{}",
                            json!({"Failed": format!("worker stdout read failed: {error}")})
                        ));
                        break;
                    }
                }
            }
        }));
        let mut stdin = worker
            .process
            .child
            .stdin
            .take()
            .context("production worker stdin")?;
        let (writer, frames) = mpsc::channel::<WriteFrame>();
        worker.writer = Some(writer);
        worker.writer_thread = Some(std::thread::spawn(move || {
            for frame in frames {
                let result = (|| {
                    writeln!(stdin, "{}", frame.line)?;
                    stdin.flush()
                })();
                let failed = result.is_err();
                let _ = frame.completed.send(result);
                if failed {
                    break;
                }
            }
        }));
        Ok(worker)
    }

    fn send(&mut self, value: &Value, deadline: Instant) -> Result<()> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        ensure!(
            !remaining.is_zero(),
            "worker frame deadline expired before enqueue"
        );
        let (completed, receipt) = mpsc::channel();
        self.writer
            .as_ref()
            .context("production worker writer closed")?
            .send(WriteFrame {
                line: serde_json::to_string(value)?,
                completed,
            })
            .context("queue production worker frame")?;
        // The writer owns stdin; a nonreading child cannot block this deadline.
        let remaining = deadline.saturating_duration_since(Instant::now());
        ensure!(!remaining.is_zero(), "worker frame deadline expired");
        receipt
            .recv_timeout(remaining)
            .context("production worker frame did not complete by its deadline")??;
        ensure!(Instant::now() < deadline, "worker frame completed too late");
        Ok(())
    }

    fn drain_events(&mut self) -> Result<()> {
        for line in self.process.output.try_iter() {
            self.events.observe(&line)?;
        }
        Ok(())
    }

    fn poll_event(&mut self, deadline: Instant, watch: &targets::ProcessWatch) -> Result<()> {
        ensure!(
            watch.exited().is_none(),
            "original target exited during worker cycle"
        );
        let remaining = deadline.saturating_duration_since(Instant::now());
        ensure!(
            !remaining.is_zero(),
            "production worker event deadline expired"
        );
        match self
            .process
            .output
            .recv_timeout(remaining.min(Duration::from_millis(10)))
        {
            Ok(line) => self.events.observe(&line),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(()),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                ensure!(
                    self.events.stopped,
                    "worker EOF without Stopped; commitment forbidden"
                );
                Ok(())
            }
        }
    }

    fn finish_exit(&mut self, deadline: Instant) -> Result<()> {
        loop {
            self.drain_events()?;
            if let Some(status) = self.process.child.try_wait()? {
                ensure!(
                    status.success(),
                    "production worker failed after Stopped: {status}"
                );
                break;
            }
            ensure!(
                Instant::now() < deadline,
                "production worker exit deadline expired"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        ensure!(self.events.stopped, "successful worker exit lacked Stopped");
        // Release stdin only after the real Stop completed. EOF racing Stop is
        // deliberately unclean in production and cannot be a benchmark shortcut.
        self.writer.take();
        self.job.take();
        if let Some(writer) = self.writer_thread.take() {
            writer
                .join()
                .map_err(|_| anyhow::anyhow!("worker writer panicked"))?;
        }
        if let Some(reader) = self.reader_thread.take() {
            reader
                .join()
                .map_err(|_| anyhow::anyhow!("worker reader panicked"))?;
        }
        // Drain through EOF before committing: a trailing Failed must not hide
        // behind an earlier Stopped or a temporarily empty receiver.
        self.drain_events()
    }
}

impl Drop for ProductionWorker {
    fn drop(&mut self) {
        self.writer.take();
        self.job.take();
        let _ = self.process.child.kill();
        let _ = self.process.child.wait();
        // Kill/exit precedes joining a possibly blocked stdin writer.
        if let Some(writer) = self.writer_thread.take() {
            let _ = writer.join();
        }
        if let Some(reader) = self.reader_thread.take() {
            let _ = reader.join();
        }
    }
}

fn retained_worker_creation(worker: &ProductionWorker) -> Result<u64> {
    use windows_sys::Win32::{
        Foundation::{FILETIME, HANDLE, WAIT_TIMEOUT},
        System::Threading::{GetProcessId, GetProcessTimes, WaitForSingleObject},
    };
    let handle = worker.process.child.as_raw_handle() as HANDLE;
    ensure!(
        unsafe { GetProcessId(handle) } == worker.process.child.id(),
        "retained production worker PID changed"
    );
    ensure!(
        unsafe { WaitForSingleObject(handle, 0) } == WAIT_TIMEOUT,
        "original production worker exited before module inspection"
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
        "read retained production worker creation: {}",
        std::io::Error::last_os_error()
    );
    ensure!(
        unsafe { WaitForSingleObject(handle, 0) } == WAIT_TIMEOUT,
        "original production worker exited during identity inspection"
    );
    Ok((u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime))
}

fn assert_default_worker_has_no_native_loader(worker: &ProductionWorker) -> Result<()> {
    use windows_sys::Win32::{
        Foundation::{
            ERROR_BAD_LENGTH, ERROR_NO_MORE_FILES, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
        },
        System::Diagnostics::ToolHelp::{
            MODULEENTRY32W, Module32FirstW, Module32NextW, TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32,
        },
    };
    struct Snapshot(HANDLE);
    impl Drop for Snapshot {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }
    let creation = retained_worker_creation(worker)?;
    let pid = worker.process.child.id();
    let mut snapshot = None;
    for attempt in 0..8 {
        let handle =
            unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid) };
        if handle != INVALID_HANDLE_VALUE && !handle.is_null() {
            snapshot = Some(Snapshot(handle));
            break;
        }
        let error = unsafe { GetLastError() };
        ensure!(
            error == ERROR_BAD_LENGTH && attempt < 7,
            "snapshot original production worker modules: {}",
            std::io::Error::from_raw_os_error(error as i32)
        );
        std::thread::yield_now();
    }
    let snapshot = snapshot.context("production worker module snapshot missing")?;
    let mut entry = MODULEENTRY32W {
        dwSize: std::mem::size_of::<MODULEENTRY32W>() as u32,
        ..Default::default()
    };
    ensure!(
        unsafe { Module32FirstW(snapshot.0, &mut entry) } != 0,
        "enumerate first original production worker module: {}",
        std::io::Error::last_os_error()
    );
    loop {
        ensure!(
            entry.th32ProcessID == pid,
            "module snapshot changed original worker PID"
        );
        let length = entry
            .szModule
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(entry.szModule.len());
        let name = String::from_utf16_lossy(&entry.szModule[..length]);
        ensure!(
            !["ProtoInputLoader32.dll", "ProtoInputLoader64.dll"]
                .iter()
                .any(|loader| name.eq_ignore_ascii_case(loader)),
            "default production worker loaded native loader {name}"
        );
        if unsafe { Module32NextW(snapshot.0, &mut entry) } == 0 {
            let error = unsafe { GetLastError() };
            ensure!(
                error == ERROR_NO_MORE_FILES,
                "complete original production worker module enumeration: {}",
                std::io::Error::from_raw_os_error(error as i32)
            );
            break;
        }
    }
    ensure!(
        retained_worker_creation(worker)? == creation,
        "original production worker creation changed during module inspection"
    );
    println!("MIGRATION_DEFAULT_WORKER_NATIVE_LOADER_ABSENT pid={pid} creation={creation}");
    Ok(())
}

fn production_executable() -> Result<PathBuf> {
    let image = PathBuf::from(std::env::var_os(WORKER_IMAGE).with_context(|| {
        format!("set {WORKER_IMAGE} to the matching built protoinputmultiplex.exe")
    })?)
    .canonicalize()?;
    ensure!(
        image.is_file()
            && image
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("protoinputmultiplex.exe")),
        "worker opt-in must select the actual production protoinputmultiplex.exe"
    );
    let bytes = std::fs::read(&image)?;
    ensure!(bytes.get(..2) == Some(b"MZ"), "worker lacks DOS header");
    let offset = bytes.get(0x3c..0x40).context("worker lacks PE offset")?;
    let offset = u32::from_le_bytes(offset.try_into()?) as usize;
    let pe = bytes
        .get(offset..)
        .context("worker PE offset out of bounds")?;
    ensure!(pe.get(..4) == Some(b"PE\0\0"), "worker lacks PE header");
    let machine = if cfg!(target_arch = "x86") {
        [0x4c, 0x01]
    } else {
        [0x64, 0x86]
    };
    ensure!(
        pe.get(4..6) == Some(&machine),
        "worker/controller architecture mismatch"
    );
    Ok(image)
}

fn assigned_profile(fixtures: &[OwnedTarget]) -> Result<Profile> {
    let mut profile = Profile::default();
    for (index, fixture) in fixtures.iter().enumerate() {
        validate_owned_target(&fixture.selected, std::process::id())?;
        profile.seats.push(Seat {
            id: format!("migration-worker-stop-{}", index + 1),
            name: format!("Owned worker Stop fixture {}", index + 1),
            mouse: emulated::device(DeviceKind::Mouse, index as u32 + 1)?.path,
            keyboard: Some(emulated::device(DeviceKind::Keyboard, index as u32 + 1)?.path),
            target_pid: fixture.selected.pid,
            desktop_start: None,
            cursor: Default::default(),
            annotation: Default::default(),
            magnifier: Default::default(),
            output: Default::default(),
        });
    }
    profile.normalize_ids()?;
    ensure!(
        !profile.concurrent && profile.desktop.is_none(),
        "assigned fixture changed mode"
    );
    Ok(profile)
}

fn assert_originals(fixtures: &mut [OwnedTarget], watch: &targets::ProcessWatch) -> Result<()> {
    ensure!(watch.exited().is_none(), "original target process exited");
    for fixture in fixtures {
        ensure!(
            fixture.process.child.try_wait()?.is_none(),
            "owned original target exited"
        );
        validate_owned_target(&fixture.selected, std::process::id())?;
    }
    Ok(())
}

struct StopSample {
    elapsed_ns: u64,
    target_handles: Vec<usize>,
}

fn input_diagnostics(fixtures: &[OwnedTarget]) -> Result<Value> {
    let windows = targets::windows()?;
    let mut result = Vec::new();
    for fixture in fixtures {
        let selected = &fixture.selected;
        let native_hwnd = query(selected, 36)?;
        let mut class = [0u16; 256];
        let class_length = unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::GetClassNameW(
                native_hwnd as HWND,
                class.as_mut_ptr(),
                class.len() as i32,
            )
        };
        result.push(json!({
            "pid": selected.pid,
            "expected_hwnd": selected.hwnd,
            "secondary_hwnd": query(selected, provider_pilot::SECONDARY_HWND)?,
            "native_focus_hwnd_after_failure": native_hwnd,
            "native_focus_class": String::from_utf16_lossy(&class[..class_length.max(0) as usize]),
            "motion_messages": query(selected, 0)?,
            "buttons_and_keys": format!("{:?}", evidence(selected)?),
            "actual_recipients": [query(selected, 31)?, query(selected, 32)?, query(selected, 33)?, query(selected, 34)?],
            "inventory": windows.iter().filter(|window| window.pid == selected.pid)
                .map(|window| json!({"hwnd": window.hwnd, "title": window.title})).collect::<Vec<_>>(),
        }));
    }
    Ok(Value::Array(result))
}

fn cycle(
    image: &Path,
    runtime: &Path,
    mode: &str,
    fixtures: &mut [OwnedTarget],
) -> Result<StopSample> {
    cycle_with_warmup(image, runtime, mode, fixtures, true)
}

fn cycle_with_warmup(
    image: &Path,
    runtime: &Path,
    mode: &str,
    fixtures: &mut [OwnedTarget],
    warmup: bool,
) -> Result<StopSample> {
    let watch = targets::ProcessWatch::open(fixtures.iter().map(|fixture| fixture.selected.pid))?;
    assert_originals(fixtures, &watch)?;
    for fixture in fixtures.iter() {
        reset(&fixture.selected)?;
    }
    let profile = assigned_profile(fixtures)?;
    let mut worker = ProductionWorker::spawn(image, runtime, mode)?;
    let startup = Instant::now() + Duration::from_secs(45 * fixtures.len() as u64);
    worker.send(&serde_json::to_value(&profile)?, startup)?;
    while worker.events.configured.is_none() {
        worker.poll_event(startup, &watch)?;
    }
    ensure!(
        worker.events.configured == Some(fixtures.len()),
        "wrong Configured count"
    );
    if mode == "default" {
        assert_default_worker_has_no_native_loader(&worker)?;
    }
    let commands_due = Instant::now() + COMMAND_LIMIT;
    if warmup {
        // Native Configured certifies host submission, not completed target setup.
        // Consume a real warm-up button pair: it follows setup in the command pipe.
        // A queued fixture marker alone could arrive before native setup executes.
        for (index, fixture) in fixtures.iter().enumerate() {
            let mouse = emulated::device(DeviceKind::Mouse, index as u32 + 1)?.handle as u32;
            for released in [false, true] {
                worker.send(
                    &json!({"Input": Packet::button(mouse, 0, released)}),
                    commands_due,
                )?;
            }
            wait_for("receiver-ordered warm-up button pair", || {
                worker.drain_events()?;
                Ok(query(&fixture.selected, 1)? == 1 && query(&fixture.selected, 2)? == 1)
            })
            .with_context(|| {
                format!(
                    "warm-up index={index} receipts={} diagnostics={:?}",
                    worker.events.input_sent,
                    input_diagnostics(fixtures)
                )
            })?;
        }
        // A GetMessage call entered before injection is not retroactively detoured.
        // Prime a real post-setup pump turn before holding measured inputs. This
        // fixture does not certify first-input delivery into a blocked retrieval.
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
                "post owned fixture pump warm-up"
            );
            wait_for("post-injection fixture pump turn", || {
                Ok(query(&fixture.selected, 35)? == fixture.selected.token as usize)
            })?;
            reset(&fixture.selected)?;
        }
    }
    let expected_receipts = fixtures.len() * if warmup { 4 } else { 2 };
    for index in 0..fixtures.len() {
        let mouse = emulated::device(DeviceKind::Mouse, index as u32 + 1)?.handle as u32;
        let keyboard = emulated::device(DeviceKind::Keyboard, index as u32 + 1)?.handle as u32;
        for packet in [
            Packet::key(keyboard, 0x41, false),
            Packet::button(mouse, 0, false),
        ] {
            worker.send(&json!({"Input": packet}), commands_due)?;
        }
    }
    loop {
        worker.drain_events()?;
        ensure!(
            !worker.events.stopped,
            "worker stopped before held-input evidence"
        );
        let mut held = worker.events.input_sent == expected_receipts;
        ensure!(
            worker.events.input_sent <= expected_receipts,
            "unexpected extra input receipts"
        );
        for fixture in fixtures.iter() {
            let observed = evidence(&fixture.selected)?;
            ensure!(
                observed.keys_up == 0 && observed.buttons_up == 0,
                "input released before Stop"
            );
            held &= observed.keys_down == 1 && observed.buttons_down == 1;
        }
        if held {
            break;
        }
        worker.poll_event(commands_due, &watch).with_context(|| {
            format!(
                "waiting for held input: receipts={}, target counters={:?}",
                worker.events.input_sent,
                fixtures
                    .iter()
                    .map(|fixture| evidence(&fixture.selected))
                    .collect::<Vec<_>>()
            )
        })?;
    }
    assert_originals(fixtures, &watch)?;
    let recipients = fixtures
        .iter()
        .map(|fixture| -> Result<_> {
            let button = query(&fixture.selected, 31)?;
            let key = query(&fixture.selected, 33)?;
            for hwnd in [button, key] {
                ensure!(hwnd != 0, "held input lacks an actual recipient HWND");
                validate_owned_target(
                    &SelectedTarget {
                        hwnd,
                        ..fixture.selected.clone()
                    },
                    std::process::id(),
                )?;
            }
            Ok((button, key))
        })
        .collect::<Result<Vec<_>>>()?;
    worker.events.stop_requested = true;
    let start = Instant::now();
    // Match session::stop_deadline for this known one/two-target cohort.
    let stop_due = start + Duration::from_secs((fixtures.len() as u64 * 6 + 5).max(30));
    worker.send(&json!("Stop"), stop_due)?;
    while !worker.events.stopped {
        worker.poll_event(stop_due, &watch)?;
    }
    worker.finish_exit(stop_due.min(Instant::now() + EXIT_LIMIT))?;
    assert_originals(fixtures, &watch)?;
    for (fixture, (button, key)) in fixtures.iter().zip(&recipients) {
        let ready = targets::eligibility(fixture.selected.pid);
        ensure!(
            ready.creation == Some(fixture.selected.creation),
            "target creation changed"
        );
        ensure!(
            ready.state != EligibilityState::Reusable,
            "target reused before supervisor commit"
        );
        ensure!(
            evidence(&fixture.selected)?
                == MessageEvidence {
                    buttons_down: 1,
                    buttons_up: 1,
                    keys_down: 1,
                    keys_up: 1,
                    authored_text: 0,
                },
            "production Stop failed to deliver genuine original-recipient releases"
        );
        ensure!(
            query(&fixture.selected, 32)? == *button && query(&fixture.selected, 34)? == *key,
            "production Stop released input to a different HWND than its original down"
        );
    }
    // No error/EOF/unsuccessful-exit path above can reach this commitment.
    ensure!(
        Instant::now() < stop_due,
        "Stop deadline expired before commitment"
    );
    worker.commit.commit()?;
    loop {
        ensure!(
            watch.exited().is_none(),
            "target exited during committed reuse"
        );
        let mut reusable = true;
        for fixture in fixtures.iter() {
            let ready = targets::eligibility(fixture.selected.pid);
            ensure!(
                ready.creation == Some(fixture.selected.creation),
                "reuse changed original identity"
            );
            reusable &= ready.state == EligibilityState::Reusable;
        }
        if reusable {
            break;
        }
        ensure!(
            Instant::now() < stop_due,
            "committed readiness exceeded Stop deadline"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    ensure!(
        Instant::now() < stop_due,
        "production Stop exceeded its deadline"
    );
    let elapsed_ns = start.elapsed().as_nanos().try_into()?;
    assert_originals(fixtures, &watch)?;
    let target_handles = fixtures
        .iter()
        .map(|fixture| {
            let count = query(&fixture.selected, HANDLE_COUNT)?;
            ensure!(
                count > 0 && count < u32::MAX as usize,
                "invalid target handle count"
            );
            Ok(count)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(StopSample {
        elapsed_ns,
        target_handles,
    })
}

fn p95(samples: &mut [u64]) -> Result<u64> {
    ensure!(samples.len() == CYCLES, "missing production Stop samples");
    samples.sort_unstable();
    Ok(samples[(samples.len() * 95).div_ceil(100) - 1])
}

#[test]
#[ignore = "opt-in cold first-input production worker check without fixture warm-up"]
fn rust_virtual_production_worker_first_input() -> Result<()> {
    cold_first_input("rust")
}

#[test]
#[ignore = "opt-in production default Rust-loader absence and cold input/Stop acceptance; requires reviewed default policy"]
fn default_production_worker_first_input_uses_rust_without_native_loader() -> Result<()> {
    cold_first_input("default")
}

#[test]
#[ignore = "opt-in native-reference cold first-input diagnostic; existing limitations may fail"]
fn native_reference_production_worker_first_input() -> Result<()> {
    cold_first_input("native")
}

fn cold_first_input(mode: &str) -> Result<()> {
    let image = production_executable()?;
    let runtime = PathBuf::from(std::env::var_os(RUNTIME).context("native runtime opt-in")?)
        .canonicalize()?;
    for count in [1, 2] {
        for trial in 1..=3 {
            let mut fixtures = (0..count)
                .map(|_| spawn_target_with_inventory(true))
                .collect::<Result<Vec<_>>>()?;
            cycle_with_warmup(&image, &runtime, mode, &mut fixtures, false).with_context(|| {
                format!("cold first input mode={mode} targets={count} trial={trial}")
            })?;
            println!(
                "MIGRATION_COLD_FIRST_INPUT mode={mode} targets={count} trial={trial}; no fixture warm-up; original input and committed Stop verified"
            );
        }
    }
    Ok(())
}

#[test]
#[ignore = "opt-in two-target production worker held-input/Stop regression on owned fixtures"]
fn rust_virtual_production_worker_two_target_stop_smoke() -> Result<()> {
    let image = production_executable()?;
    let runtime = PathBuf::from(std::env::var_os(RUNTIME).context("native runtime opt-in")?)
        .canonicalize()?;
    for mode in ["rust", "native"] {
        let mut fixtures = (0..2)
            .map(|_| spawn_target_with_inventory(true))
            .collect::<Result<Vec<_>>>()?;
        let sample = cycle(&image, &runtime, mode, &mut fixtures)
            .with_context(|| format!("two-target worker Stop mode={mode}"))?;
        println!(
            "MIGRATION_TWO_TARGET_WORKER_STOP mode={mode} ns={} handles={:?}",
            sample.elapsed_ns, sample.target_handles
        );
    }
    Ok(())
}

#[test]
#[ignore = "opt-in production session-worker Stop comparison using newly owned fixtures only"]
fn rust_virtual_production_worker_stop_comparison() -> Result<()> {
    let image = production_executable()?;
    let runtime = PathBuf::from(std::env::var_os(RUNTIME).context("native runtime opt-in")?)
        .canonicalize()?;
    for count in [1, 2] {
        let mut rust = (0..count)
            .map(|_| spawn_target_with_inventory(true))
            .collect::<Result<Vec<_>>>()?;
        let mut native = (0..count)
            .map(|_| spawn_target_with_inventory(true))
            .collect::<Result<Vec<_>>>()?;
        let mut rust_stop = Vec::new();
        let mut native_stop = Vec::new();
        let mut rust_handles = Vec::new();
        let mut native_handles = Vec::new();
        for iteration in 0..CYCLES {
            let order = if iteration % 2 == 0 {
                ["rust", "native"]
            } else {
                ["native", "rust"]
            };
            for mode in order {
                let (fixtures, stops, handles) = if mode == "rust" {
                    (&mut rust, &mut rust_stop, &mut rust_handles)
                } else {
                    (&mut native, &mut native_stop, &mut native_handles)
                };
                let sample = cycle(&image, &runtime, mode, fixtures).with_context(|| {
                    format!(
                        "production Stop mode={mode} targets={count} cycle={}",
                        iteration + 1
                    )
                })?;
                stops.push(sample.elapsed_ns);
                handles.push(sample.target_handles);
            }
        }
        let rust_p95 = p95(&mut rust_stop)?;
        let native_p95 = p95(&mut native_stop)?;
        let limit = native_p95.saturating_add(250_000_000);
        println!(
            "MIGRATION_PRODUCTION_STOP targets={count} controller={}bit target={}bit cycles_per_backend={CYCLES} p95_Stop_to_Stopped_successful_exit_committed_Reusable_ns rust={rust_p95} native={native_p95} limit={limit} target_handle_samples_rust={rust_handles:?} target_handle_samples_native={native_handles:?}; actual_owned_target_UPs_and_survival_verified; GUI_and_emergency_broker_not_exercised",
            usize::BITS,
            if std::env::var_os(TARGET_FIXTURE).is_some() {
                32
            } else {
                usize::BITS
            },
        );
        ensure!(
            rust_p95 <= limit,
            "Rust production Stop p95 exceeds native + 250 ms"
        );
    }
    Ok(())
}
