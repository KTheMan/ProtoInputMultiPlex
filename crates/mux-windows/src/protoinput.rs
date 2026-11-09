//! Bindings audited against the pinned vendor/ProtoInput protoloader.h.
//! Exports use the C ABI (cdecl), Windows unsigned long is u32, C++ bool is bool.
pub(crate) mod control;
mod control_pipe;
mod dispatch;
mod injector;
pub(crate) mod wire;
pub use crate::queue_fence::ShutdownReport;
use crate::queue_fence::{self, FenceRequest, QueueFence, Recipient};
use anyhow::{Context, Result, ensure};
use libloading::os::windows::{
    LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, Library,
};
use mux_core::{
    ResolvedSeat,
    desktop::{Bounds, MotionCoalescingSettings},
    emulated::{self, Packet},
    keyboard::ApplicationFocus,
    motion_coalescing::{MotionKey, MotionPayload, PendingMotion},
};
use std::{
    cell::Cell,
    cell::RefCell,
    collections::HashSet,
    error::Error as StdError,
    fmt,
    marker::PhantomData,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex, MutexGuard, atomic::AtomicBool},
    time::Instant,
};
use windows_sys::Win32::{
    Foundation::CloseHandle,
    System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
};

type Inject = unsafe extern "C" fn(u32, *const u16) -> u32;
type Pair = unsafe extern "C" fn(u32, u32);
type Toggle = unsafe extern "C" fn(u32, bool);
type Setup = unsafe extern "C" fn(u32, i32);
type Messages = unsafe extern "C" fn(u32, bool, bool, bool, bool);
type Focus = unsafe extern "C" fn(u32, i32, bool, bool, bool, bool, bool);
type Single = unsafe extern "C" fn(u32);
type Submit = unsafe extern "C" fn(u32, *const Packet);
type InputVersion = unsafe extern "C" fn() -> u32;
type QueueFenceExport = QueueFence;
type Window = unsafe extern "C" fn(u32, u64);
type BoundsToggle = unsafe extern "C" fn(u32, bool, bool);

fn qualification_first_key_down(packet: &Packet, first: &AtomicBool) -> bool {
    if packet.kind != 1 || packet.flags & 1 != 0 {
        return false;
    }
    qualification_claim_key_down(
        packet,
        first,
        matches!(
            std::env::var("PROTOINPUTMULTIPLEX_QUALIFICATION_STDERR").as_deref(),
            Ok("private-file-v1")
        ),
    )
}

fn qualification_claim_key_down(packet: &Packet, first: &AtomicBool, enabled: bool) -> bool {
    packet.kind == 1
        && packet.flags & 1 == 0
        && enabled
        && !first.swap(true, std::sync::atomic::Ordering::Relaxed)
}

fn qualification_stderr(line: std::fmt::Arguments<'_>) {
    use std::io::Write;
    // Qualification directs stderr to a private regular file. Do not propagate
    // a diagnostic write error into the original input/cleanup behavior.
    let _ = writeln!(std::io::stderr().lock(), "{line}");
}

/// A motion publication failed for one assigned virtual device. The failed queued
/// segment is discarded so callers never replay stale motion to a later target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MotionPublicationFailure {
    pub device: u32,
    pub detail: String,
}

/// One or more per-device motion publications failed while draining a queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MotionPublicationError {
    pub failures: Vec<MotionPublicationFailure>,
    cancelled_before_transfer: bool,
}

impl fmt::Display for MotionPublicationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "motion publication failed for {} device(s)",
            self.failures.len()
        )?;
        for failure in &self.failures {
            write!(f, "; device {}: {}", failure.device, failure.detail)?;
        }
        Ok(())
    }
}

impl StdError for MotionPublicationError {}

impl MotionPublicationError {
    fn one(device: u32, error: anyhow::Error) -> anyhow::Error {
        let cancelled_before_transfer = is_cancelled_before_transfer(&error);
        anyhow::Error::new(Self {
            failures: vec![MotionPublicationFailure {
                device,
                detail: format!("{error:#}"),
            }],
            cancelled_before_transfer,
        })
    }

    fn append(&mut self, mut other: Self) {
        self.cancelled_before_transfer &= other.cancelled_before_transfer;
        self.failures.append(&mut other.failures);
    }
}

/// True only for an explicit cancelled-before-write disposition, including a
/// motion batch consisting entirely of such rejections. No string matching and
/// no inference from a transport receipt or successful target operation.
pub fn is_cancelled_before_transfer(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<control_pipe::SendError>()
        .is_some_and(control_pipe::SendError::cancelled_before_transfer)
        || error
            .downcast_ref::<MotionPublicationError>()
            .is_some_and(|error| error.cancelled_before_transfer)
}

#[cfg(test)]
mod cancellation_disposition_tests {
    use super::*;

    fn cancelled() -> anyhow::Error {
        anyhow::Error::new(control_pipe::SendError::NotStarted(anyhow::Error::new(
            control_pipe::Cancelled,
        )))
    }

    #[test]
    fn stop_classification_preserves_typed_transfer_boundary_through_motion_errors() {
        assert!(is_cancelled_before_transfer(
            &cancelled().context("publish motion")
        ));
        let mut batch = MotionPublicationError::one(2, cancelled())
            .downcast::<MotionPublicationError>()
            .unwrap();
        batch.append(
            MotionPublicationError::one(4, cancelled())
                .downcast::<MotionPublicationError>()
                .unwrap(),
        );
        assert!(is_cancelled_before_transfer(&anyhow::Error::new(
            batch.clone()
        )));
        batch.append(
            MotionPublicationError::one(6, anyhow::anyhow!("target acknowledgement timed out"))
                .downcast::<MotionPublicationError>()
                .unwrap(),
        );
        assert!(!is_cancelled_before_transfer(&anyhow::Error::new(batch)));
        assert!(!is_cancelled_before_transfer(&anyhow::Error::new(
            control_pipe::SendError::TransferStarted(anyhow::Error::new(control_pipe::Cancelled))
        )));
        assert!(!is_cancelled_before_transfer(&anyhow::anyhow!(
            "control-pipe operation cancelled before transfer"
        )));
    }
}

