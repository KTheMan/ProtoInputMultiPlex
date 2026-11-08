#![cfg(windows)]

//! Optional target-side cursor observer. The DLL is intentionally kept loaded
//! through process exit because ProtoInput hooks may retain these export addresses.

mod keyboard;

use arc_swap::ArcSwapOption;
use cursor_snapshot_protocol::{
    KIND_BGRA8_PREMULTIPLIED, KIND_NONE, MAPPING_BYTES, MAX_DIMENSION, MAX_PAYLOAD_BYTES,
    RequestClock, STATUS_CAPTURE_FAILED, STATUS_EMPTY, STATUS_HIDDEN, STATUS_STOPPED,
    STATUS_UNSUPPORTED, STATUS_VALID, SnapshotHeader,
};
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, HWND},
    Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, GetObjectW, HBITMAP, HDC, HGDIOBJ, RGBQUAD,
        SelectObject,
    },
    System::{
        Memory::{FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, UnmapViewOfFile},
        Threading::{GetCurrentProcessId, GetCurrentThreadId, ReleaseMutex, WaitForSingleObject},
    },
    UI::WindowsAndMessaging::{
        CallNextHookEx, CopyIcon, DI_NORMAL, DestroyIcon, DrawIconEx, GetCursor, GetIconInfo,
        GetWindowThreadProcessId, HC_ACTION, HICON, HTCLIENT, ICONINFO, LoadCursorW, MSG,
        PM_REMOVE, PostMessageW, RegisterWindowMessageW, SendMessageW, SetWindowsHookExW,
        UnhookWindowsHookEx, WH_GETMESSAGE, WM_MOUSEMOVE, WM_SETCURSOR,
    },
};

const OK: u32 = 0;
const DISABLED: u32 = 1;
const DROPPED_BUSY: u32 = 2;
const INVALID: u32 = 3;
const CAPTURE_FAILED: u32 = 4;
const MAX_NATIVE_RETRIES: u32 = 3;
const WAIT_OBJECT_0: u32 = 0;
const WAIT_ABANDONED: u32 = 0x80;
const CURSOR_RESOURCES: [u32; 14] = [
    32512, 32513, 32514, 32515, 32516, 32642, 32643, 32644, 32645, 32646, 32648, 32649, 32650,
    32651,
];

static OBSERVER: OnceLock<ArcSwapOption<Observer>> = OnceLock::new();
static START_STOP_GATE: OnceLock<Mutex<()>> = OnceLock::new();
static STOCK_CURSORS: OnceLock<[usize; 14]> = OnceLock::new();
static WAKE_MESSAGE: OnceLock<u32> = OnceLock::new();
// Never wrap: stale queued wake messages must not match a later attachment.
static NEXT_WAKE_COOKIE: AtomicUsize = AtomicUsize::new(1);
// Includes retired workers and callbacks holding an old Arc after the public
// slot is cleared. Native cleanup must not acknowledge just that empty slot.
static LIVE_OBSERVERS: AtomicUsize = AtomicUsize::new(0);

struct ObserverLifetime;
impl ObserverLifetime {
    fn new() -> Self {
        LIVE_OBSERVERS.fetch_add(1, Ordering::AcqRel);
        Self
    }
}
impl Drop for ObserverLifetime {
    fn drop(&mut self) {
        LIVE_OBSERVERS.fetch_sub(1, Ordering::Release);
    }
}

fn global() -> &'static ArcSwapOption<Observer> {
    OBSERVER.get_or_init(ArcSwapOption::empty)
}
fn lifecycle_gate() -> &'static Mutex<()> {
    START_STOP_GATE.get_or_init(|| Mutex::new(()))
}
fn running() -> Result<Arc<Observer>, u32> {
    global().load_full().ok_or(DISABLED)
}

struct PendingIcon(HICON);
impl Drop for PendingIcon {
    fn drop(&mut self) {
        unsafe { DestroyIcon(self.0) };
    }
}

struct Pending {
    icon: Option<PendingIcon>,
    stock_id: Option<u32>,
    hwnd: u64,
    association: u64,
    request_sequence: u64,
}

struct Observer {
    mapping_handle: HANDLE,
    mutex_handle: HANDLE,
    mapped: *mut u8,
    pid: u32,
    attachment: u64,
    association_epoch: AtomicU64,
    selected_hwnd: AtomicU64,
    observed_request: RequestClock,
    pending: Mutex<Option<Pending>>,
    wake: Condvar,
    stopped: AtomicBool,
    thread_hook: Mutex<Option<usize>>,
    selected_thread: AtomicU32,
    selected_cookie: AtomicUsize,
    queued_cookie: AtomicUsize,
    native_retry_requested: AtomicBool,
    native_retry_cookie: AtomicUsize,
    native_retry_sequence: AtomicU64,
    native_retry_attempts: AtomicU32,
    // Last field: decrement only after Observer::drop closes its mappings and
    // every other field (including pending icons) has been destroyed.
    _lifetime: ObserverLifetime,
}
unsafe impl Send for Observer {}
unsafe impl Sync for Observer {}

impl Drop for Observer {
    fn drop(&mut self) {
        // The worker and all callbacks own Arcs. This runs only after they stop
        // borrowing the mapping, so Stop never unmaps underneath an in-flight call.
        self.detach_window_hook();
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.mapped.cast::<c_void>(),
            });
            CloseHandle(self.mapping_handle);
            CloseHandle(self.mutex_handle);
        }
    }
}

