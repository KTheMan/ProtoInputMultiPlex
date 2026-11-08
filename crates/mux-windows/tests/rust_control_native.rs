//! Opt-in native pilots using only owned migration test fixtures.
//! Set PROTOINPUTMULTIPLEX_MIGRATION_RUNTIME, then run the
//! `rust_virtual_native_compatibility_pilot` test with --ignored --exact --nocapture.
//! Evidence is printed to stdout; no runtime assets or reports are written.
#![cfg(windows)]

use anyhow::{Context, Result, bail, ensure};
use mux_core::{
    DeviceKind, ResolvedSeat,
    emulated::{self, Packet},
};
use mux_windows::{
    job::Job,
    lifecycle::{CommitOwner, WORKER_ENV},
    protoinput::Backend,
    targets::{self, EligibilityState},
};
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Write},
    os::windows::{ffi::OsStringExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HWND, LPARAM, LRESULT, POINT, WPARAM},
    Graphics::Gdi::ScreenToClient,
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
        LibraryLoader::GetModuleHandleW,
        Threading::{
            GetCurrentProcess, GetProcessHandleCount, OpenProcess,
            PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
        },
    },
    UI::{
        Input::KeyboardAndMouse::{GetAsyncKeyState, GetKeyState, GetKeyboardState},
        WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DispatchMessageW, GetCursorPos, GetForegroundWindow,
            GetMessageW, GetWindowThreadProcessId, LoadCursorW, MSG, PostQuitMessage,
            RegisterClassW, SMTO_ABORTIFHUNG, SMTO_BLOCK, SW_SHOWNOACTIVATE, SendMessageTimeoutW,
            SetCursor, ShowWindow, TranslateMessage, WM_APP, WM_CHAR, WM_DESTROY, WM_KEYDOWN,
            WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_SETCURSOR, WNDCLASSW,
            WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE,
        },
    },
};

const RUNTIME: &str = "PROTOINPUTMULTIPLEX_MIGRATION_RUNTIME";
const ROLE: &str = "PROTOINPUTMULTIPLEX_MIGRATION_ROLE";
const TOKEN: &str = "PROTOINPUTMULTIPLEX_MIGRATION_TOKEN";
const SUPERVISOR: &str = "PROTOINPUTMULTIPLEX_MIGRATION_SUPERVISOR";
const SELECTED: &str = "PROTOINPUTMULTIPLEX_MIGRATION_SELECTED";
const MODE: &str = "PROTOINPUTMULTIPLEX_MIGRATION_MODE";
const TARGET_FIXTURE: &str = "PROTOINPUTMULTIPLEX_MIGRATION_TARGET_FIXTURE";
const PILOT_KIND: &str = "PROTOINPUTMULTIPLEX_MIGRATION_KIND";
const LISTED_FIXTURE: &str = "PROTOINPUTMULTIPLEX_MIGRATION_LISTED_FIXTURE";
const QUERY: u32 = WM_APP + 91;
const RESET: u32 = WM_APP + 92;
const PUMP_WAKE: u32 = WM_APP + 93;
const CURSOR_X: usize = 10;
const CURSOR_Y: usize = 11;
const KEY_STATE: usize = 12;
const ASYNC_STATE: usize = 13;
const CHALLENGE: usize = 14;
const QUERY_TIMEOUT_MS: u32 = 500;
const POLL_LIMIT: Duration = Duration::from_secs(5);
const PROCESS_LIMIT: Duration = Duration::from_secs(90);
static COUNTERS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
static FIXTURE_TOKEN: AtomicUsize = AtomicUsize::new(0);
static SECONDARY: AtomicUsize = AtomicUsize::new(0);
// Actual recipients for left-button down/up and A down/up respectively.
static RECIPIENTS: [AtomicUsize; 4] = [const { AtomicUsize::new(0) }; 4];
static PUMP_WAKE_TOKEN: AtomicUsize = AtomicUsize::new(0);

#[path = "fixtures/native_cleanup_failure.rs"]
mod cleanup_failure;
#[path = "fixtures/native_incompatible.rs"]
mod incompatible;
#[path = "fixtures/native_input_surface.rs"]
mod input_surface;
#[path = "fixtures/native_performance.rs"]
mod performance;
#[path = "fixtures/native_providers.rs"]
mod provider_pilot;
#[path = "fixtures/native_scope_rejection.rs"]
mod scope_rejection;
#[path = "fixtures/native_short_frame.rs"]
mod short_frame;
#[path = "fixtures/native_worker_stop.rs"]
mod worker_stop;

#[derive(Debug, Deserialize, Serialize)]
struct PerformanceMetrics {
    input_ns: Vec<u64>,
    backend_shutdown_ns: u64,
    resources: performance::ControllerResources,
}