fn collect_motion_error(
    current: &mut Option<MotionPublicationError>,
    error: anyhow::Error,
) -> Result<()> {
    match error.downcast::<MotionPublicationError>() {
        Ok(error) => {
            if let Some(current) = current {
                current.append(error);
            } else {
                *current = Some(error);
            }
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn finish_motion_errors(errors: Option<MotionPublicationError>) -> Result<()> {
    errors.map_or(Ok(()), |error| Err(anyhow::Error::new(error)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CleanupStage {
    Quiesce,
    Drain,
    Retire,
}

#[derive(Default)]
struct CleanupErrors(Vec<anyhow::Error>);

impl CleanupErrors {
    fn record(&mut self, result: Result<()>) {
        if let Err(error) = result {
            self.0.push(error);
        }
    }

    fn finish(mut self) -> Result<()> {
        if self.0.is_empty() {
            return Ok(());
        }
        let first = self.0.remove(0);
        if self.0.is_empty() {
            Err(first)
        } else {
            let others = self
                .0
                .iter()
                .map(|error| format!("{error:#}"))
                .collect::<Vec<_>>()
                .join("; ");
            Err(first.context(format!("additional cleanup failures: {others}")))
        }
    }
}

/// Every target gets its minimum stop attempt before any target is drained.
/// A failed prerequisite excludes only that target from later destructive stages.
fn cleanup_targets(
    count: usize,
    mut step: impl FnMut(usize, CleanupStage) -> Result<ShutdownReport>,
) -> Result<ShutdownReport> {
    let mut eligible = vec![true; count];
    let mut errors = CleanupErrors::default();
    let mut report = ShutdownReport::default();
    for stage in [
        CleanupStage::Quiesce,
        CleanupStage::Drain,
        CleanupStage::Retire,
    ] {
        for offset in 0..count {
            let index = if stage == CleanupStage::Retire {
                count - offset - 1
            } else {
                offset
            };
            if !eligible[index] {
                continue;
            }
            match step(index, stage) {
                Ok(part) => {
                    report.acknowledged += part.acknowledged;
                    report.targets_exited += part.targets_exited;
                }
                Err(error) => {
                    eligible[index] = false;
                    errors.record(Err(
                        error.context(format!("target {index} cleanup {stage:?}"))
                    ));
                }
            }
        }
    }
    errors.finish()?;
    Ok(report)
}

fn validate_widened_motion(packet: Packet) -> Result<()> {
    let mut shape = packet;
    shape.x = 0;
    shape.y = 0;
    shape.validate()?;
    ensure!(
        packet.kind == 0 && packet.flags == 0,
        "coalesced publication accepts only relative mouse motion"
    );
    Ok(())
}

fn split_motion(x: i64, y: i64, mut publish: impl FnMut(i32, i32) -> Result<()>) -> Result<()> {
    let (mut dx, mut dy) = (x, y);
    while dx != 0 || dy != 0 {
        let sx = dx.clamp(-32767, 32767) as i32;
        let sy = dy.clamp(-32767, 32767) as i32;
        publish(sx, sy)?;
        dx -= i64::from(sx);
        dy -= i64::from(sy);
    }
    Ok(())
}

// IDs from ProtoHookIDs and ProtoMessageFilterIDs in the pinned header.
const HOOKS: &[u32] = &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
const FILTERS: &[u32] = &[0, 1, 2, 3, 4, 5, 6, 7];
static BACKEND_OWNER: Mutex<()> = Mutex::new(());

struct NativeApi {
    _library: Library,
    inject: Inject,
    setup: Setup,
    install: Pair,
    uninstall: Pair,
    enable_filter: Pair,
    disable_filter: Pair,
    mouse: Pair,
    keyboard: Pair,
    draw: Toggle,
    freeze: Toggle,
    messages: Messages,
    focus: Focus,
    stop_focus: Single,
    submit: Option<Submit>,
    queue_fence: Option<QueueFenceExport>,
    window: Option<Window>,
    bounds: Option<BoundsToggle>,
    cursor_start: Option<crate::app_cursor::Start>,
    cursor_associate: Option<crate::app_cursor::Associate>,
    cursor_stop: Option<crate::app_cursor::Stop>,
    keyboard_state: Option<crate::keyboard_state::Command>,
    block: Option<Pair>,
    unblock: Option<Pair>,
}

pub struct Backend {
    native: Option<NativeApi>,
    rust: Option<Rc<control::Registry>>,
    _lease: MutexGuard<'static, ()>,
    _thread: PhantomData<Rc<()>>,
    folder: Vec<u16>,
    path: PathBuf,
    input_version: u32,
    lifecycle: Option<crate::lifecycle::Api>,
    cursor_arches: HashSet<u32>,
}

impl Backend {
    /// The loader selected for this controller's process architecture.
    pub fn loader_name() -> Result<&'static str> {
        crate::process_arch::loader_name()
    }
    /// Optional loader ABI capability. Per-target readiness still validates its resident build.
    pub fn lifecycle_version(&self) -> u32 {
        u32::from(self.lifecycle.is_some())
    }
    fn worker_lifecycle(&self) -> Option<crate::lifecycle::Api> {
        // Direct CLI attachment retains the legacy fresh-target behavior. Only
        // supervisor children carry an all-target transaction allowing retirement.
        std::env::var_os(crate::lifecycle::WORKER_ENV).and(self.lifecycle.clone())
    }
    pub fn load(folder: &Path) -> Result<Self> {
        let lease = BACKEND_OWNER.try_lock().map_err(|_| {
            anyhow::anyhow!("a ProtoInput backend is already active in this process")
        })?;
        let loader_name = Self::loader_name()?;
        let path = folder
            .canonicalize()
            .with_context(|| format!("runtime folder missing: {}", folder.display()))?;
        let dll = path.join(loader_name);
        // An absolute path and restricted dependency search prevent loading a DLL from the CWD.
        let library = unsafe {
            Library::load_with_flags(
                &dll,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
            )
        }
        .with_context(|| format!("load {} and its dependencies", dll.display()))?;
        // Resolve every required symbol before permitting any process mutation.
        unsafe {
            let mut folder: Vec<u16> = path.as_os_str().encode_wide().collect();
            ensure!(!folder.contains(&0), "runtime path contains NUL");
            folder.extend(['\\' as u16, 0]); // upstream concatenates filenames without a separator
            Ok(Self {
                lifecycle: crate::lifecycle::Api::load(&library),
                input_version: library
                    .get::<InputVersion>(b"MuxInputVersion\0")
                    .map(|f| f())
                    .unwrap_or(1),
                native: Some(NativeApi {
                    inject: *library.get(b"EasyHookInjectRuntime\0")?,
                    setup: *library.get(b"SetupState\0")?,
                    install: *library.get(b"InstallHook\0")?,
                    uninstall: *library.get(b"UninstallHook\0")?,
                    enable_filter: *library.get(b"EnableMessageFilter\0")?,
                    disable_filter: *library.get(b"DisableMessageFilter\0")?,
                    mouse: *library.get(b"AddSelectedMouseHandle\0")?,
                    keyboard: *library.get(b"AddSelectedKeyboardHandle\0")?,
                    draw: *library.get(b"SetDrawFakeCursor\0")?,
                    freeze: *library.get(b"SetExternalFreezeFakeInput\0")?,
                    messages: *library.get(b"SetupMessagesToSend\0")?,
                    focus: *library.get(b"StartFocusMessageLoop\0")?,
                    stop_focus: *library.get(b"StopFocusMessageLoop\0")?,
                    submit: library.get::<Submit>(b"MuxSubmitInput\0").ok().map(|s| *s),
                    queue_fence: library
                        .get::<QueueFenceExport>(b"MuxQueueFence\0")
                        .ok()
                        .map(|symbol| *symbol),
                    window: library
                        .get::<Window>(b"UpdateMainWindowHandle\0")
                        .ok()
                        .map(|s| *s),
                    bounds: library
                        .get::<BoundsToggle>(b"AllowFakeCursorOutOfBounds\0")
                        .ok()
                        .map(|s| *s),
                    block: library
                        .get::<Pair>(b"EnableMessageBlock\0")
                        .ok()
                        .map(|s| *s),
                    unblock: library
                        .get::<Pair>(b"DisableMessageBlock\0")
                        .ok()
                        .map(|s| *s),
                    cursor_start: if library
                        .get::<InputVersion>(b"MuxCursorObserverVersion\0")
                        .is_ok_and(|version| version() == 1)
                    {
                        library
                            .get::<crate::app_cursor::Start>(b"MuxCursorObserverStart\0")
                            .ok()
                            .map(|s| *s)
                    } else {
                        None
                    },
                    cursor_associate: library
                        .get::<crate::app_cursor::Associate>(b"MuxCursorObserverAssociate\0")
                        .ok()
                        .map(|s| *s),
                    cursor_stop: library
                        .get::<crate::app_cursor::Stop>(b"MuxCursorObserverStop\0")
                        .ok()
                        .map(|s| *s),
                    keyboard_state: if library
                        .get::<crate::keyboard_state::Version>(b"MuxKeyboardStateVersion\0")
                        .is_ok_and(|version| version() == crate::keyboard_state::ABI_VERSION)
                    {
                        library
                            .get::<crate::keyboard_state::Command>(b"MuxKeyboardStateCommand\0")
                            .ok()
                            .map(|symbol| *symbol)
                    } else {
                        None
                    },
                    _library: library,
                }),
                rust: None,
                cursor_arches: [32, 64]
                    .into_iter()
                    .filter(|bits| {
                        path.join(format!("protoinputmultiplex_cursor_observer{bits}.dll"))
                            .is_file()
                    })
                    .collect(),
                _lease: lease,
                _thread: PhantomData,
                folder,
                path,
            })
        }
    }

    /// Owns the loader for the full session. Dropping the session requests hook removal.
    pub fn attach(self, seats: &[ResolvedSeat]) -> Result<Session> {
        ensure!(!seats.is_empty(), "profile has no seats");
        self.require_virtual_scope(seats)?;
        if seats
            .iter()
            .any(|s| is_virtual(s.mouse) || s.keyboard.is_some_and(is_virtual))
        {
            ensure!(
                self.input_version >= 3 && self.fence_supported(),
                "virtual input requires ProtoInput backend version 3 with queue-fence support"
            );
        }
        if seats.iter().any(|s| {
            s.mouse & emulated::MASK == emulated::BASE
                || s.keyboard
                    .is_some_and(|k| k & emulated::MASK == emulated::BASE)
        }) {
            ensure!(
                self.submit_supported(),
                "backend lacks emulated input support; rebuild it"
            );
        }
        let mut pids = HashSet::new();
        for seat in seats {
            ensure!(
                seat.target_pid != std::process::id(),
                "cannot attach to ProtoInputMultiPlex itself"
            );
            ensure!(pids.insert(seat.target_pid), "duplicate target process");
            let bits = target_bits(seat.target_pid)?;
            crate::process_arch::ensure_host_target(bits)?;
            if !virtual_seat(seat) || self.worker_lifecycle().is_none() {
                crate::targets::require_fresh_target(seat.target_pid)?;
            }
            for name in [
                format!("ProtoInputIJ{bits}.exe"),
                format!("ProtoInputIJP{bits}.dll"),
                format!("ProtoInputHooks{bits}.dll"),
            ] {
                ensure!(
                    self.path.join(&name).is_file(),
                    "runtime file missing: {name}"
                );
            }
        }
        let process_watches = seats
            .iter()
            .map(|seat| crate::targets::ProcessWatch::open([seat.target_pid]))
            .collect::<Result<Vec<_>>>()?;
        let mut session = Session {
            backend: self,
            instances: Vec::new(),
            lifecycle: Vec::new(),
            seats: seats.to_vec(),
            process_watches,
            native_targets: RefCell::new(vec![NativeTargetState::default(); seats.len()]),
            pending_motion: RefCell::new(vec![None; seats.len()]),
            shutdown_attempted: Cell::new(false),
            requires_queue_fence: seats
                .iter()
                .any(|seat| is_virtual(seat.mouse) || seat.keyboard.is_some_and(is_virtual)),
        };
        for (index, seat) in seats.iter().enumerate() {
            let api = &session.backend;
            {
                let lifecycle = if virtual_seat(seat) {
                    api.worker_lifecycle()
                        .map(|lifecycle| {
                            lifecycle.attach(
                                crate::targets::ProcessIdentity::open_for_attach(seat.target_pid)?,
                                &api.folder,
                            )
                        })
                        .transpose()?
                } else {
                    None
                };
                let instance = match lifecycle.as_ref() {
                    Some(attachment) => attachment.instance,
                    None => api.inject(seat.target_pid)?,
                };
                ensure!(
                    instance != 0,
                    "ProtoInput failed to attach {} (PID {}); prior seats will be stopped",
                    seat.name,
                    seat.target_pid
                );
                session.instances.push(instance);
                let resident = lifecycle.is_some();
                session.lifecycle.push(lifecycle);
                api.freeze(instance, true)?;
                api.setup(instance, index as i32 + 1)?;
                api.mouse(instance, seat.mouse)?;
                // An empty upstream selection accepts no keyboards. Do not add handle 0 as a wildcard.
                if let Some(keyboard) = seat.keyboard {
                    api.keyboard(instance, keyboard)?;
                }
                for &hook in HOOKS {
                    if resident && hook == 0 {
                        continue;
                    }
                    api.install(instance, hook)?;
                }
                for &filter in FILTERS {
                    api.enable_filter(instance, filter)?;
                }
                api.messages(instance, true, true, true, seat.keyboard.is_some())?;
                api.draw(instance, true)?;
                api.focus(instance, 10, true, true, true, true, true)?;
                api.freeze(instance, false)?;
            }
        }
        Ok(session)
    }
}

pub struct Session {
    // Attachments close native instances before their owning DLL is unloaded.
    lifecycle: Vec<Option<crate::lifecycle::Attachment>>,
    backend: Backend,
    instances: Vec<u32>,
    seats: Vec<ResolvedSeat>,
    // Keep the original process objects open through synchronous fence drains;
    // reopening by PID alone could target an unrelated process after PID reuse.
    process_watches: Vec<crate::targets::ProcessWatch>,
    // Native pointer state is per ProtoInput instance. Keyboard delivery may temporarily
    // retarget the instance, so retain the independent pointer window and client position.
    native_targets: RefCell<Vec<NativeTargetState>>,
    pending_motion: RefCell<Vec<Option<PendingMotion>>>,
    shutdown_attempted: Cell<bool>,
    requires_queue_fence: bool,
}
#[derive(Clone, Default)]
struct NativeTargetState {
    selected_window: Option<usize>,
    pointer: Option<NativePointerState>,
    recipients: Vec<Recipient>,
}
#[derive(Clone, Copy)]
struct NativePointerState {
    window: usize,
    mouse: u32,
    client: (i32, i32),
}

fn motion_key(
    user: usize,
    pid: u32,
    hwnd: Option<usize>,
    mouse: u32,
    client: Option<Bounds>,
) -> MotionKey {
    MotionKey {
        user,
        pid,
        hwnd,
        mouse,
        client,
    }
}
impl Session {
    /// Cancel only this seat's unpublished software motion. No native call or
    /// peer queue/deadline is touched; already transferred input is not recalled.
    pub fn discard_user_motion(&self, user: usize) -> Result<usize> {
        self.ensure_open()?;
        let mut pending = self.pending_motion.borrow_mut();
        discard_motion_slot(&mut pending, user)
    }
    /// Cleanup-only UP to its saved recipient, bypassing ordinary publication
    /// barriers. The caller keeps failed releases owned until success/exit.
    pub fn submit_release_to(&self, user: usize, hwnd: usize, packet: Packet) -> Result<()> {
        validate_cleanup_up(packet)?;
        self.ensure_open()?;
        ensure!(self.alive(user), "original target process exited");
        self.submit_to_direct(user, hwnd, packet)
    }
    /// Liveness of the originally attached process, protected against PID reuse.
    pub fn alive(&self, index: usize) -> bool {
        self.process_watches
            .get(index)
            .is_some_and(|watch| watch.exited().is_none())
    }
    fn ensure_open(&self) -> Result<()> {
        ensure!(
            !self.shutdown_attempted.get(),
            "ProtoInput session is shutting down"
        );
        Ok(())
    }

    fn ensure_fence_support(&self) -> Result<()> {
        ensure!(
            self.backend.input_version >= 3 && self.backend.fence_supported(),
            "desktop target selection requires ProtoInput queue-fence support"
        );
        Ok(())
    }

    fn remember_recipient(&self, index: usize, hwnd: usize) -> Result<()> {
        let seat = self.seats.get(index).context("unknown user")?;
        let recipient = Recipient::validate(hwnd, seat.target_pid)?;
        let mut targets = self.native_targets.borrow_mut();
        let target = targets.get_mut(index).context("unknown user")?;
        queue_fence::remember(&mut target.recipients, recipient)
    }

    pub fn desktop(&self, index: usize, hwnd: usize, lock: bool) -> Result<()> {
        self.flush_motion_index(index)?;
        self.ensure_open()?;
        self.ensure_fence_support()?;
        ensure!(
            self.backend.window_supported(),
            "backend lacks window selection"
        );
        let window = |instance, hwnd| self.backend.window(instance, hwnd);
        ensure!(
            self.backend.bounds_supported(),
            "backend lacks cursor bounds"
        );
        let bounds = |instance, allow, extend| self.backend.bounds(instance, allow, extend);
        ensure!(
            self.backend.block_supported(),
            "backend lacks message blocking"
        );
        let block = |instance, message| self.backend.block(instance, message);
        ensure!(
            self.backend.block_supported(),
            "backend lacks message unblocking"
        );
        let instance = *self.instances.get(index).context("unknown user")?;
        // The legacy mode changes ProtoInput's selected window independently of our
        // mouse cache. Force the next explicit position call to home its own stream.
        let mut native_targets = self.native_targets.borrow_mut();
        let target = native_targets.get_mut(index).context("unknown user")?;
        queue_fence::remember(
            &mut target.recipients,
            Recipient::validate(hwnd, self.seats[index].target_pid)?,
        )?;
        {
            // Physical cursor departure must not erase the independent virtual hover state.
            block(instance, 0x02A3)?; // WM_MOUSELEAVE
            window(instance, hwnd as u64)?;
            self.backend.draw(instance, false)?;
            bounds(instance, !lock, !lock)?;
        }
        target.selected_window = Some(hwnd);
        target.pointer = None;
        Ok(())
    }
    /// Position one user's virtual mouse in an explicitly selected target window.
    /// Coordinates and target identity are revalidated immediately before native retargeting.
    pub fn position(
        &self,
        index: usize,
        hwnd: usize,
        mouse: u32,
        x: i32,
        y: i32,
        rect: mux_core::desktop::Bounds,
    ) -> Result<()> {
        self.flush_motion_index(index)?;
        self.position_direct(index, hwnd, mouse, x, y, rect)
    }

    fn position_direct(
        &self,
        index: usize,
        hwnd: usize,
        mouse: u32,
        x: i32,
        y: i32,
        rect: Bounds,
    ) -> Result<()> {
        self.ensure_open()?;
        self.ensure_fence_support()?;
        let instance = *self.instances.get(index).context("unknown user")?;
        let seat = self.seats.get(index).context("unknown user")?;
        ensure!(seat.mouse == mouse, "mouse is not assigned to this user");
        ensure!(rect.valid(), "target client rectangle is invalid");
        let current = crate::targets::client_bounds(hwnd, seat.target_pid)?;
        ensure!(
            current == rect,
            "target client bounds changed; retry positioning"
        );
        self.remember_recipient(index, hwnd)?;
        ensure!(
            rect.contains(x, y),
            "cursor is outside the target client area"
        );
        let client = (x - rect.left, y - rect.top);
        ensure!(
            self.backend.window_supported(),
            "backend lacks window selection"
        );
        let window = |instance, hwnd| self.backend.window(instance, hwnd);
        ensure!(
            self.backend.bounds_supported(),
            "backend lacks cursor bounds"
        );
        let bounds = |instance, allow, extend| self.backend.bounds(instance, allow, extend);
        let mut native_targets = self.native_targets.borrow_mut();
        let target = native_targets.get_mut(index).context("unknown user")?;
        let api = &self.backend;
        let previous = target.pointer;
        {
            window(instance, hwnd as u64)?;
            if previous.is_none_or(|p| p.window != hwnd || p.mouse != mouse) {
                // Home this virtual stream at the target client origin. This follows the
                // already accepted desktop backend sequence without touching OS input.
                bounds(instance, false, false)?;
                api.submit(instance, &Packet::motion(mouse, -32767, -32767))?;
                api.submit(instance, &Packet::motion(mouse, 1, 1))?;
                bounds(instance, true, false)?;
            }
            let old = previous
                .filter(|p| p.window == hwnd && p.mouse == mouse)
                .map_or((0, 0), |p| p.client);
            let mut dx = client.0 - old.0;
            let mut dy = client.1 - old.1;
            // Keep every native relative packet in range and always send a final move,
            // including a zero delta, to establish hover before a button-down.
            loop {
                let sx = dx.clamp(-32767, 32767);
                let sy = dy.clamp(-32767, 32767);
                api.submit(instance, &Packet::motion(mouse, sx, sy))?;
                dx -= sx;
                dy -= sy;
                if dx == 0 && dy == 0 {
                    break;
                }
            }
        }
        target.selected_window = Some(hwnd);
        target.pointer = Some(NativePointerState {
            window: hwnd,
            mouse,
            client,
        });
        Ok(())
    }

    /// Queue one already-transformed absolute cursor position for native publication.
    /// The cursor model and all per-packet consumers remain in the caller's hot path.
    // Preserve parity with `position` while appending only the coalescing policy argument.
    #[allow(clippy::too_many_arguments)]
    pub fn queue_position(
        &self,
        index: usize,
        hwnd: usize,
        mouse: u32,
        x: i32,
        y: i32,
        rect: Bounds,
        settings: MotionCoalescingSettings,
    ) -> Result<()> {
        settings.validate()?;
        self.ensure_open()?;
        self.ensure_fence_support()?;
        if settings == MotionCoalescingSettings::default() {
            return self.position(index, hwnd, mouse, x, y, rect);
        }
        let seat = self.seats.get(index).context("unknown user")?;
        ensure!(seat.mouse == mouse, "mouse is not assigned to this user");
        ensure!(rect.valid(), "target client rectangle is invalid");
        ensure!(
            crate::targets::client_bounds(hwnd, seat.target_pid)? == rect,
            "target client bounds changed; retry positioning"
        );
        ensure!(
            rect.contains(x, y),
            "cursor is outside the target client area"
        );
        self.remember_recipient(index, hwnd)?;
        let key = motion_key(index, seat.target_pid, Some(hwnd), mouse, Some(rect));
        self.queue_position_sample(index, key, x, y, settings)
    }

    fn queue_position_sample(
        &self,
        index: usize,
        key: MotionKey,
        x: i32,
        y: i32,
        settings: MotionCoalescingSettings,
    ) -> Result<()> {
        self.flush_expired_motion_index(index)?;
        if self.pending_motion.borrow()[index].is_some_and(|pending| {
            !pending.matches(key, settings)
                || !matches!(pending.payload, MotionPayload::Position { .. })
        }) {
            self.flush_motion_index(index)?;
        }
        let now = Instant::now();
        let should_flush = {
            let mut slots = self.pending_motion.borrow_mut();
            let pending = &mut slots[index];
            if let Some(pending) = pending {
                pending.push_position(key, x, y)?;
            } else {
                *pending = Some(PendingMotion::new(
                    key,
                    settings,
                    now,
                    MotionPayload::Position { x, y },
                )?);
            }
            pending.is_some_and(|motion| motion.should_flush(now))
        };
        if should_flush {
            self.flush_motion_index(index)?;
        }
        Ok(())
    }

    /// Queue one already-transformed relative motion packet for native publication.
    pub fn queue_motion(&self, packet: Packet, settings: MotionCoalescingSettings) -> Result<()> {
        validate_widened_motion(packet)?;
        settings.validate()?;
        self.ensure_open()?;
        let mut matches = self
            .seats
            .iter()
            .enumerate()
            .filter(|(_, seat)| seat.mouse == packet.device);
        let (index, seat) = matches
            .next()
            .context("emulated device is not assigned to this session")?;
        ensure!(
            matches.next().is_none(),
            "emulated device is assigned to more than one session seat"
        );
        if settings == MotionCoalescingSettings::default() {
            self.flush_motion_index(index)?;
            return self.publish_motion_direct(packet);
        }
        self.flush_expired_motion_index(index)?;
        self.ensure_selected_window(index)?;
        let (hwnd, rect) = {
            let targets = self.native_targets.borrow();
            let target = &targets[index];
            let hwnd = target
                .selected_window
                .or_else(|| target.pointer.map(|pointer| pointer.window));
            let rect = hwnd
                .map(|hwnd| crate::targets::client_bounds(hwnd, seat.target_pid))
                .transpose()?;
            (hwnd, rect)
        };
        if let Some(hwnd) = hwnd {
            self.remember_recipient(index, hwnd)?;
        }
        let key = motion_key(index, seat.target_pid, hwnd, packet.device, rect);
        if self.pending_motion.borrow()[index].is_some_and(|pending| {
            !pending.matches(key, settings)
                || !matches!(pending.payload, MotionPayload::Delta { .. })
        }) {
            self.flush_motion_index(index)?;
        }
        let now = Instant::now();
        let should_flush = {
            let mut slots = self.pending_motion.borrow_mut();
            let pending = &mut slots[index];
            if let Some(pending) = pending {
                pending.push_delta(key, packet.x, packet.y)?;
            } else {
                *pending = Some(PendingMotion::new(
                    key,
                    settings,
                    now,
                    MotionPayload::Delta {
                        x: i64::from(packet.x),
                        y: i64::from(packet.y),
                    },
                )?);
            }
            pending.is_some_and(|motion| motion.should_flush(now))
        };
        if should_flush {
            self.flush_motion_index(index)?;
        }
        Ok(())
    }

    fn publish_motion_direct(&self, packet: Packet) -> Result<()> {
        split_motion(i64::from(packet.x), i64::from(packet.y), |x, y| {
            self.submit_direct(Packet::motion(packet.device, x, y))
        })
    }

    fn ensure_selected_window(&self, index: usize) -> Result<()> {
        self.ensure_open()?;
        let selected = {
            let targets = self.native_targets.borrow();
            targets[index]
                .selected_window
                .or_else(|| targets[index].pointer.map(|pointer| pointer.window))
        };
        if selected.is_none() {
            ensure!(
                self.backend.window_supported(),
                "backend lacks window selection"
            );
            let window = |instance, hwnd| self.backend.window(instance, hwnd);
            let seat = &self.seats[index];
            let target = crate::targets::windows()?
                .into_iter()
                .filter(|target| target.pid == seat.target_pid)
                .find(|target| crate::targets::client_bounds(target.hwnd, seat.target_pid).is_ok())
                .with_context(|| {
                    format!(
                        "no visible target window is available for {} (PID {})",
                        seat.name, seat.target_pid
                    )
                })?;
            crate::targets::client_bounds(target.hwnd, seat.target_pid)?;
            self.remember_recipient(index, target.hwnd)?;
            window(self.instances[index], target.hwnd as u64)?;
            self.native_targets.borrow_mut()[index].selected_window = Some(target.hwnd);
        }
        Ok(())
    }

    pub fn motion_deadline(&self) -> Option<Instant> {
        self.pending_motion
            .borrow()
            .iter()
            .flatten()
            .map(|pending| pending.deadline())
            .min()
    }

    pub fn poll_motion(&self) -> Result<()> {
        let now = Instant::now();
        let mut failures = None;
        let mut structural_error = None;
        for index in 0..self.seats.len() {
            if self.pending_motion.borrow()[index].is_some_and(|pending| pending.should_flush(now))
                && let Err(error) = self.flush_motion_index(index)
                && let Err(error) = collect_motion_error(&mut failures, error)
            {
                structural_error.get_or_insert(error);
            }
        }
        structural_error.map_or_else(|| finish_motion_errors(failures), Err)
    }

    pub fn flush_motion(&self) -> Result<()> {
        let mut failures = None;
        let mut structural_error = None;
        for index in 0..self.seats.len() {
            if let Err(error) = self.flush_motion_index(index)
                && let Err(error) = collect_motion_error(&mut failures, error)
            {
                structural_error.get_or_insert(error);
            }
        }
        structural_error.map_or_else(|| finish_motion_errors(failures), Err)
    }

    fn flush_expired_motion_index(&self, index: usize) -> Result<()> {
        let now = Instant::now();
        if self.pending_motion.borrow()[index].is_some_and(|pending| pending.should_flush(now)) {
            self.flush_motion_index(index)?;
        }
        Ok(())
    }

    fn flush_motion_index(&self, index: usize) -> Result<()> {
        let pending = self
            .pending_motion
            .borrow_mut()
            .get_mut(index)
            .context("unknown user")?
            .take();
        let Some(pending) = pending else {
            return Ok(());
        };
        let result = (|| -> Result<()> {
            match pending.payload {
                MotionPayload::Position { x, y } => self.position_direct(
                    index,
                    pending
                        .key
                        .hwnd
                        .context("pending position has no target window")?,
                    pending.key.mouse,
                    x,
                    y,
                    pending
                        .key
                        .client
                        .context("pending position has no client bounds")?,
                ),
                MotionPayload::Delta { x, y } => {
                    let index = pending.key.user;
                    let seat = self
                        .seats
                        .get(index)
                        .context("unknown pending motion user")?;
                    let hwnd = pending
                        .key
                        .hwnd
                        .context("pending relative motion has no target")?;
                    ensure!(
                        self.process_watches[index].exited().is_none(),
                        "assigned target process exited before motion publication"
                    );
                    ensure!(
                        crate::targets::client_bounds(hwnd, seat.target_pid)?
                            == pending
                                .key
                                .client
                                .context("pending relative motion has no client bounds")?,
                        "target client bounds changed before motion publication"
                    );
                    split_motion(x, y, |sx, sy| {
                        self.submit_direct(Packet::motion(pending.key.mouse, sx, sy))
                    })
                }
            }
        })();
        result.map_err(|error| MotionPublicationError::one(pending.key.mouse, error))
    }

    /// Submit one assigned device packet to a PID-validated window, then restore the
    /// window currently holding this user's independent virtual pointer.
    pub fn submit_to(&self, index: usize, hwnd: usize, packet: Packet) -> Result<()> {
        self.flush_motion_index(index)?;
        self.submit_to_direct(index, hwnd, packet)
    }

    fn submit_to_direct(&self, index: usize, hwnd: usize, packet: Packet) -> Result<()> {
        self.ensure_open()?;
        packet.validate()?;
        ensure!(
            packet.kind != 2 || self.backend.input_version >= 2,
            "Rebuild the native backend for Unicode text support"
        );
        let instance = *self.instances.get(index).context("unknown user")?;
        let seat = self.seats.get(index).context("unknown user")?;
        ensure!(
            if packet.kind == 0 {
                seat.mouse == packet.device
            } else {
                seat.keyboard == Some(packet.device)
            },
            "emulated device is not assigned to this user"
        );
        crate::targets::client_bounds(hwnd, seat.target_pid)?;
        self.remember_recipient(index, hwnd)?;
        ensure!(
            self.backend.submit_supported(),
            "backend lacks emulated input support"
        );
        let submit = |instance, packet: &Packet| self.backend.submit(instance, packet);
        ensure!(
            self.backend.window_supported(),
            "backend lacks window selection"
        );
        let window = |instance, hwnd| self.backend.window(instance, hwnd);
        let mut native_targets = self.native_targets.borrow_mut();
        let target = native_targets.get_mut(index).context("unknown user")?;
        let restore = target
            .selected_window
            .or_else(|| target.pointer.map(|p| p.window));
        {
            window(instance, hwnd as u64)?;
            submit(instance, &packet)?;
            if let Some(restore) = restore {
                window(instance, restore as u64)?;
            }
        }
        target.selected_window = restore.or(Some(hwnd));
        Ok(())
    }
    pub fn submit(&self, packet: Packet) -> Result<()> {
        self.flush_motion()?;
        self.submit_direct(packet)
    }

    fn submit_direct(&self, packet: Packet) -> Result<()> {
        self.ensure_open()?;
        packet.validate()?;
        ensure!(
            packet.kind != 2 || self.backend.input_version >= 2,
            "Rebuild the native backend for Unicode text support"
        );
        let mut matching_seats = self.seats.iter().enumerate().filter(|(_, seat)| {
            if packet.kind == 0 {
                seat.mouse == packet.device
            } else {
                seat.keyboard == Some(packet.device)
            }
        });
        let (seat_index, _) = matching_seats
            .next()
            .context("emulated device is not assigned to this session")?;
        ensure!(
            matching_seats.next().is_none(),
            "emulated device is assigned to more than one session seat"
        );
        ensure!(
            self.backend.submit_supported(),
            "backend lacks emulated input support"
        );
        let submit = |instance, packet: &Packet| self.backend.submit(instance, packet);
        let instance = self.instances[seat_index];
        let selected = {
            let targets = self.native_targets.borrow();
            targets[seat_index]
                .selected_window
                .or_else(|| targets[seat_index].pointer.map(|pointer| pointer.window))
        };
        if selected.is_none() {
            ensure!(
                self.backend.window_supported(),
                "backend lacks window selection"
            );
            let window = |instance, hwnd| self.backend.window(instance, hwnd);
            let seat = &self.seats[seat_index];
            let target = crate::targets::windows()?
                .into_iter()
                .filter(|target| target.pid == seat.target_pid)
                .find(|target| crate::targets::client_bounds(target.hwnd, seat.target_pid).is_ok())
                .with_context(|| {
                    format!(
                        "no visible target window is available for {} (PID {})",
                        seat.name, seat.target_pid
                    )
                })?;
            crate::targets::client_bounds(target.hwnd, seat.target_pid)?;
            self.remember_recipient(seat_index, target.hwnd)?;
            // Select a validated window before the native broadcast so fixed-mode
            // virtual input is associated with this seat's process for cleanup.
            window(instance, target.hwnd as u64)?;
            self.native_targets.borrow_mut()[seat_index].selected_window = Some(target.hwnd);
        }
        // Broadcast legacy packets are filtered by each target instance. Fence the
        // last Rust-selected recipient in every instance so cleanup remains ordered.
        for index in 0..self.instances.len() {
            let hwnd = {
                let targets = self.native_targets.borrow();
                targets[index]
                    .selected_window
                    .or_else(|| targets[index].pointer.map(|p| p.window))
            };
            if let Some(hwnd) = hwnd {
                self.remember_recipient(index, hwnd)?;
            }
        }
        // Broadcast so the native device filters, not this transport, enforce isolation.
        for &instance in &self.instances {
            {
                submit(instance, &packet)?;
            }
        }
        Ok(())
    }
    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }
    pub fn pause(&self, paused: bool) -> Result<()> {
        if self.shutdown_attempted.get() {
            return Ok(());
        }
        let mut errors = CleanupErrors::default();
        for (index, &instance) in self.instances.iter().enumerate() {
            if self.alive(index) {
                errors.record(
                    self.backend
                        .freeze(instance, paused)
                        .with_context(|| format!("pause PID {}", self.seats[index].target_pid)),
                );
            }
        }
        errors.finish()
    }

    /// Stops input and waits for each target GUI thread to remove a fence posted
    /// behind the release packets. A receipt is queue-level cleanup evidence only.
    pub fn shutdown(&mut self) -> Result<ShutdownReport> {
        self.shutdown_inner(true)
    }
    fn shutdown_inner(&mut self, prepare: bool) -> Result<ShutdownReport> {
        ensure!(
            !self.shutdown_attempted.get(),
            "ProtoInput session shutdown was already attempted"
        );
        let mut motion_errors = (0..self.instances.len())
            .map(|index| self.flush_motion_index(index).err())
            .collect::<Vec<_>>();
        self.shutdown_attempted.set(true);
        let api = &self.backend;
        let report = cleanup_targets(self.instances.len(), |index, stage| {
            let instance = self.instances[index];
            if !self.alive(index) {
                return Ok(ShutdownReport {
                    targets_exited: usize::from(stage == CleanupStage::Quiesce),
                    ..ShutdownReport::default()
                });
            }
            if stage == CleanupStage::Quiesce {
                let mut errors = CleanupErrors::default();
                errors.record(motion_errors[index].take().map_or(Ok(()), Err));
                errors.record(api.freeze(instance, true));
                errors.record(api.stop_focus(instance));
                errors.record(api.messages(instance, false, false, false, false));
                errors.record(api.draw(instance, false));
                if api.block_supported() {
                    errors.record(api.unblock(instance, 0x02A3));
                }
                errors.finish()?;
            } else if stage == CleanupStage::Drain {
                let requests = self.native_targets.borrow()[index]
                    .recipients
                    .iter()
                    .copied()
                    .map(|recipient| FenceRequest {
                        instance,
                        recipient,
                        expected_process: self.process_watches[index]
                            .handle(self.seats[index].target_pid)
                            .expect("session process watch matches its seat"),
                    })
                    .collect::<Vec<_>>();
                if !requests.is_empty() && api.fence_supported() {
                    return queue_fence::drain(&requests, |instance, hwnd, event, cookie| {
                        api.queue_fence(instance, hwnd, event, cookie)
                    });
                }
                ensure!(
                    requests.is_empty() || !self.requires_queue_fence,
                    "backend lacks queue-fence support for virtual cleanup"
                );
            } else {
                for &filter in FILTERS {
                    api.disable_filter(instance, filter)?;
                }
                for &hook in HOOKS.iter().rev() {
                    if self.lifecycle[index].is_some() && hook == 0 {
                        continue;
                    }
                    api.uninstall(instance, hook)?;
                }
            }
            Ok(ShutdownReport::default())
        })?;
        if prepare {
            for attachment in self.lifecycle.iter_mut().flatten() {
                attachment.prepare()?;
            }
        }
        Ok(report)
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if !self.shutdown_attempted.get() {
            let _ = self.shutdown_inner(false);
        }
    }
}

fn is_virtual(device: u32) -> bool {
    device & emulated::MASK == emulated::BASE
}

fn target_bits(pid: u32) -> Result<u32> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        ensure!(
            !process.is_null(),
            "cannot open PID {pid}: {}",
            std::io::Error::last_os_error()
        );
        let mut exit_code = 0;
        let alive = GetExitCodeProcess(process, &mut exit_code);
        let bits = crate::process_arch::process_bits(process);
        CloseHandle(process);
        ensure!(alive != 0 && exit_code == 259, "PID {pid} is unavailable");
        bits
    }
}

fn virtual_seat(seat: &ResolvedSeat) -> bool {
    is_virtual(seat.mouse) && seat.keyboard.is_none_or(is_virtual)
}

/// Lazy desktop attachment. Rust selects the recipient of every virtual packet.
/// One native state exists per process; the desktop policy grants exclusive ownership.
pub struct DesktopBackend {
    // Targets own lifecycle function pointers into backend's loaded DLL.
    targets: Vec<DesktopTarget>,
    backend: Backend,
    devices: Vec<ResolvedSeat>,
    paused: bool,
    shutdown_attempted: bool,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum TargetStartup {
    Pending,
    Failed,
    Ready,
}

struct DesktopTarget {
    startup: Rc<Cell<TargetStartup>>,
    application_focus: ApplicationFocus,
    pid: u32,
    identity: crate::targets::ProcessIdentity,
    instance: u32,
    lifecycle: Option<crate::lifecycle::Attachment>,
    motion: RefCell<DesktopMotionState>,
    reported_exit: bool,
    recipients: RefCell<Vec<Recipient>>,
    cursor: RefCell<Option<crate::app_cursor::Observer>>,
    cursor_error: RefCell<Option<String>>,
    cursor_available: bool,
    keyboard_state: Option<crate::keyboard_state::Bridge>,
}
impl DesktopTarget {
    fn ready_to_unfreeze(&self) -> bool {
        self.startup.get() == TargetStartup::Ready
            && (self.application_focus != ApplicationFocus::NativeControls
                || self
                    .keyboard_state
                    .as_ref()
                    .is_some_and(crate::keyboard_state::Bridge::armed))
    }
}

fn complete_target_startup(
    startup: &Cell<TargetStartup>,
    configure: impl FnOnce() -> Result<()>,
) -> Result<()> {
    ensure!(
        startup.get() == TargetStartup::Pending,
        "target startup was already attempted"
    );
    startup.set(TargetStartup::Failed);
    configure()?;
    startup.set(TargetStartup::Ready);
    Ok(())
}
struct DesktopMotionState {
    window: usize,
    mouse: u32,
    client: Option<(i32, i32)>,
    pending: Vec<Option<PendingMotion>>,
}

fn require_expected_creation(actual: u64, expected: Option<u64>) -> Result<()> {
    ensure!(
        expected.is_none_or(|creation| actual == creation),
        "application creation changed before attachment"
    );
    Ok(())
}

fn with_expected_identity<I, T>(
    identity: I,
    expected: Option<u64>,
    creation: impl FnOnce(&I) -> u64,
    attach: impl FnOnce(I) -> Result<T>,
) -> Result<T> {
    require_expected_creation(creation(&identity), expected)?;
    attach(identity)
}

impl Backend {
    pub fn desktop_session(self, devices: Vec<ResolvedSeat>) -> Result<DesktopBackend> {
        ensure!(!devices.is_empty(), "profile has no users");
        self.require_virtual_scope(&devices)?;
        ensure!(
            devices
                .iter()
                .all(|s| s.mouse & emulated::MASK == emulated::BASE
                    && s.keyboard
                        .is_none_or(|k| k & emulated::MASK == emulated::BASE)),
            "Desktop routing currently requires virtual devices"
        );
        ensure!(
            self.submit_supported()
                && self.window_supported()
                && self.bounds_supported()
                && self.block_supported()
                && self.input_version >= 3
                && self.fence_supported(),
            "backend lacks desktop routing exports"
        );
        Ok(DesktopBackend {
            backend: self,
            devices,
            targets: Vec::new(),
            paused: false,
            shutdown_attempted: false,
        })
    }
}
impl DesktopBackend {
    /// Discard one virtual mouse's unpublished slots across all targets without
    /// publishing or changing any other device's segment/deadline.
    pub fn discard_user_motion(&self, mouse: u32) -> Result<usize> {
        self.ensure_open()?;
        let slot = self.mouse_slot(mouse)?;
        let mut discarded = 0;
        for target in &self.targets {
            let mut motion = target.motion.borrow_mut();
            discarded += discard_motion_slot(&mut motion.pending, slot)?;
        }
        Ok(discarded)
    }
    /// Optional provider capability. Old routing runtimes remain fully usable.
    pub fn cursor_capability(&self) -> bool {
        self.backend.cursor_supported() && !self.backend.cursor_arches.is_empty()
    }
    /// Reads only this already-attached process. Provider failures never fail routing.
    pub fn cursor_observation(
        &self,
        index: usize,
        hwnd: usize,
        enabled: bool,
    ) -> crate::app_cursor::AppCursorObservation {
        use crate::app_cursor::{AppCursorObservation, Observer};
        let Some(target) = self.targets.get(index) else {
            return AppCursorObservation::fallback("No selected application", 0, hwnd);
        };
        let selected_window = target.motion.borrow().window;
        let hwnd = if selected_window != 0 {
            selected_window
        } else {
            hwnd
        };
        let fallback = |state| {
            let mut observation = AppCursorObservation::fallback(state, target.pid, hwnd);
            observation.capability = self.cursor_capability();
            observation
        };
        if target.startup.get() != TargetStartup::Ready {
            return fallback("Application startup failed; restart the target");
        }
        let mut cursor = target.cursor.borrow_mut();
        if self.shutdown_attempted || target.identity.ensure_alive().is_err() {
            *cursor = None;
            return fallback("Application cursor target exited");
        }
        if !self.cursor_capability() {
            return fallback("Application cursor provider unavailable");
        }
        if !target.cursor_available {
            let mut observation =
                fallback("Application cursor provider unavailable for this target architecture");
            observation.capability = false;
            return observation;
        }
        let stop = |instance, attachment| self.backend.cursor_stop(instance, attachment);
        if cursor.as_ref().is_some_and(Observer::stopped) {
            *cursor = None;
        }
        if !enabled {
            *target.cursor_error.borrow_mut() = None;
            if let Some(observer) = cursor.as_mut()
                && let Err(error) = observer.stop(stop)
            {
                let state = format!("Application cursor stop failed: {error}");
                *target.cursor_error.borrow_mut() = Some(state.clone());
                return fallback(&state);
            }
            return fallback("Application cursor following disabled");
        }
        if let Err(error) = Recipient::validate(hwnd, target.pid) {
            if let Some(observer) = cursor.as_mut() {
                let _ = observer.stop(stop);
            }
            return fallback(&format!("Application cursor unavailable: {error}"));
        }
        if cursor.as_ref().is_some_and(Observer::stopping) {
            return fallback("Waiting for application cursor observer to stop");
        }
        if let Some(error) = target.cursor_error.borrow().clone() {
            return fallback(&error);
        }
        if cursor.is_none() {
            match Observer::start(
                target.pid,
                target.instance,
                target.identity.handle(),
                |instance, mapping, mutex, attachment| {
                    self.backend
                        .cursor_start(instance, mapping, mutex, attachment)
                },
            ) {
                Ok(observer) => *cursor = Some(observer),
                Err(error) => {
                    let state = format!("Application cursor unavailable: {error}");
                    *target.cursor_error.borrow_mut() = Some(state.clone());
                    return fallback(&state);
                }
            }
        }
        let observer = cursor.as_mut().unwrap();
        if let Err(error) = observer.associate(hwnd, |instance, hwnd, association| {
            self.backend.cursor_associate(instance, hwnd, association)
        }) {
            let _ = observer.stop(stop);
            let state = format!("Application cursor unavailable: {error}");
            *target.cursor_error.borrow_mut() = Some(state.clone());
            return fallback(&state);
        }
        observer.read()
    }
    pub fn stop_cursor_observers(&self) -> Result<()> {
        let mut errors = CleanupErrors::default();
        if self.backend.cursor_supported() {
            for target in &self.targets {
                if target.identity.ensure_alive().is_err() {
                    continue;
                }
                if let Some(observer) = target.cursor.borrow_mut().as_mut() {
                    errors.record(
                        observer
                            .stop(|instance, attachment| {
                                self.backend.cursor_stop(instance, attachment)
                            })
                            .with_context(|| {
                                format!("cursor observer Stop for PID {}", target.pid)
                            }),
                    );
                }
            }
        }
        errors.finish()
    }
    fn ensure_open(&self) -> Result<()> {
        ensure!(!self.shutdown_attempted, "desktop backend is shutting down");
        Ok(())
    }

    fn ensure_target_started(&self, index: usize) -> Result<()> {
        ensure!(
            self.targets
                .get(index)
                .context("unknown target")?
                .startup
                .get()
                == TargetStartup::Ready,
            "Application startup is incomplete; stop routing and restart the target"
        );
        Ok(())
    }

    fn remember_recipient(&self, index: usize, hwnd: usize) -> Result<()> {
        let target = self.targets.get(index).context("unknown target")?;
        let recipient = Recipient::validate(hwnd, target.pid)?;
        queue_fence::remember(&mut target.recipients.borrow_mut(), recipient)
    }

    fn mouse_slot(&self, mouse: u32) -> Result<usize> {
        self.devices
            .iter()
            .position(|seat| seat.mouse == mouse)
            .context("mouse is not assigned to the desktop backend")
    }

    /// Read-only selection preflight; never publishes or drains queued motion.
    pub fn has_pending_motion(&self, mouse: u32) -> Result<bool> {
        let slot = self.mouse_slot(mouse)?;
        Ok(self
            .targets
            .iter()
            .any(|target| target.motion.borrow().pending[slot].is_some()))
    }

    fn flush_other_mouse_recipients(
        &self,
        slot: usize,
        recipient_index: usize,
        key: MotionKey,
        settings: MotionCoalescingSettings,
    ) -> Result<()> {
        let mut failures = None;
        let mut structural_error = None;
        for index in 0..self.targets.len() {
            let keep = index == recipient_index
                && self.targets[index].motion.borrow().pending[slot].is_some_and(|pending| {
                    pending.matches(key, settings)
                        && matches!(pending.payload, MotionPayload::Position { .. })
                });
            if !keep
                && self.targets[index].motion.borrow().pending[slot].is_some()
                && let Err(error) = self.flush_motion_slot(index, slot)
                && let Err(error) = collect_motion_error(&mut failures, error)
            {
                structural_error.get_or_insert(error);
            }
        }
        structural_error.map_or_else(|| finish_motion_errors(failures), Err)
    }

    /// Adds a virtual seat to future and already-open target processes. ProtoInput's
    /// registration exports return void, so this confirms calls were issued, not native
    /// acknowledgement. Device IDs remain reserved until this backend is dropped.
    pub fn register_virtual_user(&mut self, seat: ResolvedSeat) -> Result<()> {
        self.ensure_open()?;
        for (index, target) in self.targets.iter().enumerate() {
            if target.identity.ensure_alive().is_ok() {
                self.ensure_target_started(index)?;
            }
        }
        self.backend
            .require_virtual_scope(std::slice::from_ref(&seat))?;
        ensure!(
            seat.mouse & emulated::MASK == emulated::BASE
                && seat
                    .keyboard
                    .is_none_or(|key| key & emulated::MASK == emulated::BASE),
            "desktop routing requires virtual device IDs"
        );
        ensure!(
            seat.keyboard != Some(seat.mouse),
            "mouse and keyboard IDs must differ"
        );
        ensure!(
            !self.devices.iter().any(|existing| {
                existing.mouse == seat.mouse
                    || existing.keyboard == Some(seat.mouse)
                    || seat.keyboard == Some(existing.mouse)
                    || seat.keyboard.is_some() && seat.keyboard == existing.keyboard
            }),
            "virtual device ID is already reserved"
        );
        self.flush_motion()?;
        let api = &self.backend;
        for target in &self.targets {
            if target.identity.ensure_alive().is_err() {
                continue;
            }
            {
                api.freeze(target.instance, true)?;
                api.mouse(target.instance, seat.mouse)?;
                if let Some(keyboard) = seat.keyboard {
                    api.keyboard(target.instance, keyboard)?;
                }
                if seat.keyboard.is_some()
                    && !self.devices.iter().any(|device| device.keyboard.is_some())
                {
                    api.messages(target.instance, true, true, true, true)?;
                }
                api.freeze(target.instance, self.paused || !target.ready_to_unfreeze())?;
            }
        }
        for target in &self.targets {
            target.motion.borrow_mut().pending.push(None);
        }
        self.devices.push(seat);
        Ok(())
    }
    pub fn index(&self, pid: u32) -> Option<usize> {
        self.targets
            .iter()
            .position(|t| t.pid == pid && t.identity.ensure_alive().is_ok())
    }
    pub fn alive(&self, index: usize) -> bool {
        self.targets
            .get(index)
            .is_some_and(|t| t.identity.ensure_alive().is_ok())
    }
    pub fn exited(&mut self) -> Vec<u32> {
        self.targets
            .iter_mut()
            .filter_map(|t| {
                // Reap acknowledged bindings even after the user selected another
                // process; cursor_observation may never be called for this PID again.
                let stopped = t
                    .cursor
                    .borrow()
                    .as_ref()
                    .is_some_and(crate::app_cursor::Observer::stopped);
                if stopped {
                    *t.cursor.borrow_mut() = None;
                }
                if !t.reported_exit && t.identity.ensure_alive().is_err() {
                    t.reported_exit = true;
                    *t.cursor.borrow_mut() = None;
                    *t.cursor_error.borrow_mut() = None;
                    Some(t.pid)
                } else {
                    None
                }
            })
            .collect()
    }
    pub fn open(&mut self, pid: u32) -> Result<usize> {
        self.open_with_focus(pid, ApplicationFocus::Compatibility)
    }

    /// Attaches only to the selected original process creation identity.
    pub fn open_expected(&mut self, pid: u32, creation: u64) -> Result<usize> {
        self.open_expected_with_focus(pid, creation, ApplicationFocus::Compatibility)
    }

    /// Inspects an owned attachment without opening or attaching a new target.
    pub fn validate_cached_creation(&self, pid: u32, creation: u64) -> Result<Option<usize>> {
        self.ensure_open()?;
        let Some(index) = self.index(pid) else {
            return Ok(None);
        };
        require_expected_creation(self.targets[index].identity.creation, Some(creation))?;
        self.ensure_target_started(index)?;
        self.targets[index].identity.ensure_alive()?;
        Ok(Some(index))
    }

    /// Read-only preflight before ownership or input-recipient changes.
    pub fn validate_application_focus(&self, pid: u32, focus: ApplicationFocus) -> Result<()> {
        self.ensure_open()?;
        if focus == ApplicationFocus::NativeControls {
            ensure!(
                self.backend.keyboard_supported()
                    && self.backend.worker_lifecycle().is_some()
                    && !self.backend.cursor_arches.is_empty(),
                "Native controls require the updated keyboard-state backend and a supervised virtual session"
            );
        }
        if let Some(index) = self.index(pid) {
            self.ensure_target_started(index)?;
            ensure!(
                self.targets[index].application_focus == focus,
                "Application is attached with another focus policy; stop and restart routing"
            );
            if focus == ApplicationFocus::NativeControls {
                ensure!(
                    self.targets[index].ready_to_unfreeze(),
                    "Keyboard-state startup is incomplete; stop routing and restart the target"
                );
            }
        }
        Ok(())
    }

    pub fn open_with_focus(&mut self, pid: u32, focus: ApplicationFocus) -> Result<usize> {
        self.open_inner(pid, None, focus)
    }

    pub fn open_expected_with_focus(
        &mut self,
        pid: u32,
        creation: u64,
        focus: ApplicationFocus,
    ) -> Result<usize> {
        self.open_inner(pid, Some(creation), focus)
    }

    fn open_inner(
        &mut self,
        pid: u32,
        expected_creation: Option<u64>,
        focus: ApplicationFocus,
    ) -> Result<usize> {
        self.ensure_open()?;
        self.validate_application_focus(pid, focus)?;
        if let Some(index) = self.index(pid) {
            require_expected_creation(self.targets[index].identity.creation, expected_creation)?;
            self.targets[index].identity.ensure_alive()?;
            return Ok(index);
        }
        ensure!(
            pid != std::process::id(),
            "cannot target the routing worker"
        );
        let identity = if self.backend.worker_lifecycle().is_some() {
            crate::targets::ProcessIdentity::open_for_attach(pid)?
        } else {
            crate::targets::ProcessIdentity::open(pid)?
        };
        crate::process_arch::ensure_host_target(identity.bits)?;
        require_expected_creation(identity.creation, expected_creation)?;
        let retained_identity = identity.try_clone()?;
        let bits = identity.bits;
        if focus == ApplicationFocus::NativeControls {
            ensure!(
                self.backend.cursor_arches.contains(&bits),
                "Native controls keyboard-state provider is missing for the {bits}-bit target"
            );
        }
        if self.backend.worker_lifecycle().is_none() {
            crate::targets::require_fresh_target(pid)?;
        }
        for name in [
            format!("ProtoInputIJ{bits}.exe"),
            format!("ProtoInputIJP{bits}.dll"),
            format!("ProtoInputHooks{bits}.dll"),
        ] {
            ensure!(
                self.backend.path.join(&name).is_file(),
                "runtime file missing: {name}"
            );
        }
        let index = self.targets.len();
        let api = &self.backend;
        identity.ensure_alive()?;
        {
            let lifecycle = if let Some(lifecycle) = api.worker_lifecycle() {
                Some(with_expected_identity(
                    identity,
                    expected_creation,
                    |identity| identity.creation,
                    |identity| lifecycle.attach(identity, &api.folder),
                )?)
            } else {
                None
            };
            let resident = lifecycle.is_some();
            let instance = match lifecycle.as_ref() {
                Some(attachment) => attachment.instance,
                None => api.inject(pid)?,
            };
            ensure!(instance != 0, "ProtoInput failed to attach PID {pid}");
            // Record immediately so a later failure still owns cleanup.
            self.targets.push(DesktopTarget {
                startup: Rc::new(Cell::new(TargetStartup::Pending)),
                application_focus: focus,
                pid,
                identity: retained_identity,
                instance,
                lifecycle,
                motion: RefCell::new(DesktopMotionState {
                    window: 0,
                    mouse: 0,
                    client: None,
                    pending: vec![None; self.devices.len()],
                }),
                reported_exit: false,
                recipients: RefCell::new(Vec::new()),
                cursor: RefCell::new(None),
                cursor_error: RefCell::new(None),
                cursor_available: api.cursor_arches.contains(&bits),
                keyboard_state: None,
            });
            let startup = self.targets[index].startup.clone();
            complete_target_startup(&startup, || {
                api.freeze(instance, true)?;
                if focus == ApplicationFocus::NativeControls {
                    // StopFocus's backend workaround briefly starts the loop. Clear
                    // every notice first so it cannot replay resident fake focus.
                    // Pipe calls request configuration; they do not acknowledge it.
                    api.focus(instance, 10, false, false, false, false, false)?;
                    api.stop_focus(instance)?;
                    api.uninstall(instance, 10)?; // FocusHooks: restore application APIs.
                }
                api.setup(instance, index as i32 + 1)?;
                for seat in &self.devices {
                    api.mouse(instance, seat.mouse)?;
                    if let Some(k) = seat.keyboard {
                        api.keyboard(instance, k)?;
                    }
                }
                for &hook in HOOKS {
                    if resident && hook == 0
                        || focus == ApplicationFocus::NativeControls && hook == 10
                    {
                        continue;
                    }
                    api.install(instance, hook)?;
                }
                for &filter in FILTERS {
                    if focus == ApplicationFocus::NativeControls && matches!(filter, 2..=4) {
                        // Preserve WM_MOUSEACTIVATE / WM_ACTIVATE / WM_ACTIVATEAPP.
                        api.disable_filter(instance, filter)?;
                    } else {
                        api.enable_filter(instance, filter)?;
                    }
                }
                api.messages(
                    instance,
                    true,
                    true,
                    true,
                    self.devices.iter().any(|s| s.keyboard.is_some()),
                )?;
                api.draw(instance, false)?;
                api.block(instance, 0x02A3)?; // WM_MOUSELEAVE belongs to the shared OS pointer.
                if focus == ApplicationFocus::Compatibility {
                    api.focus(instance, 10, true, true, true, true, true)?;
                }
                if focus == ApplicationFocus::NativeControls {
                    let target = &mut self.targets[index];
                    let generation = target
                        .lifecycle
                        .as_ref()
                        .context("Native controls need a retained lifecycle attachment")?
                        .generation();
                    target.keyboard_state = Some(crate::keyboard_state::Bridge::new(
                        pid, instance, generation,
                    )?);
                    let keyboard = target.keyboard_state.as_mut().unwrap();
                    debug_assert_eq!(keyboard.epoch(), generation);
                    keyboard.start(
                        target.identity.handle(),
                        |instance, epoch, action, mapping, event| {
                            api.keyboard_command(instance, epoch, action, mapping, event)
                        },
                    )?;
                }
                api.freeze(instance, false)?;
                Ok(())
            })?;
        }
        Ok(index)
    }
    pub fn position(
        &mut self,
        index: usize,
        hwnd: usize,
        mouse: u32,
        x: i32,
        y: i32,
        rect: mux_core::desktop::Bounds,
    ) -> Result<()> {
        self.flush_motion_index(index)?;
        self.position_direct(index, hwnd, mouse, x, y, rect)
    }

    fn position_direct(
        &self,
        index: usize,
        hwnd: usize,
        mouse: u32,
        x: i32,
        y: i32,
        rect: Bounds,
    ) -> Result<()> {
        self.ensure_open()?;
        self.ensure_target_started(index)?;
        ensure!(self.alive(index), "target process exited");
        ensure!(rect.valid(), "target client rectangle is invalid");
        ensure!(
            rect.contains(x, y),
            "cursor is outside the target client area"
        );
        ensure!(
            crate::targets::client_bounds(hwnd, self.targets[index].pid)? == rect,
            "target client bounds changed; retry positioning"
        );
        self.remember_recipient(index, hwnd)?;
        let target = &self.targets[index];
        let api = &self.backend;
        let client = (x - rect.left, y - rect.top);
        let mut motion = target.motion.borrow_mut();
        {
            api.window(target.instance, hwnd as u64)?;
            if motion.window != hwnd || motion.mouse != mouse || motion.client.is_none() {
                api.window(target.instance, hwnd as u64)?;
                api.bounds(target.instance, false, false)?;
                api.submit(target.instance, &Packet::motion(mouse, -32767, -32767))?;
                api.submit(target.instance, &Packet::motion(mouse, 1, 1))?;
                api.bounds(target.instance, true, false)?;
                motion.client = Some((0, 0));
                motion.window = hwnd;
                motion.mouse = mouse;
            }
            let old = motion.client.unwrap();
            let mut dx = client.0 - old.0;
            let mut dy = client.1 - old.1;
            // Always publish a final move, including zero, to establish hover before button-down.
            loop {
                let sx = dx.clamp(-32767, 32767);
                let sy = dy.clamp(-32767, 32767);
                api.submit(target.instance, &Packet::motion(mouse, sx, sy))?;
                dx -= sx;
                dy -= sy;
                if dx == 0 && dy == 0 {
                    break;
                }
            }
            motion.client = Some(client);
        }
        Ok(())
    }

    /// Queue an already-transformed absolute cursor goal. Per-packet cursor policy
    /// remains in the caller; only native publication is combined here.
    // Preserve parity with `position` while appending only the coalescing policy argument.
    #[allow(clippy::too_many_arguments)]
    pub fn queue_position(
        &self,
        index: usize,
        hwnd: usize,
        mouse: u32,
        x: i32,
        y: i32,
        rect: Bounds,
        settings: MotionCoalescingSettings,
    ) -> Result<()> {
        settings.validate()?;
        self.ensure_open()?;
        self.ensure_target_started(index)?;
        ensure!(self.alive(index), "target process exited");
        let slot = self.mouse_slot(mouse)?;
        ensure!(rect.valid(), "target client rectangle is invalid");
        ensure!(
            rect.contains(x, y),
            "cursor is outside the target client area"
        );
        let target = self.targets.get(index).context("unknown target")?;
        ensure!(
            crate::targets::client_bounds(hwnd, target.pid)? == rect,
            "target client bounds changed; retry positioning"
        );
        self.remember_recipient(index, hwnd)?;
        let key = motion_key(slot, target.pid, Some(hwnd), mouse, Some(rect));
        if settings == MotionCoalescingSettings::default() {
            self.flush_other_mouse_recipients(slot, index, key, settings)?;
            return self.position_direct(index, hwnd, mouse, x, y, rect);
        }
        self.flush_expired_motion_slot(index, slot)?;
        self.flush_other_mouse_recipients(slot, index, key, settings)?;
        let now = Instant::now();
        let should_flush = {
            let mut motion = self.targets[index].motion.borrow_mut();
            let pending_slot = &mut motion.pending[slot];
            if let Some(pending) = pending_slot.as_mut() {
                pending.push_position(key, x, y)?;
            } else {
                *pending_slot = Some(PendingMotion::new(
                    key,
                    settings,
                    now,
                    MotionPayload::Position { x, y },
                )?);
            }
            pending_slot.is_some_and(|pending| pending.should_flush(now))
        };
        if should_flush {
            self.flush_motion_slot(index, slot)?;
        }
        Ok(())
    }

    pub fn motion_deadline(&self) -> Option<Instant> {
        self.targets
            .iter()
            .flat_map(|target| {
                target
                    .motion
                    .borrow()
                    .pending
                    .iter()
                    .flatten()
                    .map(|pending| pending.deadline())
                    .collect::<Vec<_>>()
            })
            .min()
    }

    pub fn poll_motion(&self) -> Result<()> {
        let now = Instant::now();
        let mut failures = None;
        let mut structural_error = None;
        for index in 0..self.targets.len() {
            let slot_count = self.devices.len();
            for slot in 0..slot_count {
                if self.targets[index].motion.borrow().pending[slot]
                    .is_some_and(|pending| pending.should_flush(now))
                    && let Err(error) = self.flush_motion_slot(index, slot)
                    && let Err(error) = collect_motion_error(&mut failures, error)
                {
                    structural_error.get_or_insert(error);
                }
            }
        }
        structural_error.map_or_else(|| finish_motion_errors(failures), Err)
    }

    pub fn flush_motion(&self) -> Result<()> {
        let mut failures = None;
        let mut structural_error = None;
        for index in 0..self.targets.len() {
            for slot in 0..self.devices.len() {
                if let Err(error) = self.flush_motion_slot(index, slot)
                    && let Err(error) = collect_motion_error(&mut failures, error)
                {
                    structural_error.get_or_insert(error);
                }
            }
        }
        structural_error.map_or_else(|| finish_motion_errors(failures), Err)
    }

    fn flush_expired_motion_slot(&self, index: usize, slot: usize) -> Result<()> {
        let now = Instant::now();
        if self
            .targets
            .get(index)
            .context("unknown target")?
            .motion
            .borrow()
            .pending[slot]
            .is_some_and(|pending| pending.should_flush(now))
        {
            self.flush_motion_slot(index, slot)?;
        }
        Ok(())
    }

    fn flush_motion_index(&self, index: usize) -> Result<()> {
        let mut failures = None;
        let mut structural_error = None;
        for slot in 0..self.devices.len() {
            if let Err(error) = self.flush_motion_slot(index, slot)
                && let Err(error) = collect_motion_error(&mut failures, error)
            {
                structural_error.get_or_insert(error);
            }
        }
        structural_error.map_or_else(|| finish_motion_errors(failures), Err)
    }

    fn flush_motion_slot(&self, index: usize, slot: usize) -> Result<()> {
        let target = self.targets.get(index).context("unknown target")?;
        let pending = target
            .motion
            .borrow_mut()
            .pending
            .get_mut(slot)
            .context("unknown mouse slot")?
            .take();
        let Some(pending) = pending else {
            return Ok(());
        };
        let result = (|| -> Result<()> {
            match pending.payload {
                MotionPayload::Position { x, y } => self.position_direct(
                    index,
                    pending
                        .key
                        .hwnd
                        .context("pending position has no target window")?,
                    pending.key.mouse,
                    x,
                    y,
                    pending
                        .key
                        .client
                        .context("pending position has no client bounds")?,
                ),
                MotionPayload::Delta { x, y } => {
                    ensure!(
                        self.alive(index),
                        "target process exited before motion publication"
                    );
                    split_motion(x, y, |sx, sy| {
                        self.submit_direct(index, Packet::motion(pending.key.mouse, sx, sy))
                    })
                }
            }
        })();
        result.map_err(|error| MotionPublicationError::one(pending.key.mouse, error))
    }

    pub fn submit(&self, index: usize, packet: Packet) -> Result<()> {
        self.flush_motion_index(index)?;
        self.submit_direct(index, packet)
    }

    fn submit_direct(&self, index: usize, packet: Packet) -> Result<()> {
        self.ensure_open()?;
        self.ensure_target_started(index)?;
        packet.validate()?;
        ensure!(
            packet.kind != 2 || self.backend.input_version >= 2,
            "Rebuild the native backend for Unicode text support"
        );
        ensure!(self.alive(index), "target process exited");
        ensure!(
            self.devices.iter().any(|s| if packet.kind == 0 {
                s.mouse == packet.device
            } else {
                s.keyboard == Some(packet.device)
            }),
            "unassigned virtual device"
        );
        let target = &self.targets[index];
        let window = target.motion.borrow().window;
        ensure!(window != 0, "target has no validated input window");
        self.remember_recipient(index, window)?;
        {
            self.backend.submit(target.instance, &packet)?;
        }
        Ok(())
    }
    pub fn release_to(&self, index: usize, hwnd: usize, packet: Packet) -> Result<()> {
        self.ensure_open()?;
        self.ensure_target_started(index)?;
        ensure!(self.alive(index), "target process exited");
        self.flush_motion_index(index)?;
        self.submit_to_window_direct(index, hwnd, packet)
    }
    pub fn release_to_without_motion(
        &self,
        index: usize,
        hwnd: usize,
        packet: Packet,
    ) -> Result<()> {
        validate_cleanup_up(packet)?;
        self.submit_to_window_direct(index, hwnd, packet)
    }
    fn submit_to_window_direct(&self, index: usize, hwnd: usize, packet: Packet) -> Result<()> {
        self.ensure_open()?;
        self.ensure_target_started(index)?;
        ensure!(self.alive(index), "target process exited");
        let target = &self.targets[index];
        let selected_window = target.motion.borrow().window;
        let recipient = Recipient::validate(hwnd, target.pid)?;
        self.remember_recipient(index, hwnd)?;
        static FIRST: AtomicBool = AtomicBool::new(false);
        if qualification_first_key_down(&packet, &FIRST) {
            let target_identity_matches = target.pid == target.identity.pid;
            let recipient_pid_matches = recipient.pid == target.identity.pid;
            let recipient_window_matches = recipient.hwnd == hwnd;
            let selected_window_matches = selected_window == hwnd;
            let instance_binding_matches = target
                .lifecycle
                .as_ref()
                .is_some_and(|attachment| attachment.instance == target.instance);
            let startup_config_calls_returned = target.startup.get() == TargetStartup::Ready;
            let requested_keyboard_messages =
                self.devices.iter().any(|seat| seat.keyboard.is_some());
            let packet_keyboard_assigned = self
                .devices
                .iter()
                .any(|seat| seat.keyboard == Some(packet.device));
            let requested_frozen = self.paused || !target.ready_to_unfreeze();
            let native_backend_selected = self.backend.rust.is_none();
            qualification_stderr(format_args!(
                "PIM_QUAL_KEY_CONTEXT_V1 target_identity_matches={target_identity_matches} recipient_pid_matches={recipient_pid_matches} recipient_window_matches={recipient_window_matches} selected_window_matches={selected_window_matches} instance_binding_matches={instance_binding_matches} startup_config_calls_returned={startup_config_calls_returned} requested_keyboard_messages={requested_keyboard_messages} packet_keyboard_assigned={packet_keyboard_assigned} requested_frozen={requested_frozen} native_backend_selected={native_backend_selected}"
            ));
        }
        {
            self.backend.window(target.instance, hwnd as u64)?;
        }
        let result = self.submit_direct(index, packet);
        {
            self.backend
                .window(target.instance, selected_window as u64)?;
        }
        result
    }
    pub fn pause(&mut self, paused: bool) -> Result<()> {
        if self.shutdown_attempted {
            return Ok(());
        }
        let mut errors = CleanupErrors::default();
        for (i, t) in self.targets.iter().enumerate() {
            if self.alive(i) {
                if !paused && t.startup.get() != TargetStartup::Ready {
                    errors.record(Err(anyhow::anyhow!(
                        "cannot unpause PID {} after incomplete startup",
                        t.pid
                    )));
                }
                errors.record(
                    self.backend
                        .freeze(t.instance, paused || !t.ready_to_unfreeze())
                        .with_context(|| format!("pause PID {}", t.pid)),
                );
            }
        }
        self.paused = paused;
        errors.finish()
    }

    /// Stops virtual emission, fences all target GUI threads, and only then removes
    /// the native filters that normalize synthesized keyboard releases.
    pub fn shutdown(&mut self) -> Result<ShutdownReport> {
        self.shutdown_inner(true)
    }
    fn shutdown_inner(&mut self, prepare: bool) -> Result<ShutdownReport> {
        ensure!(
            !self.shutdown_attempted,
            "desktop backend shutdown was already attempted"
        );
        let mut motion_errors = (0..self.targets.len())
            .map(|index| self.flush_motion_index(index).err())
            .collect::<Vec<_>>();
        self.shutdown_attempted = true;
        let report = cleanup_targets(self.targets.len(), |index, stage| {
            if !self.alive(index) {
                return Ok(ShutdownReport {
                    targets_exited: usize::from(stage == CleanupStage::Quiesce),
                    ..ShutdownReport::default()
                });
            }
            let api = &self.backend;
            let target = &mut self.targets[index];
            if stage == CleanupStage::Quiesce {
                let mut errors = CleanupErrors::default();
                errors.record(motion_errors[index].take().map_or(Ok(()), Err));
                // Stop every emission mechanism even if the observer or stream
                // has failed. A failed target must not delay other targets' stop.
                errors.record(api.freeze(target.instance, true));
                errors.record(api.stop_focus(target.instance));
                errors.record(api.messages(target.instance, false, false, false, false));
                errors.record(api.draw(target.instance, false));
                errors.record(api.unblock(target.instance, 0x02A3));
                if let Some(observer) = target.cursor.borrow_mut().as_mut() {
                    errors.record(
                        observer.stop(|instance, attachment| api.cursor_stop(instance, attachment)),
                    );
                }
                errors.finish()?;
            } else if stage == CleanupStage::Drain {
                let requests = target
                    .recipients
                    .borrow()
                    .iter()
                    .copied()
                    .map(|recipient| FenceRequest {
                        instance: target.instance,
                        recipient,
                        expected_process: target.identity.handle(),
                    })
                    .collect::<Vec<_>>();
                if !requests.is_empty() {
                    ensure!(
                        api.fence_supported(),
                        "backend lacks queue-fence support for virtual cleanup"
                    );
                    return queue_fence::drain(&requests, |instance, hwnd, event, cookie| {
                        api.queue_fence(instance, hwnd, event, cookie)
                    });
                }
            } else {
                if let Some(keyboard) = &mut target.keyboard_state
                    && keyboard.attempted()
                {
                    keyboard.stop(
                        target.identity.handle(),
                        |instance, epoch, action, mapping, event| {
                            api.keyboard_command(instance, epoch, action, mapping, event)
                        },
                    )?;
                }
                for &filter in FILTERS {
                    api.disable_filter(target.instance, filter)?;
                }
                for &hook in HOOKS.iter().rev() {
                    if target.lifecycle.is_some() && hook == 0 {
                        continue;
                    }
                    api.uninstall(target.instance, hook)?;
                }
            }
            Ok(ShutdownReport::default())
        })?;
        if prepare {
            for target in &mut self.targets {
                if let Some(attachment) = &mut target.lifecycle {
                    attachment.prepare()?;
                }
            }
        }
        Ok(report)
    }
}

fn discard_motion_slot(slots: &mut [Option<PendingMotion>], user: usize) -> Result<usize> {
    let slot = slots.get_mut(user).context("unknown motion slot")?;
    Ok(usize::from(slot.take().is_some()))
}
fn validate_cleanup_up(packet: Packet) -> Result<()> {
    packet.validate()?;
    ensure!(
        (packet.kind == 1 && packet.flags & 1 != 0)
            || (packet.kind == 0 && matches!(packet.flags, 2 | 8 | 32 | 128 | 512)),
        "cleanup-only submission requires a key/button UP"
    );
    Ok(())
}
impl Drop for DesktopBackend {
    fn drop(&mut self) {
        if !self.shutdown_attempted {
            let _ = self.shutdown_inner(false);
        }
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;

    #[test]
    fn failed_recorded_lazy_startup_cannot_reopen_or_publish() {
        // Exercise the same recorded-target completion boundary used by open_inner.
        // No loader, injection or native callbacks are involved in this fixture.
        let pid = std::process::id();
        let identity = crate::targets::ProcessIdentity::open(pid).unwrap();
        let creation = identity.creation;
        let startup = Rc::new(Cell::new(TargetStartup::Pending));
        let mouse = emulated::BASE | 2;
        let mut desktop = DesktopBackend {
            backend: Backend {
                native: None,
                rust: None,
                _lease: BACKEND_OWNER.lock().unwrap(),
                _thread: PhantomData,
                folder: vec![0],
                path: PathBuf::new(),
                input_version: 3,
                lifecycle: None,
                cursor_arches: HashSet::new(),
            },
            targets: vec![DesktopTarget {
                startup: startup.clone(),
                application_focus: ApplicationFocus::Compatibility,
                pid,
                identity,
                instance: 1,
                lifecycle: None,
                motion: RefCell::new(DesktopMotionState {
                    window: 0,
                    mouse: 0,
                    client: None,
                    pending: vec![None],
                }),
                reported_exit: false,
                recipients: RefCell::new(Vec::new()),
                cursor: RefCell::new(None),
                cursor_error: RefCell::new(None),
                cursor_available: false,
                keyboard_state: None,
            }],
            devices: vec![ResolvedSeat {
                name: "owned startup fixture".into(),
                mouse,
                keyboard: Some(emulated::BASE | 3),
                target_pid: pid,
            }],
            paused: false,
            shutdown_attempted: false,
        };
        let mut configured = 0;
        assert!(
            complete_target_startup(&startup, || {
                configured += 1;
                anyhow::bail!("lost setup ACK");
            })
            .is_err()
        );
        assert!(
            complete_target_startup(&startup, || {
                configured += 1;
                Ok(())
            })
            .is_err()
        );
        assert_eq!(configured, 1, "failed recorded startup may never retry");
        assert!(!desktop.targets[0].ready_to_unfreeze());
        assert!(
            desktop
                .open_expected(pid, creation)
                .unwrap_err()
                .to_string()
                .contains("startup")
        );
        assert!(desktop.validate_cached_creation(pid, creation).is_err());
        let bounds = Bounds {
            left: 0,
            top: 0,
            right: 100,
            bottom: 100,
        };
        assert!(
            desktop
                .position(0, 0, mouse, 1, 1, bounds)
                .unwrap_err()
                .to_string()
                .contains("startup")
        );
        assert!(
            desktop
                .queue_position(
                    0,
                    0,
                    mouse,
                    1,
                    1,
                    bounds,
                    MotionCoalescingSettings::default()
                )
                .is_err()
        );
        assert!(
            desktop
                .submit(0, Packet::motion(mouse, 1, 1))
                .unwrap_err()
                .to_string()
                .contains("startup")
        );
        assert!(
            desktop
                .release_to(0, 0, Packet::button(mouse, 0, true))
                .unwrap_err()
                .to_string()
                .contains("startup")
        );
        assert!(
            desktop
                .cursor_observation(0, 0, true)
                .state
                .contains("startup failed")
        );
        assert!(format!("{:#}", desktop.pause(false).unwrap_err()).contains("incomplete startup"));
        assert_eq!(desktop.targets[0].motion.borrow().window, 0);
        // The fake backend owns no target state that requires native shutdown.
        desktop.shutdown_attempted = true;
    }
}

#[cfg(test)]
mod cleanup_tests {
    use super::{CleanupStage, ShutdownReport, cleanup_targets};

    #[test]
    fn broken_streams_do_not_skip_healthy_target_shutdown() {
        let mut calls = Vec::new();
        let result = cleanup_targets(3, |index, stage| {
            calls.push((index, stage));
            if (index, stage) == (0, CleanupStage::Quiesce) {
                anyhow::bail!("observer Stop transfer uncertain");
            }
            if (index, stage) == (1, CleanupStage::Drain) {
                anyhow::bail!("queue fence timed out");
            }
            Ok(ShutdownReport::default())
        });
        let detail = format!("{:#}", result.unwrap_err());
        assert!(detail.contains("observer Stop transfer uncertain"));
        assert!(detail.contains("queue fence timed out"));
        assert_eq!(
            calls,
            vec![
                (0, CleanupStage::Quiesce),
                (1, CleanupStage::Quiesce),
                (2, CleanupStage::Quiesce),
                (1, CleanupStage::Drain),
                (2, CleanupStage::Drain),
                (2, CleanupStage::Retire),
            ]
        );
    }

    #[test]
    fn retirement_failure_preserves_failure_after_other_targets_retire() {
        let mut retired = Vec::new();
        let result = cleanup_targets(2, |index, stage| {
            if stage == CleanupStage::Retire {
                retired.push(index);
                if index == 1 {
                    anyhow::bail!("keyboard Stop unconfirmed");
                }
            }
            Ok(ShutdownReport::default())
        });
        assert_eq!(retired, vec![1, 0]);
        assert!(
            result.is_err(),
            "a partial cleanup must never permit all-target prepare"
        );
    }

    #[test]
    fn successful_cleanup_reports_each_drain_once() {
        let report = cleanup_targets(2, |_, stage| {
            Ok(ShutdownReport {
                acknowledged: usize::from(stage == CleanupStage::Drain),
                targets_exited: 0,
            })
        })
        .unwrap();
        assert_eq!(report.acknowledged, 2);
    }
}

#[cfg(test)]
mod expected_creation_tests {
    use super::{require_expected_creation, with_expected_identity};

    #[test]
    fn user_motion_discard_preserves_peer_payload_event_count_and_deadline_across_targets() {
        use super::{MotionKey, MotionPayload, PendingMotion, discard_motion_slot};
        use mux_core::desktop::{Bounds, MotionCoalescingSettings};
        let now = std::time::Instant::now();
        let settings = MotionCoalescingSettings {
            buffer_events: Some(64),
            interval_ms: Some(9),
        };
        let pending = |user, pid, mouse| {
            PendingMotion::new(
                MotionKey {
                    user,
                    pid,
                    hwnd: Some(pid as usize),
                    mouse,
                    client: Some(Bounds {
                        left: 0,
                        top: 0,
                        right: 100,
                        bottom: 100,
                    }),
                },
                settings,
                now,
                MotionPayload::Position { x: 12, y: 23 },
            )
            .unwrap()
        };
        let a = pending(0, 10, 0xf1000002);
        let mut b = pending(1, 20, 0xf1000004);
        b.push_position(b.key, 24, 46).unwrap();
        let mut targets = [vec![Some(a), Some(b)], vec![Some(a), Some(b)]];
        let peer_before = [targets[0][1], targets[1][1]];
        for target in &mut targets {
            assert_eq!(discard_motion_slot(target, 0).unwrap(), 1);
        }
        assert_eq!([targets[0][1], targets[1][1]], peer_before);
        assert_eq!(
            targets[0][1].unwrap().deadline(),
            now + std::time::Duration::from_millis(9)
        );
        assert_eq!(targets[0][1].unwrap().events, 2);
        assert_eq!(discard_motion_slot(&mut targets[0], 0).unwrap(), 0);
        let before = targets[0].clone();
        assert!(discard_motion_slot(&mut targets[0], 9).is_err());
        assert_eq!(targets[0], before);
    }

    #[test]
    fn cleanup_submission_accepts_only_valid_key_or_button_ups() {
        use super::validate_cleanup_up;
        use mux_core::emulated::Packet;
        let mouse = 0xf1000002;
        let keyboard = 0xf1000003;
        assert!(validate_cleanup_up(Packet::key_scanned(keyboard, 65, 30, 0, true)).is_ok());
        for button in 0..5 {
            assert!(validate_cleanup_up(Packet::button(mouse, button, true)).is_ok());
        }
        for packet in [
            Packet::key_scanned(keyboard, 65, 30, 0, false),
            Packet::button(mouse, 0, false),
            Packet::motion(mouse, 1, 2),
            Packet::wheel(mouse, 120),
            Packet::text_unit(keyboard, 65),
        ] {
            assert!(validate_cleanup_up(packet).is_err());
        }
    }

    #[test]
    fn attachment_preflight_rejects_different_creation() {
        assert!(require_expected_creation(42, Some(41)).is_err());
        assert!(require_expected_creation(42, Some(42)).is_ok());
        assert!(require_expected_creation(42, None).is_ok());
    }

    #[test]
    fn wrong_creation_never_reaches_attach() {
        let mut calls = 0;
        let result = with_expected_identity(
            42,
            Some(41),
            |identity| *identity,
            |_| {
                calls += 1;
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(calls, 0);
    }

    #[test]
    fn attach_receives_the_same_checked_identity() {
        let identity = Box::new(42u64);
        let address = std::ptr::from_ref(identity.as_ref());
        let attached = with_expected_identity(
            identity,
            Some(42),
            |identity| **identity,
            |identity| {
                assert_eq!(std::ptr::from_ref(identity.as_ref()), address);
                Ok(identity)
            },
        )
        .unwrap();
        assert_eq!(*attached, 42);
    }
}