/// Maps target-local duplicated handles and starts one async worker. Both
/// handles are consumed on every path, including invalid input and spawn failure.
#[unsafe(no_mangle)]
pub extern "C" fn ProtoMuxCursorObserverStart(
    target_mapping: u64,
    target_mutex: u64,
    target_pid: u32,
    attachment_epoch: u64,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| unsafe {
        let _gate = lifecycle_gate().lock().unwrap_or_else(|e| e.into_inner());
        if !fits_native_handle(target_mapping)
            || !fits_native_handle(target_mutex)
            || target_mapping == 0
            || target_mutex == 0
            || target_mapping == target_mutex
        {
            close_consumed(target_mapping, target_mutex);
            return INVALID;
        }
        let mapping_handle = target_mapping as HANDLE;
        let mutex_handle = target_mutex as HANDLE;
        if target_pid == 0
            || attachment_epoch == 0
            || GetCurrentProcessId() != target_pid
            || global().load().is_some()
        {
            close_consumed(target_mapping, target_mutex);
            return INVALID;
        }
        let view = MapViewOfFile(mapping_handle, FILE_MAP_ALL_ACCESS, 0, 0, MAPPING_BYTES);
        let mapped = view.Value.cast::<u8>();
        if mapped.is_null() {
            close_consumed(target_mapping, target_mutex);
            return CAPTURE_FAILED;
        }
        let header = &*(mapped.cast::<SnapshotHeader>());
        if !header.validate(MAPPING_BYTES, target_pid, attachment_epoch) {
            UnmapViewOfFile(view);
            close_consumed(target_mapping, target_mutex);
            return INVALID;
        }
        let observer = Arc::new(Observer {
            mapping_handle,
            mutex_handle,
            mapped,
            pid: target_pid,
            attachment: attachment_epoch,
            association_epoch: AtomicU64::new(0),
            selected_hwnd: AtomicU64::new(0),
            observed_request: RequestClock::new(STATUS_EMPTY),
            pending: Mutex::new(None),
            wake: Condvar::new(),
            stopped: AtomicBool::new(false),
            thread_hook: Mutex::new(None),
            selected_thread: AtomicU32::new(0),
            selected_cookie: AtomicUsize::new(0),
            queued_cookie: AtomicUsize::new(0),
            native_retry_requested: AtomicBool::new(false),
            native_retry_cookie: AtomicUsize::new(0),
            native_retry_sequence: AtomicU64::new(0),
            native_retry_attempts: AtomicU32::new(0),
            _lifetime: ObserverLifetime::new(),
        });
        let worker_observer = Arc::clone(&observer);
        match thread::Builder::new()
            .name("ProtoInputMultiPlex cursor observer".into())
            .spawn(move || worker_loop(worker_observer))
        {
            Ok(_worker) => {
                global().store(Some(observer));
                OK
            }
            Err(_) => {
                drop(observer);
                CAPTURE_FAILED
            }
        }
    }))
    .unwrap_or(INVALID)
}

/// Associates the observer with one selected window and invalidates older work.
#[unsafe(no_mangle)]
pub extern "C" fn ProtoMuxCursorObserverAssociate(hwnd: u64, generation: u64) -> u32 {
    catch_unwind(AssertUnwindSafe(|| unsafe {
        let _gate = lifecycle_gate().lock().unwrap_or_else(|e| e.into_inner());
        let Ok(observer) = running() else {
            return DISABLED;
        };
        if observer.stopped.load(Ordering::Acquire)
            || !fits_native_handle(hwnd)
            || hwnd == 0
            || generation == 0
        {
            return INVALID;
        }
        let old = observer.association_epoch.load(Ordering::Acquire);
        if generation <= old {
            return INVALID;
        }
        let mut pid = 0;
        let target_thread = GetWindowThreadProcessId(hwnd as HWND, &mut pid);
        if target_thread == 0 || pid != observer.pid {
            return INVALID;
        }
        let Some(cookie) = allocate_wake_cookie() else {
            return INVALID;
        };
        if !observer.detach_window_hook() {
            return CAPTURE_FAILED;
        }
        observer.selected_hwnd.store(hwnd, Ordering::Release);
        observer
            .association_epoch
            .store(generation, Ordering::Release);
        observer.observed_request.advance(STATUS_EMPTY);
        if let Ok(mut pending) = observer.pending.try_lock() {
            pending.take();
        }
        observer.wake.notify_one();
        observer
            .selected_thread
            .store(target_thread, Ordering::Release);
        observer.selected_cookie.store(cookie, Ordering::Release);
        // The callback and target thread belong to this injected process, so no
        // external hook module or cross-process GUI-thread access is needed.
        let hook = SetWindowsHookExW(
            WH_GETMESSAGE,
            Some(cursor_message_hook),
            ptr::null_mut(),
            target_thread,
        );
        if hook.is_null() {
            observer.detach_window_hook();
            observer.observed_request.advance(STATUS_CAPTURE_FAILED);
            observer.wake.notify_one();
            return CAPTURE_FAILED;
        }
        *observer
            .thread_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(hook as usize);
        let expected = observer.observed_request.snapshot().0;
        observer.native_retry_attempts.store(0, Ordering::Release);
        drop(_gate);
        if observer.request_selected_shape(hwnd, cookie) {
            OK
        } else {
            observer.publish_native_failure(cookie, expected, STATUS_CAPTURE_FAILED);
            CAPTURE_FAILED
        }
    }))
    .unwrap_or(INVALID)
}

/// Requests that the app's normal cursor-selection path run after virtual motion.
/// It posts only to the selected target window; it never reads or moves the host cursor.
#[unsafe(no_mangle)]
pub extern "C" fn ProtoMuxCursorObserverMotion(selected_hwnd: u64) -> u32 {
    catch_unwind(AssertUnwindSafe(|| unsafe {
        let _gate = lifecycle_gate().lock().unwrap_or_else(|e| e.into_inner());
        let Ok(observer) = running() else {
            return DISABLED;
        };
        if observer.stopped.load(Ordering::Acquire)
            || !fits_native_handle(selected_hwnd)
            || selected_hwnd == 0
            || observer.selected_hwnd.load(Ordering::Acquire) != selected_hwnd
            || observer.association_epoch.load(Ordering::Acquire) == 0
        {
            return INVALID;
        }
        let mut pid = 0;
        if GetWindowThreadProcessId(selected_hwnd as HWND, &mut pid) == 0 || pid != observer.pid {
            return INVALID;
        }
        let cookie = observer.selected_cookie.load(Ordering::Acquire);
        let expected = observer.observed_request.snapshot().0;
        observer.native_retry_attempts.store(0, Ordering::Release);
        drop(_gate);
        if observer.request_selected_shape(selected_hwnd, cookie) {
            OK
        } else {
            observer.publish_native_failure(cookie, expected, STATUS_CAPTURE_FAILED);
            CAPTURE_FAILED
        }
    }))
    .unwrap_or(INVALID)
}