struct CycleEvidence {
    messages: Vec<MessageEvidence>,
    metrics: Option<PerformanceMetrics>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SelectedTarget {
    pid: u32,
    hwnd: usize,
    token: u32,
    creation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MessageEvidence {
    buttons_down: usize,
    buttons_up: usize,
    keys_down: usize,
    keys_up: usize,
    authored_text: usize,
}

struct OwnedProcess {
    child: Child,
    output: Receiver<String>,
}

impl OwnedProcess {
    fn spawn(role: &str, configure: impl FnOnce(&mut Command)) -> Result<Self> {
        let image = if role == "target" {
            target_executable()?
        } else {
            std::env::current_exe()?
        };
        let mut command = Command::new(image);
        if std::env::var_os(TARGET_FIXTURE).is_some() {
            // Freeze the supervisor's canonical choice in each child rather
            // than resolving an inherited relative path in another process.
            command.env(TARGET_FIXTURE, target_executable()?);
        }
        command
            .args([
                "--ignored",
                "--exact",
                "migration_process_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW; target creates its own no-activate HWND.
            .env(ROLE, role)
            .env_remove(WORKER_ENV)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        configure(&mut command);
        let mut child = command.spawn().context("spawn owned migration process")?;
        let stdout = child.stdout.take().context("owned process stdout")?;
        let (sender, output) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if sender.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self { child, output })
    }

    fn line(&mut self, prefix: &str, deadline: Instant) -> Result<String> {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(
                !remaining.is_zero(),
                "PID {} did not publish {prefix}",
                self.child.id()
            );
            match self
                .output
                .recv_timeout(remaining.min(Duration::from_millis(100)))
            {
                Ok(line) => {
                    if line.contains("MIGRATION_METRICS ") {
                        println!(
                            "fixture PID {}: consumed-edge metrics received",
                            self.child.id()
                        );
                    } else {
                        println!("fixture PID {}: {line}", self.child.id());
                    }
                    // libtest may prefix the first user line with its test name.
                    if let Some(offset) = line.find(prefix) {
                        return Ok(line[offset..].to_owned());
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    ensure!(
                        self.child.try_wait()?.is_none(),
                        "owned process exited before {prefix}"
                    );
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("owned process closed stdout before {prefix}")
                }
            }
        }
    }

    fn wait(&mut self, deadline: Instant) -> Result<ExitStatus> {
        loop {
            for line in self.output.try_iter() {
                println!("fixture PID {}: {line}", self.child.id());
            }
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            ensure!(
                Instant::now() < deadline,
                "owned PID {} did not exit before its deadline",
                self.child.id()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct OwnedTarget {
    process: OwnedProcess,
    selected: SelectedTarget,
}

fn token() -> u32 {
    static NEXT: AtomicUsize = AtomicUsize::new(1);
    let ticks = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u32;
    (ticks ^ std::process::id().rotate_left(11) ^ NEXT.fetch_add(1, Ordering::Relaxed) as u32)
        .max(1)
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

// An explicit counterpart fixture allows an x64 controller to exercise x86
// targets. It never selects an existing process; the supervisor still owns the
// child and authenticates its image, creation, parent, HWND and fresh challenge.
fn target_executable() -> Result<PathBuf> {
    let Some(path) = std::env::var_os(TARGET_FIXTURE) else {
        return Ok(std::env::current_exe()?.canonicalize()?);
    };
    ensure!(
        cfg!(target_arch = "x86_64"),
        "cross-width fixture requires an x64 controller"
    );
    let path = PathBuf::from(path).canonicalize()?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    ensure!(
        filename.starts_with("rust_control_native-") && filename.ends_with(".exe"),
        "cross-width target must be the compiled migration fixture"
    );
    let bytes = std::fs::read(&path)?;
    ensure!(bytes.get(..2) == Some(b"MZ"), "fixture lacks DOS header");
    let offset = bytes.get(0x3c..0x40).context("fixture lacks PE offset")?;
    let offset = u32::from_le_bytes(offset.try_into()?) as usize;
    let header = bytes
        .get(offset..)
        .context("fixture PE offset out of bounds")?;
    ensure!(
        header.get(..4) == Some(b"PE\0\0"),
        "fixture lacks PE header"
    );
    ensure!(
        header.get(4..6) == Some(&[0x4c, 0x01]),
        "cross-width fixture must be an actual x86 executable"
    );
    Ok(path)
}

fn spawn_target() -> Result<OwnedTarget> {
    spawn_target_with_inventory(false)
}

fn spawn_target_with_inventory(listed: bool) -> Result<OwnedTarget> {
    spawn_target_configured(listed, false)
}

fn spawn_target_configured(listed: bool, pause_capable: bool) -> Result<OwnedTarget> {
    spawn_target_features(listed, pause_capable, false)
}

fn spawn_target_features(listed: bool, pause_capable: bool, surface: bool) -> Result<OwnedTarget> {
    let expected = token();
    let mut process = OwnedProcess::spawn("target", |command| {
        command
            .env(TOKEN, expected.to_string())
            .env(LISTED_FIXTURE, if listed { "1" } else { "0" })
            .env(
                cleanup_failure::PAUSE_CAPABLE,
                if pause_capable { "1" } else { "0" },
            )
            .env(
                input_surface::SURFACE_FIXTURE,
                if surface { "1" } else { "0" },
            );
    })?;
    let line = process.line("MIGRATION_TARGET ", Instant::now() + POLL_LIMIT)?;
    let fields = line.split_whitespace().collect::<Vec<_>>();
    ensure!(fields.len() == 4, "malformed owned target greeting");
    let pid = fields[1].parse::<u32>()?;
    let hwnd = fields[2].parse::<usize>()?;
    ensure!(
        pid == process.child.id() && fields[3].parse::<u32>()? == expected,
        "owned target identity mismatch"
    );
    let eligibility = targets::eligibility(pid);
    ensure!(
        eligibility.state == EligibilityState::Fresh,
        "fresh fixture: {}",
        eligibility.reason
    );
    let selected = SelectedTarget {
        pid,
        hwnd,
        token: expected,
        creation: eligibility.creation.context("fixture creation")?,
    };
    validate_owned_target(&selected, std::process::id())?;
    Ok(OwnedTarget { process, selected })
}

fn query(target: &SelectedTarget, field: usize) -> Result<usize> {
    let mut value = 0;
    let result = unsafe {
        SendMessageTimeoutW(
            target.hwnd as HWND,
            QUERY,
            field,
            0,
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            QUERY_TIMEOUT_MS,
            &mut value,
        )
    };
    ensure!(
        result != 0,
        "bounded fixture query {field} failed for PID {}: {}",
        target.pid,
        std::io::Error::last_os_error()
    );
    Ok(value)
}

fn reset(target: &SelectedTarget) -> Result<()> {
    let mut value = 0;
    ensure!(
        unsafe {
            SendMessageTimeoutW(
                target.hwnd as HWND,
                RESET,
                0,
                0,
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                QUERY_TIMEOUT_MS,
                &mut value,
            )
        } != 0,
        "reset fixture counters"
    );
    Ok(())
}

fn wait_for(label: &str, mut ready: impl FnMut() -> Result<bool>) -> Result<()> {
    let deadline = Instant::now() + POLL_LIMIT;
    loop {
        if ready()? {
            return Ok(());
        }
        ensure!(Instant::now() < deadline, "timed out waiting for {label}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn evidence(target: &SelectedTarget) -> Result<MessageEvidence> {
    Ok(MessageEvidence {
        buttons_down: query(target, 1)?,
        buttons_up: query(target, 2)?,
        keys_down: query(target, 3)?,
        keys_up: query(target, 4)?,
        authored_text: query(target, 5)?,
    })
}

fn expected_evidence() -> MessageEvidence {
    MessageEvidence {
        buttons_down: 1,
        buttons_up: 1,
        keys_down: 1,
        keys_up: 1,
        authored_text: 1,
    }
}

fn validate_owned_target(target: &SelectedTarget, supervisor: u32) -> Result<()> {
    let mut pid = 0;
    ensure!(
        unsafe { GetWindowThreadProcessId(target.hwnd as HWND, &mut pid) } != 0
            && pid == target.pid,
        "selected HWND does not belong to its owned fixture PID"
    );
    ensure!(
        query(target, CHALLENGE)? == target.token as usize,
        "owned fixture challenge failed"
    );
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, target.pid) };
    ensure!(!process.is_null(), "open owned fixture image");
    let mut image = vec![0u16; 32768];
    let mut length = image.len() as u32;
    let found = unsafe { QueryFullProcessImageNameW(process, 0, image.as_mut_ptr(), &mut length) };
    unsafe {
        CloseHandle(process);
    }
    ensure!(found != 0, "query owned fixture image");
    let image = PathBuf::from(std::ffi::OsString::from_wide(&image[..length as usize]));
    ensure!(
        image.canonicalize()? == target_executable()?,
        "refusing to inject an executable other than the owned migration fixture"
    );
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    ensure!(
        snapshot != windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE,
        "snapshot owned fixture parent"
    );
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    let mut next = unsafe { Process32FirstW(snapshot, &mut entry) };
    let mut parent = None;
    while next != 0 {
        if entry.th32ProcessID == target.pid {
            parent = Some(entry.th32ParentProcessID);
            break;
        }
        next = unsafe { Process32NextW(snapshot, &mut entry) };
    }
    unsafe {
        CloseHandle(snapshot);
    }
    ensure!(
        parent == Some(supervisor),
        "fixture was not spawned by this supervisor"
    );
    let eligibility = targets::eligibility(target.pid);
    ensure!(
        eligibility.creation == Some(target.creation),
        "original fixture process identity changed"
    );
    Ok(())
}

fn assert_no_native_loader() -> Result<()> {
    for name in ["ProtoInputLoader32.dll", "ProtoInputLoader64.dll"] {
        ensure!(
            unsafe { GetModuleHandleW(wide(name).as_ptr()) }.is_null(),
            "Rust controller loaded {name}"
        );
    }
    Ok(())
}

fn controller() -> Result<()> {
    println!("MIGRATION_CONTROLLER_READY");
    std::io::stdout().flush()?;
    let mut start = String::new();
    std::io::stdin().read_line(&mut start)?;
    ensure!(
        start.trim() == "START",
        "controller was not released by its owning Job supervisor"
    );
    let mode = std::env::var(MODE)?;
    ensure!(
        matches!(mode.as_str(), "rust" | "native"),
        "invalid backend mode"
    );
    let targets: Vec<SelectedTarget> = serde_json::from_str(&std::env::var(SELECTED)?)?;
    ensure!(
        (1..=2).contains(&targets.len()),
        "pilot supports one or two owned fixtures"
    );
    let supervisor = std::env::var(SUPERVISOR)?.parse()?;
    for target in &targets {
        validate_owned_target(target, supervisor)?;
    }
    let runtime =
        PathBuf::from(std::env::var_os(RUNTIME).context("native runtime opt-in missing")?)
            .canonicalize()?;
    if mode == "rust" {
        assert_no_native_loader()?;
    }
    let kind = std::env::var(PILOT_KIND).unwrap_or_else(|_| "compatibility".into());
    if matches!(kind.as_str(), "scope-reject" | "legacy-scope-reject") {
        ensure!(mode == "rust", "scope rejection requires Rust control");
    }
    if kind == "legacy-scope-reject" {
        return scope_rejection::legacy_controller(&runtime, &targets);
    }
    let backend = if mode == "rust" {
        Backend::load_rust_virtual(&runtime, Arc::new(AtomicBool::new(false)))?
    } else {
        Backend::load(&runtime)?
    };
    if mode == "rust" {
        assert_no_native_loader()?;
    }
    let seats = targets
        .iter()
        .enumerate()
        .map(|(index, target)| -> Result<_> {
            Ok(ResolvedSeat {
                name: format!("Owned migration fixture {}", index + 1),
                mouse: emulated::device(DeviceKind::Mouse, index as u32 + 1)?.handle as u32,
                keyboard: Some(
                    emulated::device(DeviceKind::Keyboard, index as u32 + 1)?.handle as u32,
                ),
                target_pid: target.pid,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if kind == "scope-reject" {
        return scope_rejection::physical_controller(backend, &targets);
    }
    if kind == "build-reject" {
        return incompatible::controller(backend, &seats, &targets, &mode);
    }
    if kind == "providers" {
        provider_pilot::controller(backend, &seats, &targets)?;
        println!(
            "MIGRATION_STOPPED {mode} {} {}",
            targets.len(),
            std::env::var(TOKEN)?
        );
        std::io::stdout().flush()?;
        return Ok(());
    }
    ensure!(
        matches!(
            kind.as_str(),
            "compatibility" | "performance" | "hold" | "reject" | "fence-failure" | "input-surface"
        ),
        "invalid pilot kind"
    );
    if kind == "reject" {
        ensure!(targets.len() == 1, "rejection fixture requires one target");
        let before = targets::eligibility(targets[0].pid);
        ensure!(
            matches!(
                before.state,
                EligibilityState::Busy | EligibilityState::Dirty
            ),
            "rejection target was startable"
        );
        let error = backend
            .attach(&seats)
            .err()
            .context("competing or dirty target unexpectedly attached")?;
        ensure!(
            targets::eligibility(targets[0].pid).state == before.state,
            "rejected attachment changed target eligibility"
        );
        println!("MIGRATION_REJECTED {mode} {}: {error:#}", targets[0].pid);
        return Ok(());
    }
    let mut session = backend.attach(&seats)?;
    if mode == "rust" {
        assert_no_native_loader()?;
    }
    for (index, target) in targets.iter().enumerate() {
        session.desktop(index, target.hwnd, true)?;
        reset(target)?;
    }
    if kind == "hold" {
        println!("MIGRATION_ATTACHED");
        std::io::stdout().flush()?;
        let mut proceed = String::new();
        std::io::stdin().read_line(&mut proceed)?;
        ensure!(
            proceed.trim() == "CONTINUE",
            "held controller did not receive continuation"
        );
    }
    if kind == "input-surface" {
        input_surface::exercise(&session, &seats, &targets, &mode)?;
    } else {
        for (index, target) in targets.iter().enumerate() {
            let seat = &seats[index];
            let keyboard = seat.keyboard.context("fixture keyboard")?;
            let rect = targets::client_bounds(target.hwnd, target.pid)?;
            let client = (17 + index as i32 * 7, 21 + index as i32 * 5);
            session.position(
                index,
                target.hwnd,
                seat.mouse,
                rect.left + client.0,
                rect.top + client.1,
                rect,
            )?;
            wait_for("motion and hooked cursor API", || {
                Ok(query(target, 0)? > 0
                    && query(target, CURSOR_X)? as isize == client.0 as isize
                    && query(target, CURSOR_Y)? as isize == client.1 as isize)
            })?;
            session.submit_to(index, target.hwnd, Packet::button(seat.mouse, 0, false))?;
            wait_for("button down", || Ok(query(target, 1)? == 1))?;
            session.submit_to(index, target.hwnd, Packet::key(keyboard, 0x41, false))?;
            wait_for("key down and hooked key-state APIs", || {
                Ok(query(target, 3)? == 1
                    && query(target, KEY_STATE)? == 1
                    && query(target, ASYNC_STATE)? == 1)
            })?;
            session.submit_to(index, target.hwnd, Packet::text_unit(keyboard, 0x03A9))?;
            wait_for("authored Unicode text", || Ok(query(target, 5)? == 1))?;
            session.submit_to(index, target.hwnd, Packet::button(seat.mouse, 0, true))?;
            session.submit_to(index, target.hwnd, Packet::key(keyboard, 0x41, true))?;
            wait_for("button/key release and cleared key-state APIs", || {
                Ok(evidence(target)? == expected_evidence()
                    && query(target, KEY_STATE)? == 0
                    && query(target, ASYNC_STATE)? == 0)
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
                    "input leaked into another explicitly selected fixture"
                );
            }
            println!(
                "MIGRATION_INPUT_VERIFIED {mode} PID {} cursor {},{} buttons 1/1 keys 1/1 text Ω APIs verified",
                target.pid, client.0, client.1
            );
        }
    }
    if kind == "fence-failure" {
        return cleanup_failure::controller(session, &targets, &mode);
    }
    let input_ns = if kind == "performance" {
        performance::measure_input(&session, &seats, &targets)?
    } else {
        Vec::new()
    };
    let resources = if kind == "performance" {
        Some(performance::resources()?)
    } else {
        None
    };
    let shutdown_started = Instant::now();
    let report = session.shutdown()?;
    let backend_shutdown_ns = shutdown_started.elapsed().as_nanos().try_into()?;
    ensure!(
        report.acknowledged == targets.len() && report.targets_exited == 0,
        "cleanup did not acknowledge every original target GUI queue: {report:?}"
    );
    drop(session);
    if mode == "rust" {
        assert_no_native_loader()?;
    }
    if kind == "performance" {
        println!(
            "MIGRATION_METRICS {}",
            serde_json::to_string(&PerformanceMetrics {
                input_ns,
                backend_shutdown_ns,
                resources: resources.context("missing live routing resource sample")?,
            })?
        );
    }
    println!(
        "MIGRATION_STOPPED {mode} {} {}",
        targets.len(),
        std::env::var(TOKEN)?
    );
    std::io::stdout().flush()?;
    Ok(())
}

fn run_cycle(
    runtime: &Path,
    mode: &str,
    fixtures: &mut [OwnedTarget],
    kind: &str,
) -> Result<CycleEvidence> {
    let running = start_controller(runtime, mode, fixtures, kind)?;
    finish_cycle(running, mode, fixtures, kind)
}

struct RunningController {
    process: OwnedProcess,
    job: Job,
    commit: CommitOwner,
    token: u32,
}

fn start_controller(
    runtime: &Path,
    mode: &str,
    fixtures: &[OwnedTarget],
    kind: &str,
) -> Result<RunningController> {
    let commit = CommitOwner::create()?;
    let expected = token();
    let selected = fixtures
        .iter()
        .map(|fixture| fixture.selected.clone())
        .collect::<Vec<_>>();
    let mut controller = OwnedProcess::spawn("controller", |command| {
        command
            .env(WORKER_ENV, commit.name())
            .env(RUNTIME, runtime)
            .env(MODE, mode)
            .env(PILOT_KIND, kind)
            .env(SELECTED, serde_json::to_string(&selected).unwrap())
            .env(TOKEN, expected.to_string())
            .env(SUPERVISOR, std::process::id().to_string());
        if kind == "legacy-scope-reject" {
            command.env_remove(WORKER_ENV);
        }
    })?;
    let job = Job::own(&controller.child)?;
    let deadline = Instant::now() + PROCESS_LIMIT;
    controller.line("MIGRATION_CONTROLLER_READY", deadline)?;
    writeln!(
        controller
            .child
            .stdin
            .as_mut()
            .context("controller start pipe")?,
        "START"
    )?;
    Ok(RunningController {
        process: controller,
        job,
        commit,
        token: expected,
    })
}

fn finish_cycle(
    running: RunningController,
    mode: &str,
    fixtures: &mut [OwnedTarget],
    kind: &str,
) -> Result<CycleEvidence> {
    let RunningController {
        process: mut controller,
        job,
        commit,
        token: expected,
    } = running;
    let deadline = Instant::now() + PROCESS_LIMIT;
    let metrics = if kind == "performance" {
        let line = controller.line("MIGRATION_METRICS ", deadline)?;
        Some(serde_json::from_str(
            line.trim_start_matches("MIGRATION_METRICS "),
        )?)
    } else {
        None
    };
    let stopped = controller.line("MIGRATION_STOPPED ", deadline)?;
    ensure!(
        stopped == format!("MIGRATION_STOPPED {mode} {} {expected}", fixtures.len()),
        "wrong controller stop evidence"
    );
    ensure!(
        controller.wait(deadline)?.success(),
        "controller failed after reporting stopped"
    );
    drop(job);
    for fixture in fixtures.iter_mut() {
        ensure!(
            fixture.process.child.try_wait()?.is_none(),
            "original target exited with its controller"
        );
        ensure!(
            targets::eligibility(fixture.selected.pid).state != EligibilityState::Reusable,
            "fixture became reusable before the supervisor committed"
        );
    }
    // Commit only the all-target stopped transaction whose controller exited successfully.
    commit.commit()?;
    let mut observations = Vec::new();
    for fixture in fixtures.iter_mut() {
        wait_for("committed reusable readiness", || {
            let readiness = targets::eligibility(fixture.selected.pid);
            ensure!(
                readiness.creation == Some(fixture.selected.creation),
                "original target identity changed"
            );
            Ok(readiness.state == EligibilityState::Reusable)
        })?;
        validate_owned_target(&fixture.selected, std::process::id())?;
        let observed = evidence(&fixture.selected)?;
        ensure!(
            observed == expected_evidence(),
            "wrong real target messages: {observed:?}"
        );
        observations.push(observed);
    }
    Ok(CycleEvidence {
        messages: observations,
        metrics,
    })
}

fn reject_controller(runtime: &Path, fixtures: &[OwnedTarget]) -> Result<()> {
    let mut rejected = start_controller(runtime, "rust", fixtures, "reject")?;
    rejected
        .process
        .line("MIGRATION_REJECTED rust ", Instant::now() + PROCESS_LIMIT)?;
    ensure!(
        rejected
            .process
            .wait(Instant::now() + POLL_LIMIT)?
            .success(),
        "rejection controller failed"
    );
    // No stopped evidence: its pending transaction is deliberately never committed.
    Ok(())
}

#[test]
#[ignore = "opt-in competing-controller and abrupt-controller-exit fault tests on owned fixtures"]
fn rust_virtual_native_ownership_failures() -> Result<()> {
    let runtime = PathBuf::from(std::env::var_os(RUNTIME).context("native runtime opt-in")?)
        .canonicalize()?;
    for mode in ["rust", "native"] {
        let mut fixtures = vec![spawn_target()?];
        let mut owner = start_controller(&runtime, mode, &fixtures, "hold")?;
        owner
            .process
            .line("MIGRATION_ATTACHED", Instant::now() + PROCESS_LIMIT)?;
        ensure!(
            targets::eligibility(fixtures[0].selected.pid).state == EligibilityState::Busy,
            "owner did not retain its lease"
        );
        reject_controller(&runtime, &fixtures)?;
        writeln!(
            owner.process.child.stdin.as_mut().context("owner stdin")?,
            "CONTINUE"
        )?;
        finish_cycle(owner, mode, &mut fixtures, "hold")?;
        // A rejected competitor must not damage the owner's normal input/reuse.
        run_cycle(&runtime, mode, &mut fixtures, "compatibility")?;

        let mut crashed = start_controller(&runtime, mode, &fixtures, "hold")?;
        crashed
            .process
            .line("MIGRATION_ATTACHED", Instant::now() + PROCESS_LIMIT)?;
        // TerminateProcess through the owned Child gives a deliberate failure
        // code. Kill-on-Job-close may report exit zero on Windows, so exit status
        // alone must never substitute for the stopped nonce and commitment.
        crashed.process.child.kill()?;
        ensure!(
            !crashed.process.wait(Instant::now() + POLL_LIMIT)?.success(),
            "terminated controller exited successfully"
        );
        drop(crashed.job); // Contain any descendants; no Rust Drop cleanup ran.
        wait_for("dirty original target after controller death", || {
            ensure!(
                fixtures[0].process.child.try_wait()?.is_none(),
                "original target died with controller"
            );
            let ready = targets::eligibility(fixtures[0].selected.pid);
            ensure!(
                ready.creation == Some(fixtures[0].selected.creation),
                "target creation changed"
            );
            Ok(ready.state == EligibilityState::Dirty)
        })?;
        reject_controller(&runtime, &fixtures)?;
        ensure!(
            targets::eligibility(fixtures[0].selected.pid).state == EligibilityState::Dirty,
            "failed controller granted target reuse"
        );
        println!(
            "MIGRATION_OWNERSHIP_FAILURES mode={mode} competitor rejected; owner survived/reused; actual controller death quarantined original target; restart rejected"
        );
        // Neither the failed owner's transaction nor the rejected transaction is committed.
    }
    Ok(())
}

#[test]
#[ignore = "opt-in native runtime injection into newly spawned owned test fixtures only"]
fn rust_virtual_native_compatibility_pilot() -> Result<()> {
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
                let observed = run_cycle(&runtime, mode, &mut fixtures, "compatibility")?.messages;
                if let Some(expected) = &reference {
                    ensure!(
                        &observed == expected,
                        "native/Rust message comparison differed"
                    );
                } else {
                    reference = Some(observed);
                }
                println!(
                    "MIGRATION_CLEAN_CYCLE mode={mode} targets={count} cycle={cycle} committed reuse verified"
                );
            }
        }
    }
    println!(
        "MIGRATION_COMPATIBILITY_PILOT_PASSED controller={}bit target={}bit; cursor/keyboard-state providers and failure scenarios remain separate gates",
        usize::BITS,
        if std::env::var_os(TARGET_FIXTURE).is_some() {
            32
        } else {
            usize::BITS
        }
    );
    Ok(())
}

#[test]
#[ignore = "internal subprocess entry point; requires an owning pilot supervisor"]
fn migration_process_child() -> Result<()> {
    match std::env::var(ROLE).as_deref() {
        Ok("target") => target(),
        Ok("controller") => controller(),
        _ => bail!("fixture child requires its owning migration supervisor"),
    }
}

fn target() -> Result<()> {
    FIXTURE_TOKEN.store(
        std::env::var(TOKEN)?.parse::<u32>()? as usize,
        Ordering::Relaxed,
    );
    let pause_capable = std::env::var(cleanup_failure::PAUSE_CAPABLE).as_deref() == Ok("1");
    if pause_capable {
        cleanup_failure::prepare_target()?;
    }
    if std::env::var(input_surface::SURFACE_FIXTURE).as_deref() == Ok("1") {
        input_surface::prepare_target();
    }
    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            QUERY => match wparam {
                0..=7 => COUNTERS[wparam].load(Ordering::Relaxed) as isize,
                CURSOR_X | CURSOR_Y => {
                    let mut point = POINT { x: 0, y: 0 };
                    if unsafe { GetCursorPos(&mut point) } == 0
                        || unsafe { ScreenToClient(hwnd, &mut point) } == 0
                    {
                        return isize::MIN;
                    }
                    if wparam == CURSOR_X {
                        point.x as isize
                    } else {
                        point.y as isize
                    }
                }
                KEY_STATE => isize::from(unsafe { GetKeyState(0x41) } < 0),
                ASYNC_STATE => isize::from(unsafe { GetAsyncKeyState(0x41) } < 0),
                CHALLENGE => FIXTURE_TOKEN.load(Ordering::Relaxed) as isize,
                30 => {
                    let mut handles = 0;
                    if unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut handles) } == 0 {
                        return -1;
                    }
                    handles as isize
                }
                31..=34 => RECIPIENTS[wparam - 31].load(Ordering::Relaxed) as isize,
                35 => PUMP_WAKE_TOKEN.load(Ordering::Relaxed) as isize,
                36 => unsafe { GetForegroundWindow() as isize },
                cleanup_failure::PAUSE_QUERY => cleanup_failure::request_pause(hwnd, lparam),
                input_surface::QUERY_BASE..=input_surface::QUERY_END => {
                    input_surface::query(wparam)
                }
                provider_pilot::SECONDARY_HWND => SECONDARY.load(Ordering::Relaxed) as isize,
                provider_pilot::TABLE_A | provider_pilot::CAPS_TABLE => {
                    let mut keys = [0u8; 256];
                    if unsafe { GetKeyboardState(keys.as_mut_ptr()) } == 0 {
                        return -1;
                    }
                    let key = if wparam == provider_pilot::TABLE_A {
                        0x41
                    } else {
                        0x14
                    };
                    keys[key] as isize
                }
                provider_pilot::SHIFT_WORD
                | provider_pilot::LSHIFT_WORD
                | provider_pilot::RSHIFT_WORD
                | provider_pilot::CAPS_WORD => {
                    let key = match wparam {
                        provider_pilot::SHIFT_WORD => 0x10,
                        provider_pilot::LSHIFT_WORD => 0xa0,
                        provider_pilot::RSHIFT_WORD => 0xa1,
                        _ => 0x14,
                    };
                    unsafe { GetKeyState(key) as u16 as isize }
                }
                _ => 0,
            },
            RESET => {
                input_surface::reset();
                for counter in &COUNTERS {
                    counter.store(0, Ordering::Relaxed);
                }
                for recipient in &RECIPIENTS {
                    recipient.store(0, Ordering::Relaxed);
                }
                PUMP_WAKE_TOKEN.store(0, Ordering::Relaxed);
                1
            }
            PUMP_WAKE => {
                PUMP_WAKE_TOKEN.store(wparam, Ordering::Relaxed);
                0
            }
            WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_KEYDOWN | WM_KEYUP | WM_CHAR => {
                if matches!(message, WM_KEYDOWN | WM_KEYUP) && wparam == 0x27 {
                    input_surface::observe(hwnd, message, wparam, lparam);
                }
                let counter = match message {
                    WM_MOUSEMOVE => Some(0),
                    WM_LBUTTONDOWN => Some(1),
                    WM_LBUTTONUP => Some(2),
                    WM_KEYDOWN if wparam == 0x41 => Some(3),
                    WM_KEYUP if wparam == 0x41 => Some(4),
                    WM_KEYDOWN if wparam == 0x42 => Some(6),
                    WM_KEYUP if wparam == 0x42 => Some(7),
                    WM_CHAR if wparam == 0x03A9 => Some(5),
                    _ => None,
                };
                if let Some(index) = counter {
                    COUNTERS[index].fetch_add(1, Ordering::Relaxed);
                    if (1..=4).contains(&index) {
                        RECIPIENTS[index - 1].store(hwnd as usize, Ordering::Relaxed);
                    }
                }
                0
            }
            windows_sys::Win32::UI::WindowsAndMessaging::WM_RBUTTONDOWN
            | windows_sys::Win32::UI::WindowsAndMessaging::WM_RBUTTONUP
            | windows_sys::Win32::UI::WindowsAndMessaging::WM_MBUTTONDOWN
            | windows_sys::Win32::UI::WindowsAndMessaging::WM_MBUTTONUP
            | windows_sys::Win32::UI::WindowsAndMessaging::WM_XBUTTONDOWN
            | windows_sys::Win32::UI::WindowsAndMessaging::WM_XBUTTONUP
            | windows_sys::Win32::UI::WindowsAndMessaging::WM_MOUSEWHEEL
            | windows_sys::Win32::UI::WindowsAndMessaging::WM_MOUSEHWHEEL => {
                input_surface::observe(hwnd, message, wparam, lparam);
                0
            }
            WM_DESTROY => {
                unsafe {
                    PostQuitMessage(0);
                }
                0
            }
            WM_SETCURSOR => {
                let cursor = if hwnd as usize == SECONDARY.load(Ordering::Relaxed) {
                    32515
                } else {
                    32512
                };
                unsafe {
                    SetCursor(LoadCursorW(ptr::null_mut(), cursor as *const u16));
                }
                1
            }
            _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        }
    }
    let name = wide("ProtoInputMultiPlexRustControlMigrationFixture");
    unsafe {
        let instance = GetModuleHandleW(ptr::null());
        ensure!(!instance.is_null(), "fixture module instance");
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: name.as_ptr(),
            ..std::mem::zeroed()
        };
        ensure!(RegisterClassW(&class) != 0, "register fixture window");
        // Offscreen and NOACTIVATE: the pilot never moves the user's desktop pointer.
        // The production worker selects from the real application inventory,
        // which intentionally excludes tool windows. Its explicit fixture uses
        // the same offscreen/no-activate placement with a listed primary window.
        let primary_style = WS_EX_NOACTIVATE
            | if std::env::var(LISTED_FIXTURE).as_deref() == Ok("1") {
                0
            } else {
                WS_EX_TOOLWINDOW
            };
        let hwnd = CreateWindowExW(
            primary_style,
            name.as_ptr(),
            name.as_ptr(),
            WS_POPUP | WS_VISIBLE,
            -32000,
            -32000,
            320,
            240,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            ptr::null(),
        );
        ensure!(!hwnd.is_null(), "create fixture window");
        let secondary = CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            name.as_ptr(),
            name.as_ptr(),
            WS_POPUP | WS_VISIBLE,
            -32000,
            -32000,
            320,
            240,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            ptr::null(),
        );
        ensure!(!secondary.is_null(), "create secondary fixture window");
        SECONDARY.store(secondary as usize, Ordering::Relaxed);
        ShowWindow(secondary, SW_SHOWNOACTIVATE);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        println!(
            "MIGRATION_TARGET {} {} {}",
            std::process::id(),
            hwnd as usize,
            FIXTURE_TOKEN.load(Ordering::Relaxed)
        );
        std::io::stdout().flush()?;
        if pause_capable {
            cleanup_failure::target_pump()?;
        } else {
            let mut message: MSG = std::mem::zeroed();
            loop {
                // No HWND or message filters: the resident hook must observe the real queue fence.
                let result = GetMessageW(&mut message, ptr::null_mut(), 0, 0);
                ensure!(result != -1, "fixture GetMessage failed");
                if result == 0 {
                    break;
                }
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
    Ok(())
}