/// Called by ProtoInput's target-local cursor/visibility hook. It copies the
/// cursor while local, enqueues only the latest icon, and never waits on a lock.
#[unsafe(no_mangle)]
pub extern "C" fn ProtoMuxCursorObserverNotify(
    target_local_cursor: u64,
    visible: u32,
    selected_hwnd: u64,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        let Ok(observer) = running() else {
            return DISABLED;
        };
        unsafe { notify_cursor(observer, target_local_cursor, visible, selected_hwnd, None) }
    }))
    .unwrap_or(INVALID)
}

unsafe fn notify_cursor(
    observer: Arc<Observer>,
    target_local_cursor: u64,
    visible: u32,
    selected_hwnd: u64,
    native_context: Option<(u64, u64)>,
) -> u32 {
    unsafe {
        // Native post-handler sampling reserves its generation atomically with
        // lifecycle validation. Release this try-lock before capture or pending
        // work; no application handler, Notify reentry or copy runs under it.
        let native_gate = if native_context.is_some() {
            match lifecycle_gate().try_lock() {
                Ok(gate) => Some(gate),
                Err(_) => return DROPPED_BUSY,
            }
        } else {
            None
        };
        if observer.stopped.load(Ordering::Acquire)
            || visible > 1
            || !fits_native_handle(selected_hwnd)
            || selected_hwnd == 0
            || (target_local_cursor != 0 && !fits_native_handle(target_local_cursor))
            || (visible != 0 && target_local_cursor == 0)
            || observer.selected_hwnd.load(Ordering::Acquire) != selected_hwnd
        {
            return INVALID;
        }
        let association = observer.association_epoch.load(Ordering::Acquire);
        let cookie = observer.selected_cookie.load(Ordering::Acquire);
        if association == 0
            || native_context.is_some_and(|(expected_association, expected_sequence)| {
                expected_association != association
                    || expected_sequence != observer.observed_request.snapshot().0
            })
        {
            return INVALID;
        }
        let mut pid = 0;
        if GetWindowThreadProcessId(selected_hwnd as HWND, &mut pid) != GetCurrentThreadId()
            || pid != observer.pid
        {
            // An unrelated calling thread must not supersede the current image.
            return INVALID;
        }
        let status = if visible == 0 {
            STATUS_HIDDEN
        } else {
            STATUS_VALID
        };
        let sequence = if let Some((_, expected)) = native_context {
            let Some(sequence) = observer.observed_request.advance_if(expected, status) else {
                return INVALID;
            };
            sequence
        } else {
            observer.observed_request.advance(status)
        };
        drop(native_gate);
        if visible == 0 {
            match observer.pending.try_lock() {
                Ok(mut pending) => {
                    pending.take();
                }
                Err(_) => {
                    observer
                        .observed_request
                        .set_status(sequence, STATUS_HIDDEN);
                }
            }
            observer.wake.notify_one();
            return OK;
        }

        // Stock handles are classified before copying: CopyIcon creates a new
        // handle that cannot compare equal to a shared LoadCursorW handle.
        let stock_id = classify_stock(target_local_cursor as HICON);
        let icon = if stock_id.is_none() {
            let copied = CopyIcon(target_local_cursor as HICON);
            if copied.is_null() {
                observer
                    .observed_request
                    .set_status(sequence, STATUS_CAPTURE_FAILED);
                observer.wake.notify_one();
                return CAPTURE_FAILED;
            }
            Some(PendingIcon(copied))
        } else {
            None
        };
        let pending = Pending {
            icon,
            stock_id,
            hwnd: selected_hwnd,
            association,
            request_sequence: sequence,
        };
        match observer.pending.try_lock() {
            Ok(mut slot) => {
                // A later callback or reassociation may have superseded this
                // copy while CopyIcon ran. Never let an older notification win.
                if observer.stopped.load(Ordering::Acquire)
                    || observer.observed_request.snapshot().0 != sequence
                    || observer.association_epoch.load(Ordering::Acquire) != association
                    || observer.selected_hwnd.load(Ordering::Acquire) != selected_hwnd
                {
                    return INVALID;
                }
                *slot = Some(pending);
                observer.wake.notify_one();
                OK
            }
            Err(_) => {
                // This newer sequence still invalidates any older frame, even
                // though its pixels could not be queued without blocking.
                if let Some(expected) = pending_busy_retry_sequence(
                    &observer.observed_request,
                    sequence,
                    (cookie, selected_hwnd, association),
                    (
                        observer.selected_cookie.load(Ordering::Acquire),
                        observer.selected_hwnd.load(Ordering::Acquire),
                        observer.association_epoch.load(Ordering::Acquire),
                    ),
                    observer.stopped.load(Ordering::Acquire),
                ) {
                    // Resample the selected GUI thread asynchronously within the
                    // existing retry budget; never retain or replay this icon.
                    observer.schedule_native_retry(cookie, expected);
                }
                observer.wake.notify_one();
                DROPPED_BUSY
            }
        }
    }
}

/// Signals asynchronous teardown. The worker publishes STATUS_STOPPED when the
/// shared mutex is available; host handles remain mapped until it observes that.
#[unsafe(no_mangle)]
pub extern "C" fn ProtoMuxCursorObserverStop(attachment_epoch: u64) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        let _gate = lifecycle_gate().lock().unwrap_or_else(|e| e.into_inner());
        let Some(observer) = global().load_full() else {
            return DISABLED;
        };
        if observer.attachment != attachment_epoch {
            return INVALID;
        }
        observer.stopped.store(true, Ordering::Release);
        observer.detach_window_hook();
        observer.selected_hwnd.store(0, Ordering::Release);
        observer.association_epoch.store(0, Ordering::Release);
        observer.observed_request.advance(STATUS_STOPPED);
        if let Ok(mut pending) = observer.pending.try_lock() {
            pending.take();
        }
        observer.wake.notify_all();
        // No join here: callbacks and worker own Arc references and the worker
        // releases the final view/handles after publishing its stopped marker.
        OK
    }))
    .unwrap_or(INVALID)
}

/// Read-only retirement acknowledgement. Stop itself only schedules teardown.
/// A busy lifecycle gate or an old worker/callback still owning resources is
/// deliberately reported as not quiescent; the caller may retry within its budget.
#[unsafe(no_mangle)]
pub extern "C" fn ProtoMuxCursorObserverQuiescent() -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        let Ok(_gate) = lifecycle_gate().try_lock() else {
            return 0;
        };
        u32::from(global().load().is_none() && LIVE_OBSERVERS.load(Ordering::Acquire) == 0)
    }))
    .unwrap_or(0)
}

fn worker_loop(observer: Arc<Observer>) {
    let mut processed = 0u64;
    let mut retry: Option<Frame> = None;
    loop {
        observer.retry_native_wake();
        let wait_sequence = observer.observed_request.snapshot().0;
        let ((sequence, status), pending) = receive_latest(&observer.observed_request, || {
            let mut slot = observer.pending.lock().unwrap_or_else(|e| e.into_inner());
            if slot.is_none() && wait_sequence == processed && retry.is_none() {
                let (next, _) = observer
                    .wake
                    .wait_timeout(slot, Duration::from_millis(16))
                    .unwrap_or_else(|e| e.into_inner());
                slot = next;
            }
            if observer.stopped.load(Ordering::Acquire) {
                None
            } else {
                slot.take()
            }
        });

        if observer.stopped.load(Ordering::Acquire) {
            retry = Some(Frame::empty(sequence, 0, 0, STATUS_STOPPED));
        } else if retry
            .as_ref()
            .is_some_and(|frame| frame.sequence != sequence)
        {
            retry = None;
        }

        if observer.stopped.load(Ordering::Acquire) {
            if !observer.detach_window_hook() {
                thread::sleep(Duration::from_millis(16));
                continue;
            }
            // A host can observe STOPPED as soon as publication releases the
            // mapping mutex. Serialize publication and clearing the old global
            // with Start so that acknowledged restart cannot see the old slot.
            let acknowledged = {
                let _gate = lifecycle_gate().lock().unwrap_or_else(|e| e.into_inner());
                if let Some(frame) = retry.as_ref()
                    && publish_frame(&observer, frame)
                {
                    global().store(None);
                    true
                } else {
                    false
                }
            };
            if acknowledged {
                return;
            }
            thread::sleep(Duration::from_millis(16));
            continue;
        }

        if let Some(item) = pending.filter(|p| p.request_sequence == sequence) {
            if item.association != observer.association_epoch.load(Ordering::Acquire)
                || item.hwnd != observer.selected_hwnd.load(Ordering::Acquire)
            {
                observer
                    .observed_request
                    .set_status(sequence, STATUS_UNSUPPORTED);
                let hwnd = observer.selected_hwnd.load(Ordering::Acquire);
                let association = observer.association_epoch.load(Ordering::Acquire);
                retry = Some(Frame::empty(
                    sequence,
                    hwnd,
                    association,
                    STATUS_UNSUPPORTED,
                ));
            } else {
                retry = Some(if let Some(resource_id) = item.stock_id {
                    Frame::stock(sequence, item.hwnd, item.association, resource_id)
                } else if let Some(icon) = item.icon.as_ref()
                    && let Some(image) = rasterize_custom(icon.0)
                {
                    Frame::custom(sequence, item.hwnd, item.association, image)
                } else {
                    Frame::empty(sequence, item.hwnd, item.association, STATUS_UNSUPPORTED)
                });
            }
        } else if sequence != processed && retry.is_none() {
            retry = Some(Frame::empty(
                sequence,
                observer.selected_hwnd.load(Ordering::Acquire),
                observer.association_epoch.load(Ordering::Acquire),
                status,
            ));
        }

        if let Some(frame) = retry.as_ref() {
            if observer.observed_request.snapshot().0 != frame.sequence
                || observer.association_epoch.load(Ordering::Acquire) != frame.association
                || observer.selected_hwnd.load(Ordering::Acquire) != frame.hwnd
            {
                retry = None;
                continue;
            }
            if publish_frame(&observer, frame) {
                processed = frame.sequence;
                retry = None;
            }
            if retry.is_some() {
                thread::sleep(Duration::from_millis(16));
            }
        }
    }
}

// The request may change while receive waits, or while it takes a pending item.
// Pair that item with the post-wake clock, never a pre-wait generation.
fn receive_latest<T>(
    clock: &RequestClock,
    receive: impl FnOnce() -> Option<T>,
) -> ((u64, u32), Option<T>) {
    let pending = receive();
    (clock.snapshot(), pending)
}

#[derive(Clone)]
struct Image {
    width: u32,
    height: u32,
    hotspot_x: i32,
    hotspot_y: i32,
    bgra: Vec<u8>,
}
impl Image {
    fn valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.width <= MAX_DIMENSION
            && self.height <= MAX_DIMENSION
            && self.hotspot_x >= 0
            && self.hotspot_y >= 0
            && self.hotspot_x < self.width as i32
            && self.hotspot_y < self.height as i32
            && self.bgra.len() == (self.width * self.height * 4) as usize
            && self.bgra.len() <= MAX_PAYLOAD_BYTES
    }
}

fn classify_stock(icon: HICON) -> Option<u32> {
    let handles = STOCK_CURSORS.get_or_init(|| {
        std::array::from_fn(|index| unsafe {
            LoadCursorW(
                ptr::null_mut(),
                CURSOR_RESOURCES[index] as usize as *const u16,
            ) as usize
        })
    });
    CURSOR_RESOURCES
        .iter()
        .zip(handles)
        .find_map(|(resource, handle)| {
            (*handle != 0 && *handle == icon as usize).then_some(*resource)
        })
}

struct Dib {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
}
impl Dib {
    unsafe fn new(width: i32, height: i32, bgra: u32) -> Option<Self> {
        unsafe {
            let dc = CreateCompatibleDC(ptr::null_mut());
            if dc.is_null() {
                return None;
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: core::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    biSizeImage: 0,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                },
                bmiColors: [RGBQUAD {
                    rgbBlue: 0,
                    rgbGreen: 0,
                    rgbRed: 0,
                    rgbReserved: 0,
                }],
            };
            let mut bits = ptr::null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0);
            if bitmap.is_null() || bits.is_null() {
                if !bitmap.is_null() {
                    DeleteObject(bitmap);
                }
                DeleteDC(dc);
                return None;
            }
            let old = SelectObject(dc, bitmap);
            if old.is_null() {
                DeleteObject(bitmap);
                DeleteDC(dc);
                return None;
            }
            let pixels = bits.cast::<u32>();
            for i in 0..(width as usize * height as usize) {
                *pixels.add(i) = bgra;
            }
            Some(Self {
                dc,
                bitmap,
                old,
                bits: pixels,
            })
        }
    }
}
impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

unsafe fn bitmap_size(bitmap: HBITMAP) -> Option<(i32, i32)> {
    unsafe {
        if bitmap.is_null() {
            return None;
        }
        let mut value: BITMAP = core::mem::zeroed();
        let copied = GetObjectW(
            bitmap,
            core::mem::size_of::<BITMAP>() as i32,
            (&mut value as *mut BITMAP).cast(),
        );
        (copied == core::mem::size_of::<BITMAP>() as i32).then_some((value.bmWidth, value.bmHeight))
    }
}

/// Rasterize custom cursor copies off the hook. Dual-background compositing
/// admits ordinary alpha cursors while rejecting XOR/inverted channel behavior.
fn rasterize_custom(icon: HICON) -> Option<Image> {
    unsafe {
        let mut info: ICONINFO = core::mem::zeroed();
        if GetIconInfo(icon, &mut info) == 0 {
            return None;
        }
        struct IconBitmaps(HBITMAP, HBITMAP);
        impl Drop for IconBitmaps {
            fn drop(&mut self) {
                unsafe {
                    if !self.0.is_null() {
                        DeleteObject(self.0);
                    }
                    if !self.1.is_null() {
                        DeleteObject(self.1);
                    }
                }
            }
        }
        let _bitmaps = IconBitmaps(info.hbmColor, info.hbmMask);
        let (mut width, mut height) = if !info.hbmColor.is_null() {
            bitmap_size(info.hbmColor)?
        } else {
            let (mask_width, mask_height) = bitmap_size(info.hbmMask)?;
            (mask_width, mask_height / 2)
        };
        if width <= 0
            || height <= 0
            || width as u32 > MAX_DIMENSION
            || height as u32 > MAX_DIMENSION
        {
            return None;
        }
        width = width.min(MAX_DIMENSION as i32);
        height = height.min(MAX_DIMENSION as i32);
        let count = width as usize * height as usize;
        if count.checked_mul(4)? > MAX_PAYLOAD_BYTES {
            return None;
        }
        let black = Dib::new(width, height, 0x00000000)?;
        let white = Dib::new(width, height, 0x00ff_ffff)?;
        if DrawIconEx(
            black.dc,
            0,
            0,
            icon,
            width,
            height,
            0,
            ptr::null_mut(),
            DI_NORMAL,
        ) == 0
            || DrawIconEx(
                white.dc,
                0,
                0,
                icon,
                width,
                height,
                0,
                ptr::null_mut(),
                DI_NORMAL,
            ) == 0
        {
            return None;
        }
        let black_pixels = core::slice::from_raw_parts(black.bits, count);
        let white_pixels = core::slice::from_raw_parts(white.bits, count);
        let mut bgra = Vec::with_capacity(count * 4);
        for (&b, &w) in black_pixels.iter().zip(white_pixels) {
            let bb = [b as u8, (b >> 8) as u8, (b >> 16) as u8];
            let ww = [w as u8, (w >> 8) as u8, (w >> 16) as u8];
            let diffs = [
                ww[0] as i16 - bb[0] as i16,
                ww[1] as i16 - bb[1] as i16,
                ww[2] as i16 - bb[2] as i16,
            ];
            if diffs.iter().any(|d| !(0..=255).contains(d))
                || (diffs[0] - diffs[1]).abs() > 1
                || (diffs[1] - diffs[2]).abs() > 1
            {
                return None;
            }
            let alpha = 255 - *diffs.iter().min()? as u8;
            bgra.extend_from_slice(&[bb[0], bb[1], bb[2], alpha]);
        }
        let image = Image {
            width: width as u32,
            height: height as u32,
            hotspot_x: info.xHotspot as i32,
            hotspot_y: info.yHotspot as i32,
            bgra,
        };
        image.valid().then_some(image)
    }
}

struct Frame {
    sequence: u64,
    hwnd: u64,
    association: u64,
    status: u32,
    kind: u32,
    image: Option<Image>,
}
impl Frame {
    fn empty(sequence: u64, hwnd: u64, association: u64, status: u32) -> Self {
        Self {
            sequence,
            hwnd,
            association,
            // A notification can announce VALID before copying/queuing finishes.
            // Missing data is a fallback, never a malformed VALID/None record.
            status: if status == STATUS_VALID {
                STATUS_UNSUPPORTED
            } else {
                status
            },
            kind: KIND_NONE,
            image: None,
        }
    }
    fn stock(sequence: u64, hwnd: u64, association: u64, id: u32) -> Self {
        Self {
            sequence,
            hwnd,
            association,
            status: STATUS_VALID,
            kind: id,
            image: None,
        }
    }
    fn custom(sequence: u64, hwnd: u64, association: u64, image: Image) -> Self {
        Self {
            sequence,
            hwnd,
            association,
            status: STATUS_VALID,
            kind: KIND_BGRA8_PREMULTIPLIED,
            image: Some(image),
        }
    }
}

fn publish_frame(observer: &Observer, frame: &Frame) -> bool {
    unsafe {
        let wait = WaitForSingleObject(observer.mutex_handle, 0);
        if wait != WAIT_OBJECT_0 && wait != WAIT_ABANDONED {
            return false;
        }
        let header = &mut *(observer.mapped.cast::<SnapshotHeader>());
        if header.magic != cursor_snapshot_protocol::MAGIC
            || header.attachment_epoch != observer.attachment
            || observer.observed_request.snapshot().0 != frame.sequence
            || observer.association_epoch.load(Ordering::Acquire) != frame.association
            || observer.selected_hwnd.load(Ordering::Acquire) != frame.hwnd
            || observer.pid != header.target_pid
        {
            ReleaseMutex(observer.mutex_handle);
            return false;
        }
        header.sequence = header.sequence.wrapping_add(1);
        header.association_epoch = frame.association;
        header.selected_hwnd = frame.hwnd;
        header.status = frame.status;
        header.flags = u32::from(frame.status == STATUS_VALID);
        header.kind = frame.kind;
        header.width = 0;
        header.height = 0;
        header.stride = 0;
        header.format = 0;
        header.hotspot_x = 0;
        header.hotspot_y = 0;
        header.payload_len = 0;
        if let Some(image) = frame.image.as_ref() {
            header.width = image.width;
            header.height = image.height;
            header.stride = image.width * 4;
            header.format = 1;
            header.hotspot_x = image.hotspot_x;
            header.hotspot_y = image.hotspot_y;
            header.payload_len = image.bgra.len() as u32;
            let dst = observer.mapped.add(core::mem::size_of::<SnapshotHeader>());
            ptr::copy_nonoverlapping(image.bgra.as_ptr(), dst, image.bgra.len());
        }
        ReleaseMutex(observer.mutex_handle);
        true
    }
}

fn fits_native_handle(value: u64) -> bool {
    value <= usize::MAX as u64
}

fn allocate_wake_cookie() -> Option<usize> {
    NEXT_WAKE_COOKIE
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            value.checked_add(1)
        })
        .ok()
}
fn wake_message() -> u32 {
    *WAKE_MESSAGE.get_or_init(|| {
        let name: Vec<u16> = "ProtoInputMultiPlex.CursorObserver.Wake.V1"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        unsafe { RegisterWindowMessageW(name.as_ptr()) }
    })
}
impl Observer {
    fn detach_window_hook(&self) -> bool {
        self.selected_cookie.store(0, Ordering::Release);
        self.queued_cookie.store(0, Ordering::Release);
        self.native_retry_requested.store(false, Ordering::Release);
        self.native_retry_cookie.store(0, Ordering::Release);
        self.selected_thread.store(0, Ordering::Release);
        let mut hook = self.thread_hook.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = *hook {
            if unsafe { UnhookWindowsHookEx(handle as _) } == 0 {
                return false;
            }
            *hook = None;
        }
        true
    }
    fn publish_native_failure(&self, cookie: usize, expected: u64, status: u32) {
        if cookie != 0
            && self.selected_cookie.load(Ordering::Acquire) == cookie
            && !self.stopped.load(Ordering::Acquire)
            && self.observed_request.advance_if(expected, status).is_some()
        {
            self.wake.notify_one();
        }
    }
    fn schedule_native_retry(&self, cookie: usize, expected: u64) {
        self.native_retry_sequence
            .store(expected, Ordering::Release);
        self.native_retry_cookie.store(cookie, Ordering::Release);
        self.native_retry_requested.store(true, Ordering::Release);
        self.wake.notify_one();
    }
    fn retry_native_wake(&self) {
        if !self.native_retry_requested.swap(false, Ordering::AcqRel) {
            return;
        }
        let cookie = self.native_retry_cookie.load(Ordering::Acquire);
        let expected = self.native_retry_sequence.load(Ordering::Acquire);
        let current = || {
            cookie != 0
                && self.selected_cookie.load(Ordering::Acquire) == cookie
                && self.observed_request.snapshot().0 == expected
                && !self.stopped.load(Ordering::Acquire)
        };
        if !current() {
            return;
        }
        let attempt = self.native_retry_attempts.fetch_add(1, Ordering::AcqRel);
        if attempt >= MAX_NATIVE_RETRIES {
            self.publish_native_failure(cookie, expected, STATUS_CAPTURE_FAILED);
            return;
        }
        // Retry from the background worker, rather than spinning the target's
        // PeekMessage loop. At most one current wake and one deferred retry exist.
        thread::sleep(Duration::from_millis(16));
        if current()
            && !self.request_selected_shape(self.selected_hwnd.load(Ordering::Acquire), cookie)
        {
            self.publish_native_failure(cookie, expected, STATUS_CAPTURE_FAILED);
        }
    }
    fn request_selected_shape(&self, hwnd: u64, cookie: usize) -> bool {
        let message = wake_message();
        // Serialize validation and queue reservation with reassociation. Every
        // caller releases its lifecycle guard before entering this method.
        let gate = lifecycle_gate().lock().unwrap_or_else(|e| e.into_inner());
        if cookie == 0
            || message == 0
            || hwnd == 0
            || self.stopped.load(Ordering::Acquire)
            || self.selected_cookie.load(Ordering::Acquire) != cookie
            || self.selected_hwnd.load(Ordering::Acquire) != hwnd
            || self.association_epoch.load(Ordering::Acquire) == 0
            || self.selected_thread.load(Ordering::Acquire) == 0
        {
            return false;
        }
        let mut pid = 0;
        let actual_thread = unsafe { GetWindowThreadProcessId(hwnd as HWND, &mut pid) };
        if actual_thread == 0
            || actual_thread != self.selected_thread.load(Ordering::Acquire)
            || pid != self.pid
            || self.selected_cookie.load(Ordering::Acquire) != cookie
        {
            return false;
        }
        match admit_current_wake(
            &self.queued_cookie,
            cookie,
            self.selected_cookie.load(Ordering::Acquire),
        ) {
            Some(WakeAdmission::Coalesced) => return true,
            Some(WakeAdmission::Post) => {}
            None => return false,
        }
        // An old wake can be posted after a later reassociation, but its cookie
        // cannot reserve or clear that association's queue slot. Never run an
        // app callback while retaining the lifecycle guard.
        drop(gate);
        let posted = unsafe { PostMessageW(hwnd as HWND, message, cookie, 0) } != 0;
        if !posted {
            let _ =
                self.queued_cookie
                    .compare_exchange(cookie, 0, Ordering::AcqRel, Ordering::Acquire);
        }
        posted
    }
}

#[derive(Debug, PartialEq, Eq)]
enum WakeAdmission {
    Post,
    Coalesced,
}
fn admit_current_wake(
    queued: &AtomicUsize,
    requested: usize,
    current: usize,
) -> Option<WakeAdmission> {
    if requested == 0 || requested != current {
        return None;
    }
    // Called only while lifecycle validation is serialized. A stale different
    // cookie is replaced, not mistaken for a pending wake of this association.
    Some(if queued.swap(requested, Ordering::AcqRel) == requested {
        WakeAdmission::Coalesced
    } else {
        WakeAdmission::Post
    })
}

fn remove_current_wake(
    queued: &AtomicUsize,
    expected: usize,
    received: usize,
    removed: bool,
) -> bool {
    removed
        && expected != 0
        && expected == received
        && queued
            .compare_exchange(received, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
}
fn should_retry_native(result: u32, before: u64, after: u64, same_context: bool) -> bool {
    result == DROPPED_BUSY && before == after && same_context
}
fn pending_busy_retry_sequence(
    clock: &RequestClock,
    sequence: u64,
    captured_context: (usize, u64, u64),
    current_context: (usize, u64, u64),
    stopped: bool,
) -> Option<u64> {
    clock.set_status(sequence, STATUS_UNSUPPORTED);
    // Unlike lifecycle-gate contention, pending contention occurs after this
    // notification has advanced the clock. Retry its own generation only: a
    // newer cursor, hidden notification, reassociation or Stop must win.
    (captured_context.0 != 0
        && captured_context == current_context
        && !stopped
        && clock.snapshot() == (sequence, STATUS_UNSUPPORTED))
        .then_some(sequence)
}
fn may_sample_native_cursor(before: u64, after: u64, same_context: bool) -> bool {
    before == after && same_context
}

unsafe extern "system" fn cursor_message_hook(code: i32, removed: usize, message: isize) -> isize {
    if code == HC_ACTION as i32 && message != 0 {
        // A panic may not unwind through USER32. The next hook is called even
        // after a rejected, stale, peeked or panic-interrupted private message.
        let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
            process_cursor_wake(removed, &mut *(message as *mut MSG));
        }));
    }
    unsafe { CallNextHookEx(ptr::null_mut(), code, removed, message) }
}
unsafe fn process_cursor_wake(removed: usize, message: &mut MSG) {
    let private_message = wake_message();
    if private_message == 0 || message.message != private_message {
        return;
    }
    let cookie = message.wParam;
    // Only cookies issued by this loaded module identify our private wake.
    if cookie == 0 || cookie >= NEXT_WAKE_COOKIE.load(Ordering::Acquire) {
        return;
    }
    let hwnd = message.hwnd as usize as u64;
    message.message = 0; // WM_NULL: suppress only our returned message copy.
    message.hwnd = ptr::null_mut();
    message.wParam = 0;
    message.lParam = 0;
    let Ok(observer) = running() else {
        return;
    };
    if !remove_current_wake(
        &observer.queued_cookie,
        observer.selected_cookie.load(Ordering::Acquire),
        cookie,
        removed == PM_REMOVE as usize,
    ) {
        return;
    }
    let association = observer.association_epoch.load(Ordering::Acquire);
    let attachment = observer.attachment;
    let target_thread = observer.selected_thread.load(Ordering::Acquire);
    let same_context = || {
        let mut pid = 0;
        !observer.stopped.load(Ordering::Acquire)
            && observer.attachment == attachment
            && observer.association_epoch.load(Ordering::Acquire) == association
            && observer.selected_hwnd.load(Ordering::Acquire) == hwnd
            && observer.selected_cookie.load(Ordering::Acquire) == cookie
            && target_thread != 0
            && target_thread == unsafe { GetCurrentThreadId() }
            && unsafe { GetWindowThreadProcessId(hwnd as HWND, &mut pid) } == target_thread
            && pid == observer.pid
    };
    if association == 0 || !same_context() {
        return;
    }
    let before = observer.observed_request.snapshot().0;
    // This runs on the selected app GUI thread. Do not hold lifecycle or pending
    // locks while calling its normal handler: it may call intercepted SetCursor.
    unsafe {
        SendMessageW(
            hwnd as HWND,
            WM_SETCURSOR,
            hwnd as usize,
            HTCLIENT as isize | ((WM_MOUSEMOVE as isize) << 16),
        );
    }
    // WM_SETCURSOR's result controls further message processing. A native EDIT
    // can return zero after selecting its I-beam, so it is not capture eligibility.
    // Intercepted cursor/visibility notifications still take precedence.
    let after = observer.observed_request.snapshot().0;
    if !may_sample_native_cursor(before, after, same_context()) {
        return;
    }
    // GetCursor is target-thread state after its own native handler. Never use
    // global GetCursorInfo, host cursor position or a class-cursor guess here.
    let cursor = unsafe { GetCursor() };
    let result = unsafe {
        notify_cursor(
            Arc::clone(&observer),
            cursor as usize as u64,
            u32::from(!cursor.is_null()),
            hwnd,
            Some((association, before)),
        )
    };
    if should_retry_native(
        result,
        before,
        observer.observed_request.snapshot().0,
        same_context(),
    ) {
        observer.schedule_native_retry(cookie, before);
    }
}

unsafe fn close_consumed(mapping: u64, mutex: u64) {
    unsafe {
        // On a narrower target, never truncate an invalid u64 into a different
        // potentially valid handle. Equal representable inputs are closed once.
        if mapping != 0 && fits_native_handle(mapping) {
            CloseHandle(mapping as HANDLE);
        }
        if mutex != 0 && mutex != mapping && fits_native_handle(mutex) {
            CloseHandle(mutex as HANDLE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_arriving_during_receive_uses_post_wake_generation() {
        let clock = RequestClock::new(STATUS_EMPTY);
        let ((sequence, status), pending) = receive_latest(&clock, || {
            let arrived = clock.advance(STATUS_VALID);
            Some(Pending {
                stock_id: Some(32649),
                icon: None,
                hwnd: 12,
                association: 7,
                request_sequence: arrived,
            })
        });
        let pending = pending.expect("latest stock notification");
        assert_eq!(sequence, pending.request_sequence);
        assert_eq!(status, STATUS_VALID);
        let frame = Frame::stock(
            sequence,
            pending.hwnd,
            pending.association,
            pending.stock_id.unwrap(),
        );
        assert_eq!(frame.kind, 32649);
        assert_eq!(frame.status, STATUS_VALID);
    }

    #[test]
    fn incomplete_capture_is_fallback_instead_of_valid_without_payload() {
        let frame = Frame::empty(1, 12, 7, STATUS_VALID);
        assert_eq!(frame.status, STATUS_UNSUPPORTED);
        let mut header = SnapshotHeader::empty(123, 9);
        header.sequence = frame.sequence;
        header.selected_hwnd = frame.hwnd;
        header.association_epoch = frame.association;
        header.status = frame.status;
        header.kind = frame.kind;
        assert!(header.validate(MAPPING_BYTES, 123, 9));
    }
    #[test]
    fn private_wake_coalesces_peeks_and_rejects_old_association_cookie() {
        let queued = AtomicUsize::new(11);
        assert!(!remove_current_wake(&queued, 11, 11, false));
        assert_eq!(queued.load(Ordering::Acquire), 11);
        assert!(!remove_current_wake(&queued, 12, 11, true));
        assert_eq!(queued.load(Ordering::Acquire), 11);
        assert!(remove_current_wake(&queued, 11, 11, true));
        assert!(!remove_current_wake(&queued, 11, 11, true));
    }
    #[test]
    fn native_sample_eligibility_uses_generation_and_context_only() {
        assert!(may_sample_native_cursor(8, 8, true));
        for status in [
            STATUS_VALID,
            STATUS_HIDDEN,
            STATUS_CAPTURE_FAILED,
            STATUS_UNSUPPORTED,
        ] {
            let clock = RequestClock::new(STATUS_EMPTY);
            let before = clock.snapshot().0;
            clock.advance(status);
            assert!(!may_sample_native_cursor(before, clock.snapshot().0, true));
        }
        assert!(!may_sample_native_cursor(8, 9, true));
        assert!(!may_sample_native_cursor(8, 8, false));
    }
    #[test]
    fn native_busy_retry_is_only_for_unchanged_current_request() {
        assert!(should_retry_native(DROPPED_BUSY, 8, 8, true));
        assert!(!should_retry_native(DROPPED_BUSY, 8, 9, true));
        assert!(!should_retry_native(DROPPED_BUSY, 8, 8, false));
        assert!(!should_retry_native(OK, 8, 8, true));
        let clock = RequestClock::new(STATUS_EMPTY);
        let before = clock.snapshot().0;
        let newer = clock.advance(STATUS_VALID);
        assert_eq!(clock.advance_if(before, STATUS_UNSUPPORTED), None);
        assert_eq!(clock.snapshot(), (newer, STATUS_VALID));
    }
    #[test]
    fn pending_busy_retry_uses_its_own_already_advanced_generation() {
        let clock = RequestClock::new(STATUS_EMPTY);
        let before = clock.snapshot().0;
        let sequence = clock.advance(STATUS_VALID);
        let context = (11, 12, 7);
        // The old native gate correctly rejects a changed request. Pending
        // contention needs a separate retry for the notification that owns it.
        assert!(!should_retry_native(DROPPED_BUSY, before, sequence, true));
        assert_eq!(
            pending_busy_retry_sequence(&clock, sequence, context, context, false),
            Some(sequence)
        );
        assert_eq!(clock.snapshot(), (sequence, STATUS_UNSUPPORTED));
    }
    #[test]
    fn pending_busy_retry_preserves_newer_notifications_and_lifecycle_changes() {
        let context = (11, 12, 7);
        for status in [STATUS_VALID, STATUS_HIDDEN, STATUS_STOPPED] {
            let clock = RequestClock::new(STATUS_EMPTY);
            let old = clock.advance(STATUS_VALID);
            let newer = clock.advance(status);
            assert_eq!(
                pending_busy_retry_sequence(&clock, old, context, context, false),
                None
            );
            assert_eq!(clock.snapshot(), (newer, status));
        }
        for current in [(12, 12, 7), (11, 13, 7), (11, 12, 8), (0, 12, 7)] {
            let clock = RequestClock::new(STATUS_EMPTY);
            let sequence = clock.advance(STATUS_VALID);
            assert_eq!(
                pending_busy_retry_sequence(&clock, sequence, context, current, false),
                None
            );
        }
        let clock = RequestClock::new(STATUS_EMPTY);
        let sequence = clock.advance(STATUS_VALID);
        assert_eq!(
            pending_busy_retry_sequence(&clock, sequence, context, context, true),
            None
        );
        assert_eq!(
            pending_busy_retry_sequence(&clock, sequence, (0, 12, 7), (0, 12, 7), false),
            None
        );
    }
    #[test]
    fn reassociation_rejects_late_old_reservation_and_repairs_stale_slot() {
        let queued = AtomicUsize::new(0);
        assert_eq!(
            admit_current_wake(&queued, 11, 11),
            Some(WakeAdmission::Post)
        );
        // Reassociation clears the old slot before admission runs under the
        // new lifecycle guard. A previously started old request must not poison it.
        queued.store(0, Ordering::Release);
        assert_eq!(admit_current_wake(&queued, 11, 12), None);
        assert_eq!(queued.load(Ordering::Acquire), 0);
        assert_eq!(
            admit_current_wake(&queued, 12, 12),
            Some(WakeAdmission::Post)
        );
        assert_eq!(
            admit_current_wake(&queued, 12, 12),
            Some(WakeAdmission::Coalesced)
        );
        assert!(!remove_current_wake(&queued, 12, 11, true));
        assert_eq!(queued.load(Ordering::Acquire), 12);
        // Defensive admission also replaces an already stale cookie instead of
        // falsely reporting that the new association already has a queued wake.
        queued.store(11, Ordering::Release);
        assert_eq!(
            admit_current_wake(&queued, 12, 12),
            Some(WakeAdmission::Post)
        );
        assert_eq!(queued.load(Ordering::Acquire), 12);
    }
}
