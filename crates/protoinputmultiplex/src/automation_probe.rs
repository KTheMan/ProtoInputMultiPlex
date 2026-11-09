//! Explicit opt-in disposable Unicode EDIT target. Records only its test window's input.
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, GetKeyState, GetKeyboardLayout, GetKeyboardState, SetFocus,
            VK_CONTROL, VK_MENU, VK_SHIFT,
        },
        WindowsAndMessaging::*,
    },
};
#[derive(Default, Serialize)]
struct Snapshot {
    cursor_selection_count: u64,
    cursor_selection_events: Vec<CursorSelectionEvent>,
    cursor_fixture_kind: Option<u32>,
    keyboard_focus_fixture: Option<KeyboardFocusFixture>,
    pid: u32,
    cursor: (i32, i32),
    buttons: u8,
    test_keys: u8,
    frames: u64,
    text: String,
    held_keys: Vec<u32>,
    key_events: Vec<KeyEvent>,
    key_event_count: u64,
    root_key_events: Vec<RootKeyEvent>,
    root_key_event_count: u64,
    text_events: u64,
    mouse_events: Vec<MouseEvent>,
    mouse_event_count: u64,
    wheel_events: u64,
    wheel_delta: i64,
    horizontal_wheel_events: u64,
    horizontal_wheel_delta: i64,
    capture_color: Option<[u8; 3]>,
    capture_delay_ms: u32,
    capture_hwnd: usize,
    wm_print_count: u64,
    wm_printclient_count: u64,
    wm_paint_count: u64,
    last_print_flags: u32,
    last_print_hdc_non_null: bool,
    capture_delay_applied_count: u64,
    capture_delay_total_ms: u64,
    native_events: Vec<NativeEvent>,
    native_event_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    accelerator: Option<AcceleratorDiagnostic>,
    clock_us: u64,
}
#[derive(Serialize)]
struct KeyboardFocusFixture {
    root: usize,
    edits: [usize; 2],
    thread_id: u32,
    foreground_before_seed: usize,
    foreground_after_seed: usize,
    second_edit_focus_policy: &'static str,
    seeded: NativeKeyboardFocus,
    current: NativeKeyboardFocus,
    edit_text: [String; 2],
}
#[derive(Default, Serialize)]
struct NativeKeyboardFocus {
    available: bool,
    active: usize,
    focused: usize,
    capture: usize,
    caret: usize,
    caret_rect: [i32; 4],
}
fn keyboard_focus(thread_id: u32) -> NativeKeyboardFocus {
    let mut info: GUITHREADINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
    let available = thread_id != 0 && unsafe { GetGUIThreadInfo(thread_id, &mut info) } != 0;
    NativeKeyboardFocus {
        available,
        active: info.hwndActive as usize,
        focused: info.hwndFocus as usize,
        capture: info.hwndCapture as usize,
        caret: info.hwndCaret as usize,
        caret_rect: [
            info.rcCaret.left,
            info.rcCaret.top,
            info.rcCaret.right,
            info.rcCaret.bottom,
        ],
    }
}
fn edit_text(hwnd: HWND) -> String {
    let mut text = vec![0u16; 32769];
    let length = unsafe { GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32) };
    String::from_utf16_lossy(&text[..length.max(0) as usize])
}
#[derive(Serialize)]
struct CursorSelectionEvent {
    hwnd: usize,
    thread_id: u32,
    hit_test: i16,
    trigger: u16,
    native_result: isize,
    before_kind: u32,
    after_kind: u32,
    after_resource: CursorResourceInfo,
}
#[derive(Serialize)]
struct CursorResourceInfo {
    success: bool,
    resource_id: u16,
    module: String,
    resource: String,
}
// GetIconInfoEx owns its returned bitmaps even when a partial failure occurs.
struct CursorInfoBitmaps(ICONINFOEXW);
impl Drop for CursorInfoBitmaps {
    fn drop(&mut self) {
        unsafe {
            if !self.0.hbmMask.is_null() {
                DeleteObject(self.0.hbmMask);
            }
            if !self.0.hbmColor.is_null() && self.0.hbmColor != self.0.hbmMask {
                DeleteObject(self.0.hbmColor);
            }
        }
    }
}
fn current_cursor_kind(cursor: HCURSOR) -> u32 {
    if cursor.is_null() {
        return 0;
    }
    const STOCK_IDS: [u32; 14] = [
        32512, 32513, 32514, 32515, 32516, 32642, 32643, 32644, 32645, 32646, 32648, 32649, 32650,
        32651,
    ];
    STOCK_IDS
        .into_iter()
        .find(|id| unsafe {
            LoadCursorW(std::ptr::null_mut(), *id as usize as *const u16) == cursor
        })
        .unwrap_or(u32::MAX)
}
fn cursor_resource(cursor: HCURSOR) -> CursorResourceInfo {
    let mut info = CursorInfoBitmaps(unsafe { std::mem::zeroed() });
    info.0.cbSize = std::mem::size_of::<ICONINFOEXW>() as u32;
    let success = !cursor.is_null() && unsafe { GetIconInfoExW(cursor, &mut info.0) } != 0;
    let bounded_wide = |value: &[u16]| {
        let length = value
            .iter()
            .take(256)
            .position(|unit| *unit == 0)
            .unwrap_or(value.len().min(256));
        String::from_utf16_lossy(&value[..length])
    };
    CursorResourceInfo {
        success,
        resource_id: if success { info.0.wResID } else { 0 },
        module: if success {
            bounded_wide(&info.0.szModName)
        } else {
            String::new()
        },
        resource: if success {
            bounded_wide(&info.0.szResName)
        } else {
            String::new()
        },
    }
}

#[derive(Serialize)]
struct MouseEvent {
    button: u32,
    pressed: bool,
}
#[derive(Serialize)]
struct KeyEvent {
    vk: u32,
    pressed: bool,
    scan: u32,
    extended: bool,
    alt: bool,
}
impl KeyEvent {
    fn from_message(message: u32, wparam: usize, lparam: isize) -> Self {
        Self {
            vk: wparam as u32,
            pressed: matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN),
            scan: ((lparam >> 16) & 255) as u32,
            extended: lparam & (1 << 24) != 0,
            alt: lparam & (1 << 29) != 0,
        }
    }
}
#[derive(Serialize)]
struct RootKeyEvent {
    hwnd: usize,
    message: u32,
    #[serde(flatten)]
    key: KeyEvent,
}
impl Snapshot {
    fn root_key(&mut self, hwnd: usize, message: u32, wparam: usize, lparam: isize) {
        if !matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP) {
            return;
        }
        self.root_key_event_count = self.root_key_event_count.saturating_add(1);
        if self.root_key_events.len() >= 2048 {
            self.root_key_events.remove(0);
        }
        self.root_key_events.push(RootKeyEvent {
            hwnd,
            message,
            key: KeyEvent::from_message(message, wparam, lparam),
        });
    }
}
#[derive(Serialize)]
struct NativeEvent {
    hwnd: usize,
    kind: String,
    at_us: u64,
    code: u32,
    pressed: Option<bool>,
    client: Option<(i32, i32)>,
    delta: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wheel_screen: Option<(i32, i32)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wheel_modifiers: Option<u16>,
}
#[derive(Default, Serialize)]
struct AcceleratorDiagnostic {
    enabled: bool,
    ready: bool,
    command_count: u64,
    last_command_id: Option<u16>,
    last_command_notification_code: Option<u16>,
    last_command_hwnd: Option<usize>,
    last_command_source_hwnd: Option<usize>,
    journal: Vec<AcceleratorMessage>,
    journal_overflow: bool,
    keyboard_state_fixture: bool,
    keyboard_layout: usize,
}
#[derive(Serialize)]
struct AcceleratorMessage {
    hwnd: usize,
    message: u32,
    vk: u32,
    control_down: bool,
    shift_down: bool,
    alt_down: bool,
    translate_accelerator_consumed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    keyboard_state: Option<KeyboardTableSample>,
    #[serde(skip_serializing_if = "Option::is_none")]
    peek: Option<KeyboardPeekSample>,
}
#[derive(Serialize)]
struct KeyboardTableSample {
    available: bool,
    table: Vec<u8>,
}
fn keyboard_table_sample() -> KeyboardTableSample {
    let mut table = vec![0; 256];
    let available = unsafe { GetKeyboardState(table.as_mut_ptr()) } != 0;
    KeyboardTableSample { available, table }
}
#[derive(Serialize, PartialEq, Eq)]
struct QueueMessageIdentity {
    hwnd: usize,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
}
impl From<&MSG> for QueueMessageIdentity {
    fn from(msg: &MSG) -> Self {
        Self {
            hwnd: msg.hwnd as usize,
            message: msg.message,
            wparam: msg.wParam,
            lparam: msg.lParam,
            time: msg.time,
        }
    }
}
#[derive(Serialize)]
struct KeyboardPeekSample {
    before: KeyboardTableSample,
    after_first: KeyboardTableSample,
    after_second: KeyboardTableSample,
    first: QueueMessageIdentity,
    second: Option<QueueMessageIdentity>,
    removed: Option<QueueMessageIdentity>,
}
static SNAPSHOT: OnceLock<Mutex<Snapshot>> = OnceLock::new();
static OLD_PROC: OnceLock<usize> = OnceLock::new();
static CAPTURE_COLOR: OnceLock<Mutex<[u8; 3]>> = OnceLock::new();
static CAPTURE_DELAY_MS: AtomicU32 = AtomicU32::new(0);
static CAPTURE_HWND: OnceLock<usize> = OnceLock::new();
static TRACE_EPOCH: OnceLock<Instant> = OnceLock::new();
static CURSOR_FIXTURE: OnceLock<usize> = OnceLock::new();
static KEYBOARD_CLICK_SURFACE: OnceLock<usize> = OnceLock::new();
static NATIVE_CONTROLS_FIXTURE: OnceLock<()> = OnceLock::new();

const MAX_NATIVE_EVENTS: usize = 4096;
const MAX_ACCELERATOR_MESSAGES: usize = 64;
const ACCELERATOR_COMMAND_ID: u16 = 0x7A51;

const WM_SET_CAPTURE_COLOR: u32 = 0x8051;
const WM_SET_CAPTURE_DELAY: u32 = 0x8052;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

struct FixtureCursor(HICON);
impl Drop for FixtureCursor {
    fn drop(&mut self) {
        unsafe {
            DestroyIcon(self.0);
        }
    }
}

fn create_fixture_cursor() -> Result<FixtureCursor> {
    unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: 16,
                biHeight: -16,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            },
            ..std::mem::zeroed()
        };
        let mut bits = std::ptr::null_mut();
        let color = CreateDIBSection(
            std::ptr::null_mut(),
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );
        ensure!(!color.is_null(), "create fixture cursor color bitmap");
        let pixels = std::slice::from_raw_parts_mut(bits as *mut u8, 16 * 16 * 4);
        pixels.fill(0);
        for y in 1..15 {
            for x in 1..15 {
                let pixel = &mut pixels[(y * 16 + x) * 4..][..4];
                if x < 4 && y < 4 {
                    pixel.copy_from_slice(&[255, 255, 255, 255]);
                } else if x == y || x == 15 - y || (4..12).contains(&x) && (4..12).contains(&y) {
                    pixel.copy_from_slice(&[24, 120, 180, 192]);
                }
            }
        }
        let mask_bits = [0u8; 32];
        let mask = CreateBitmap(16, 16, 1, 1, mask_bits.as_ptr().cast());
        if mask.is_null() {
            DeleteObject(color);
            anyhow::bail!("create fixture cursor mask");
        }
        let icon = CreateIconIndirect(&ICONINFO {
            fIcon: 0,
            xHotspot: 3,
            yHotspot: 5,
            hbmMask: mask,
            hbmColor: color,
        });
        DeleteObject(mask);
        DeleteObject(color);
        ensure!(!icon.is_null(), "create fixture custom cursor");
        Ok(FixtureCursor(icon))
    }
}

fn fixture_cursor_message(hwnd: HWND, message: u32, l: LPARAM) -> Option<LRESULT> {
    let custom = CURSOR_FIXTURE.get()?;
    if !matches!(message, WM_MOUSEMOVE | WM_SETCURSOR) {
        return None;
    }
    let (x, y) = client_point(l);
    // Regions use the owned top-level client's coordinates even when routing
    // correctly selects the EDIT child. No interactive-desktop cursor is read.
    let mut point = POINT { x, y };
    if message == WM_MOUSEMOVE {
        unsafe {
            ClientToScreen(hwnd, &mut point);
            ScreenToClient(GetAncestor(hwnd, GA_ROOT), &mut point);
        }
    }
    let (x, y) = (point.x, point.y);
    let kind = if message == WM_SETCURSOR {
        SNAPSHOT
            .get()
            .and_then(|snapshot| snapshot.lock().unwrap().cursor_fixture_kind)
            .unwrap_or(32512)
    } else if y >= 220 {
        0
    } else if x < 80 {
        32649
    } else if x < 160 {
        32513
    } else if x < 240 {
        32514
    } else if x < 320 {
        2
    } else {
        32644
    };
    unsafe {
        let cursor = match kind {
            0 => std::ptr::null_mut(),
            2 => *custom as HCURSOR,
            _ => LoadCursorW(std::ptr::null_mut(), kind as usize as *const u16),
        };
        SetCursor(cursor);
    }
    if let Some(snapshot) = SNAPSHOT.get() {
        snapshot.lock().unwrap().cursor_fixture_kind = Some(kind);
    }
    Some(if message == WM_SETCURSOR { 1 } else { 0 })
}

fn capture_color() -> Option<[u8; 3]> {
    CAPTURE_COLOR.get().map(|color| *color.lock().unwrap())
}

fn handle_capture_message(message: u32, value: WPARAM) -> Option<LRESULT> {
    // The fixture controls exist only when launched with --capture-pattern.
    CAPTURE_COLOR.get()?;
    match message {
        WM_SET_CAPTURE_COLOR => {
            if value > 0xFFFFFF {
                return Some(0);
            }
            let rgb = value as u32;
            let color = [
                ((rgb >> 16) & 255) as u8,
                ((rgb >> 8) & 255) as u8,
                (rgb & 255) as u8,
            ];
            *CAPTURE_COLOR.get().unwrap().lock().unwrap() = color;
            if let Some(snapshot) = SNAPSHOT.get() {
                snapshot.lock().unwrap().capture_color = Some(color);
            }
            let child = CAPTURE_HWND.get().copied().unwrap_or_default() as HWND;
            if !child.is_null() {
                unsafe { InvalidateRect(child, std::ptr::null(), 1) };
            }
            Some(0)
        }
        WM_SET_CAPTURE_DELAY => {
            if value > 5000 {
                return Some(0);
            }
            let delay_ms = value as u32;
            CAPTURE_DELAY_MS.store(delay_ms, Ordering::Relaxed);
            if let Some(snapshot) = SNAPSHOT.get() {
                snapshot.lock().unwrap().capture_delay_ms = delay_ms;
            }
            Some(0)
        }
        _ => None,
    }
}

unsafe extern "system" fn root_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if matches!(
        m,
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP | WM_CHAR
    ) {
        record_native_event(h, m, w, l);
        if let Some(snapshot) = SNAPSHOT.get() {
            snapshot.lock().unwrap().root_key(h as usize, m, w, l);
        }
    }
    if let Some(result) = fixture_cursor_message(h, m, l) {
        return result;
    }
    if let Some(result) = handle_capture_message(m, w) {
        return result;
    }
    if m == WM_COMMAND
        && w as u16 == ACCELERATOR_COMMAND_ID
        && let Some(snapshot) = SNAPSHOT.get()
    {
        let mut snapshot = snapshot.lock().unwrap();
        if let Some(accelerator) = &mut snapshot.accelerator {
            accelerator.command_count = accelerator.command_count.saturating_add(1);
            accelerator.last_command_id = Some(w as u16);
            accelerator.last_command_notification_code = Some((w >> 16) as u16);
            accelerator.last_command_hwnd = Some(h as usize);
            accelerator.last_command_source_hwnd = Some(l as usize);
            return 0;
        }
    }
    if m == WM_DESTROY {
        unsafe { PostQuitMessage(0) };
        return 0;
    }
    unsafe { DefWindowProcW(h, m, w, l) }
}
unsafe extern "system" fn edit_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if let Some(result) = handle_capture_message(m, w) {
        return result;
    }
    record_native_event(h, m, w, l);
    // Explicit opt-in focus fixture only: this real EDIT accepts keys/text but
    // clicking it must leave the initially focused EDIT and its caret intact.
    // ProtoInput's synthetic focus notices must not make EDIT create a new caret.
    // Native GetGUIThreadInfo remains the authority; no focus API runs here.
    if KEYBOARD_CLICK_SURFACE.get().copied() == Some(h as usize) {
        match m {
            WM_MOUSEACTIVATE => return MA_NOACTIVATE as LRESULT,
            WM_ACTIVATE | WM_SETFOCUS | WM_LBUTTONDOWN | WM_LBUTTONUP => return 0,
            _ => {}
        }
    }
    if let Some(result) = fixture_cursor_message(h, m, l) {
        return result;
    }
    if CAPTURE_COLOR.get().is_some() {
        if m == WM_PRINT || m == WM_PRINTCLIENT {
            if let Some(snapshot) = SNAPSHOT.get() {
                let mut snapshot = snapshot.lock().unwrap();
                if m == WM_PRINT {
                    snapshot.wm_print_count += 1;
                    snapshot.last_print_flags = l as u32;
                    snapshot.last_print_hdc_non_null = !(w as HDC).is_null();
                } else {
                    snapshot.wm_printclient_count += 1;
                }
            }
            let delay_ms = CAPTURE_DELAY_MS.load(Ordering::Relaxed);
            if delay_ms > 0 {
                std::thread::sleep(Duration::from_millis(u64::from(delay_ms)));
                if let Some(snapshot) = SNAPSHOT.get() {
                    let mut snapshot = snapshot.lock().unwrap();
                    snapshot.capture_delay_applied_count += 1;
                    snapshot.capture_delay_total_ms += u64::from(delay_ms);
                }
            }
            let dc = w as HDC;
            if !dc.is_null() {
                unsafe { paint_capture_pattern(h, dc) };
            }
            return 0;
        }
        if m == WM_PAINT {
            if let Some(snapshot) = SNAPSHOT.get() {
                snapshot.lock().unwrap().wm_paint_count += 1;
            }
            unsafe {
                let mut paint: PAINTSTRUCT = std::mem::zeroed();
                let dc = BeginPaint(h, &mut paint);
                paint_capture_pattern(h, dc);
                EndPaint(h, &paint);
            }
            return 0;
        }
    }
    if let Some(snapshot) = SNAPSHOT.get() {
        let mut snapshot = snapshot.lock().unwrap();
        if matches!(m, WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP) {
            snapshot.key_event_count += 1;
            if snapshot.key_events.len() >= 2048 {
                snapshot.key_events.remove(0);
            }
            snapshot.key_events.push(KeyEvent::from_message(m, w, l));
        }
        if m == WM_CHAR {
            snapshot.text_events += 1;
        }
        let button = match m {
            WM_LBUTTONDOWN | WM_LBUTTONUP => Some(0),
            WM_RBUTTONDOWN | WM_RBUTTONUP => Some(1),
            WM_MBUTTONDOWN | WM_MBUTTONUP => Some(2),
            WM_XBUTTONDOWN | WM_XBUTTONUP => Some(if (w >> 16) & 0xFFFF == 1 { 3 } else { 4 }),
            _ => None,
        };
        if let Some(button) = button {
            snapshot.mouse_event_count += 1;
            if snapshot.mouse_events.len() >= 2048 {
                snapshot.mouse_events.remove(0);
            }
            snapshot.mouse_events.push(MouseEvent {
                button,
                pressed: matches!(
                    m,
                    WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
                ),
            });
        }
        if m == WM_MOUSEWHEEL {
            snapshot.wheel_events += 1;
            snapshot.wheel_delta += i64::from((w >> 16) as u16 as i16);
        }
        if m == WM_MOUSEHWHEEL {
            snapshot.horizontal_wheel_events += 1;
            snapshot.horizontal_wheel_delta += i64::from((w >> 16) as u16 as i16);
        }
    }
    // This opt-in EDIT fixture records native non-primary button edges above,
    // then prevents modal tracking/autoscroll from blocking report sampling.
    // Keyboard/text processing and backend held-state sampling remain native.
    if NATIVE_CONTROLS_FIXTURE.get().is_none()
        && matches!(
            m,
            WM_CONTEXTMENU
                | WM_RBUTTONDOWN
                | WM_RBUTTONUP
                | WM_MBUTTONDOWN
                | WM_MBUTTONUP
                | WM_XBUTTONDOWN
                | WM_XBUTTONUP
        )
    {
        return 0;
    }
    let old: WNDPROC = Some(unsafe {
        std::mem::transmute::<usize, unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>(
            *OLD_PROC.get().unwrap(),
        )
    });
    // Trace only the real native EDIT handler. Explicit cursor-fixture messages
    // already returned above. No cursor is selected or changed by this trace.
    let before = if m == WM_SETCURSOR {
        Some(current_cursor_kind(unsafe { GetCursor() }))
    } else {
        None
    };
    let result = unsafe { CallWindowProcW(old, h, m, w, l) };
    if let Some(before_kind) = before {
        let after = unsafe { GetCursor() };
        let event = CursorSelectionEvent {
            hwnd: h as usize,
            thread_id: unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() },
            hit_test: (l & 0xFFFF) as u16 as i16,
            trigger: ((l >> 16) & 0xFFFF) as u16,
            native_result: result,
            before_kind,
            after_kind: current_cursor_kind(after),
            after_resource: cursor_resource(after),
        };
        // Never hold this lock across native handlers or cursor inspection.
        if let Some(snapshot) = SNAPSHOT.get() {
            let mut snapshot = snapshot.lock().unwrap();
            snapshot.cursor_selection_count = snapshot.cursor_selection_count.saturating_add(1);
            if snapshot.cursor_selection_events.len() >= 16 {
                snapshot.cursor_selection_events.remove(0);
            }
            snapshot.cursor_selection_events.push(event);
        }
    }
    result
}

fn record_native_event(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) {
    let (kind, code, pressed, client, delta) = match message {
        WM_KEYDOWN | WM_SYSKEYDOWN => ("key", w as u32, Some(true), None, None),
        WM_KEYUP | WM_SYSKEYUP => ("key", w as u32, Some(false), None, None),
        WM_CHAR => ("char", w as u32, None, None, None),
        WM_LBUTTONDOWN => ("button", 0, Some(true), Some(client_point(l)), None),
        WM_LBUTTONUP => ("button", 0, Some(false), Some(client_point(l)), None),
        WM_RBUTTONDOWN => ("button", 1, Some(true), Some(client_point(l)), None),
        WM_RBUTTONUP => ("button", 1, Some(false), Some(client_point(l)), None),
        WM_MBUTTONDOWN => ("button", 2, Some(true), Some(client_point(l)), None),
        WM_MBUTTONUP => ("button", 2, Some(false), Some(client_point(l)), None),
        WM_XBUTTONDOWN => (
            "button",
            if (w >> 16) & 0xFFFF == 1 { 3 } else { 4 },
            Some(true),
            Some(client_point(l)),
            None,
        ),
        WM_XBUTTONUP => (
            "button",
            if (w >> 16) & 0xFFFF == 1 { 3 } else { 4 },
            Some(false),
            Some(client_point(l)),
            None,
        ),
        WM_MOUSEMOVE => ("motion", 0, None, Some(client_point(l)), None),
        WM_MOUSEWHEEL => (
            "wheel",
            0,
            None,
            None,
            Some(((w >> 16) as u16 as i16) as i32),
        ),
        WM_MOUSEHWHEEL => (
            "horizontal_wheel",
            0,
            None,
            None,
            Some(((w >> 16) as u16 as i16) as i32),
        ),
        _ => return,
    };
    let at_us = TRACE_EPOCH
        .get()
        .map(|epoch| epoch.elapsed().as_micros().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0);
    if let Some(snapshot) = SNAPSHOT.get() {
        let mut snapshot = snapshot.lock().unwrap();
        snapshot.native_event_count = snapshot.native_event_count.saturating_add(1);
        if snapshot.native_events.len() < MAX_NATIVE_EVENTS {
            snapshot.native_events.push(NativeEvent {
                hwnd: hwnd as usize,
                kind: kind.to_owned(),
                at_us,
                code,
                pressed,
                client,
                delta,
                wheel_screen: matches!(message, WM_MOUSEWHEEL | WM_MOUSEHWHEEL)
                    .then(|| client_point(l)),
                wheel_modifiers: matches!(message, WM_MOUSEWHEEL | WM_MOUSEHWHEEL)
                    .then_some(w as u16),
            });
        }
    }
}

fn client_point(l: LPARAM) -> (i32, i32) {
    ((l as u16 as i16) as i32, ((l >> 16) as u16 as i16) as i32)
}
unsafe fn paint_capture_pattern(hwnd: HWND, dc: HDC) {
    unsafe {
        let color = capture_color().unwrap();
        let rgb = u32::from(color[0]) | (u32::from(color[1]) << 8) | (u32::from(color[2]) << 16);
        let mut bounds: RECT = std::mem::zeroed();
        GetClientRect(hwnd, &mut bounds);
        let background = CreateSolidBrush(rgb);
        FillRect(dc, &bounds, background);
        DeleteObject(background);
        // A known source-only registration mark also proves orientation and dimensions.
        let mark = RECT {
            left: 4,
            top: 4,
            right: 20,
            bottom: 20,
        };
        FillRect(dc, &mark, GetStockObject(WHITE_BRUSH) as HBRUSH);
    }
}
// USER32 probes this input at DWORD alignment even though ACCEL itself is WORD-aligned.
#[repr(C, align(4))]
struct AcceleratorTableEntry(ACCEL);
struct OwnedAcceleratorTable(HACCEL);
impl Drop for OwnedAcceleratorTable {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { DestroyAcceleratorTable(self.0) };
        }
    }
}
struct OwnedWindowTimer {
    hwnd: HWND,
    id: usize,
}
impl Drop for OwnedWindowTimer {
    fn drop(&mut self) {
        unsafe { KillTimer(self.hwnd, self.id) };
    }
}

pub fn run(
    report: Option<PathBuf>,
    capture_pattern: Option<u32>,
    cursor_fixture: bool,
    keyboard_focus_fixture: bool,
    native_controls_fixture: bool,
    accelerator_fixture: bool,
    keyboard_state_fixture: bool,
) -> Result<()> {
    ensure!(
        !native_controls_fixture
            || (!keyboard_focus_fixture && !cursor_fixture && capture_pattern.is_none()),
        "Native controls fixture must run without keyboard focus, cursor or capture fixtures"
    );
    ensure!(
        !accelerator_fixture || native_controls_fixture,
        "Accelerator fixture requires ordinary native controls"
    );
    if native_controls_fixture {
        NATIVE_CONTROLS_FIXTURE
            .set(())
            .map_err(|_| anyhow::anyhow!("Native controls fixture already initialized"))?;
    }
    ensure!(
        !keyboard_focus_fixture || (!cursor_fixture && capture_pattern.is_none()),
        "Keyboard focus fixture must run without cursor or capture fixtures"
    );
    let pid = std::process::id();
    let _fixture = if cursor_fixture {
        let cursor = create_fixture_cursor()?;
        CURSOR_FIXTURE
            .set(cursor.0 as usize)
            .map_err(|_| anyhow::anyhow!("cursor fixture already initialized"))?;
        Some(cursor)
    } else {
        None
    };
    if let Some(rgb) = capture_pattern {
        ensure!(rgb <= 0xFFFFFF, "capture pattern RGB exceeds 24 bits");
        CAPTURE_COLOR
            .set(Mutex::new([(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8]))
            .map_err(|_| anyhow::anyhow!("capture pattern already initialized"))?;
    }
    ensure!(
        !keyboard_state_fixture || accelerator_fixture,
        "Keyboard-state fixture requires accelerator fixture"
    );
    let report =
        report.unwrap_or_else(|| PathBuf::from(format!("runtime/automation-probe-{pid}.json")));
    if let Some(parent) = report.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    SNAPSHOT
        .set(Mutex::new(Snapshot {
            pid,
            accelerator: accelerator_fixture.then(|| AcceleratorDiagnostic {
                enabled: true,
                keyboard_state_fixture,
                keyboard_layout: unsafe { GetKeyboardLayout(0) as usize },
                ..Default::default()
            }),
            ..Default::default()
        }))
        .map_err(|_| anyhow::anyhow!("Probe already initialized"))?;
    TRACE_EPOCH
        .set(Instant::now())
        .map_err(|_| anyhow::anyhow!("Probe trace clock already initialized"))?;
    unsafe {
        ensure!(
            !windows_sys::Win32::UI::HiDpi::SetThreadDpiAwarenessContext(
                windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            )
            .is_null(),
            "Set fixture DPI context"
        );
        let module = GetModuleHandleW(std::ptr::null());
        let class = wide("ProtoInputMultiPlex.AutomationProbe");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(root_proc),
            hInstance: module,
            lpszClassName: class.as_ptr(),
            hbrBackground: (COLOR_WINDOW + 1) as HBRUSH,
            ..std::mem::zeroed()
        };
        ensure!(RegisterClassW(&wc) != 0, "Register automation probe window");
        let root = CreateWindowExW(
            WS_EX_NOACTIVATE,
            class.as_ptr(),
            wide(&format!("ProtoInputMultiPlex Automation Test — PID {pid}")).as_ptr(),
            WS_OVERLAPPEDWINDOW,
            40,
            50,
            420,
            360,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            module,
            std::ptr::null(),
        );
        ensure!(!root.is_null(), "Create automation probe window");
        let _accelerator_table = if accelerator_fixture {
            let entry = AcceleratorTableEntry(ACCEL {
                fVirt: FVIRTKEY | FCONTROL,
                key: u16::from(b'S'),
                cmd: ACCELERATOR_COMMAND_ID,
            });
            let table = OwnedAcceleratorTable(CreateAcceleratorTableW(&entry.0, 1));
            if table.0.is_null() {
                return Err(std::io::Error::last_os_error())
                    .context("Create Ctrl+S accelerator table");
            }
            SNAPSHOT
                .get()
                .unwrap()
                .lock()
                .unwrap()
                .accelerator
                .as_mut()
                .unwrap()
                .ready = true;
            Some(table)
        } else {
            None
        };
        let two_edits = keyboard_focus_fixture || native_controls_fixture;
        // Native form controls leave Tab navigation to the dialog manager.
        // Legacy text probes retain their multiline editing and scrolling styles.
        let edit_style = if native_controls_fixture {
            WS_CHILD | WS_VISIBLE | WS_TABSTOP
        } else {
            WS_CHILD | WS_VISIBLE | WS_VSCROLL | ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32
        };
        let edit = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            wide("EDIT").as_ptr(),
            wide("").as_ptr(),
            edit_style,
            20,
            30,
            if two_edits { 170 } else { 360 },
            240,
            root,
            std::ptr::null_mut(),
            module,
            std::ptr::null(),
        );
        ensure!(
            !edit.is_null(),
            "Create automation probe Unicode text editor"
        );
        {
            let mut snapshot = SNAPSHOT.get().unwrap().lock().unwrap();
            snapshot.capture_color = capture_color();
            snapshot.capture_hwnd = edit as usize;
        }
        CAPTURE_HWND
            .set(edit as usize)
            .map_err(|_| anyhow::anyhow!("Capture fixture HWND already initialized"))?;
        OLD_PROC
            .set(GetWindowLongPtrW(edit, GWLP_WNDPROC) as usize)
            .map_err(|_| anyhow::anyhow!("Probe procedure already initialized"))?;
        SetWindowLongPtrW(edit, GWLP_WNDPROC, edit_proc as *const () as _);
        SendMessageW(edit, 0x00C5, 32768, 0); // EM_SETLIMITTEXT
        let second_edit = if two_edits {
            let second = CreateWindowExW(
                WS_EX_CLIENTEDGE,
                wide("EDIT").as_ptr(),
                wide("").as_ptr(),
                edit_style,
                210,
                30,
                170,
                240,
                root,
                std::ptr::null_mut(),
                module,
                std::ptr::null(),
            );
            ensure!(!second.is_null(), "Create keyboard fixture's second editor");
            ensure!(
                GetWindowLongPtrW(second, GWLP_WNDPROC) as usize == *OLD_PROC.get().unwrap(),
                "Both EDIT controls must use the same native class procedure"
            );
            if keyboard_focus_fixture {
                KEYBOARD_CLICK_SURFACE
                    .set(second as usize)
                    .map_err(|_| anyhow::anyhow!("Keyboard click surface already initialized"))?;
            }
            SetWindowLongPtrW(second, GWLP_WNDPROC, edit_proc as *const () as _);
            SendMessageW(second, 0x00C5, 32768, 0);
            Some(second)
        } else {
            None
        };
        ShowWindow(root, SW_SHOWNOACTIVATE);
        let _timer = if accelerator_fixture {
            let id = SetTimer(root, 1, 100, None);
            ensure!(id != 0, "Create automation probe report timer");
            Some(OwnedWindowTimer { hwnd: root, id })
        } else {
            None
        };
        if let Some(second) = second_edit {
            let foreground_before_seed = GetForegroundWindow() as usize;
            // Opt-in disposable fixture only. This runs once before injection;
            // injected FocusHook intentionally cannot provide a later seed.
            SetFocus(edit);
            let thread_id = windows_sys::Win32::System::Threading::GetCurrentThreadId();
            let seeded = keyboard_focus(thread_id);
            let foreground_after_seed = GetForegroundWindow() as usize;
            ensure!(
                foreground_before_seed == foreground_after_seed,
                "Keyboard fixture seeding changed foreground window"
            );
            ensure!(
                seeded.available
                    && seeded.focused == edit as usize
                    && seeded.caret == edit as usize,
                "Native EDIT focus/caret seed not established"
            );
            SNAPSHOT
                .get()
                .unwrap()
                .lock()
                .unwrap()
                .keyboard_focus_fixture = Some(KeyboardFocusFixture {
                root: root as usize,
                edits: [edit as usize, second as usize],
                thread_id,
                foreground_before_seed,
                foreground_after_seed,
                second_edit_focus_policy: if native_controls_fixture {
                    "ordinary_native_controls"
                } else {
                    "preserve_initial_left_focus"
                },
                seeded,
                current: keyboard_focus(thread_id),
                edit_text: [String::new(), String::new()],
            });
        }
        if accelerator_fixture {
            let timer_id = _timer.as_ref().unwrap().id;
            let mut msg: MSG = std::mem::zeroed();
            loop {
                // This opt-in pump observes the next message twice without removal.
                // Sampling stays on this consuming GUI thread; no host pointer state
                // or input mutation is involved. The ordinary accelerator pump is unchanged.
                let mut peek = if keyboard_state_fixture {
                    loop {
                        let before = keyboard_table_sample();
                        let mut first: MSG = std::mem::zeroed();
                        if PeekMessageW(&mut first, std::ptr::null_mut(), 0, 0, PM_NOREMOVE) != 0 {
                            let after_first = keyboard_table_sample();
                            let mut second: MSG = std::mem::zeroed();
                            let second = (PeekMessageW(
                                &mut second,
                                std::ptr::null_mut(),
                                0,
                                0,
                                PM_NOREMOVE,
                            ) != 0)
                                .then(|| QueueMessageIdentity::from(&second));
                            break Some(KeyboardPeekSample {
                                before,
                                after_first,
                                after_second: keyboard_table_sample(),
                                first: QueueMessageIdentity::from(&first),
                                second,
                                removed: None,
                            });
                        }
                        ensure!(
                            WaitMessage() != 0,
                            "Wait for keyboard-state fixture message"
                        );
                    }
                } else {
                    None
                };
                let status = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
                if status == 0 {
                    return Ok(());
                }
                ensure!(status != -1, "Read automation probe message");
                if matches!(
                    msg.message,
                    WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP
                ) {
                    if let Some(peek) = &mut peek {
                        peek.removed = Some(QueueMessageIdentity::from(&msg));
                    }
                    let keyboard_state = keyboard_state_fixture.then(keyboard_table_sample);
                    let ctrl = GetKeyState(VK_CONTROL as i32) < 0;
                    let shift = GetKeyState(VK_SHIFT as i32) < 0;
                    let alt = GetKeyState(VK_MENU as i32) < 0;
                    let consumed =
                        TranslateAcceleratorW(root, _accelerator_table.as_ref().unwrap().0, &msg)
                            != 0;
                    if let Some(snapshot) = SNAPSHOT.get() {
                        let mut snapshot = snapshot.lock().unwrap();
                        let accelerator = snapshot.accelerator.as_mut().unwrap();
                        if accelerator.journal.len() < MAX_ACCELERATOR_MESSAGES {
                            accelerator.journal.push(AcceleratorMessage {
                                hwnd: msg.hwnd as usize,
                                message: msg.message,
                                vk: msg.wParam as u32,
                                control_down: ctrl,
                                shift_down: shift,
                                alt_down: alt,
                                translate_accelerator_consumed: consumed,
                                keyboard_state,
                                peek,
                            });
                        } else {
                            accelerator.journal_overflow = true;
                        }
                    }
                    if consumed {
                        continue;
                    }
                }
                let report_tick =
                    msg.hwnd == root && msg.message == WM_TIMER && msg.wParam == timer_id;
                if IsDialogMessageW(root, &msg) == 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                // Dialog processing can dispatch the timer itself. Sampling still
                // belongs to this tick, whether or not it needed explicit dispatch.
                if report_tick {
                    let text = edit_text(edit);
                    let fixture_text = second_edit.map(|second| [text.clone(), edit_text(second)]);
                    let actual_focus = second_edit.map(|_| {
                        keyboard_focus(windows_sys::Win32::System::Threading::GetCurrentThreadId())
                    });
                    let mut s = SNAPSHOT.get().unwrap().lock().unwrap();
                    s.clock_us = TRACE_EPOCH
                        .get()
                        .map(|epoch| epoch.elapsed().as_micros().min(u128::from(u64::MAX)) as u64)
                        .unwrap_or(0);
                    s.text = text;
                    if let Some(fixture) = &mut s.keyboard_focus_fixture {
                        fixture.current = actual_focus.unwrap();
                        fixture.edit_text = fixture_text.unwrap();
                    }
                    s.cursor = mux_windows::targets::probe_input().0;
                    s.buttons = mux_windows::targets::probe_buttons();
                    s.test_keys = mux_windows::targets::probe_input().1;
                    s.held_keys = (8..=254)
                        .filter(|key| GetAsyncKeyState(*key as i32) < 0)
                        .collect();
                    s.frames += 1;
                    std::fs::write(&report, serde_json::to_vec_pretty(&*s)?)
                        .context("Write automation probe report")?;
                }
            }
        } else {
            let mut last = Instant::now() - Duration::from_secs(1);
            loop {
                let mut msg: MSG = std::mem::zeroed();
                while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    if msg.message == WM_QUIT {
                        return Ok(());
                    }
                    if native_controls_fixture && IsDialogMessageW(root, &msg) != 0 {
                        continue;
                    }
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                if last.elapsed() >= Duration::from_millis(100) {
                    let text = edit_text(edit);
                    let fixture_text = second_edit.map(|second| [text.clone(), edit_text(second)]);
                    let actual_focus = second_edit.map(|_| {
                        keyboard_focus(windows_sys::Win32::System::Threading::GetCurrentThreadId())
                    });
                    let mut s = SNAPSHOT.get().unwrap().lock().unwrap();
                    s.clock_us = TRACE_EPOCH
                        .get()
                        .map(|epoch| epoch.elapsed().as_micros().min(u128::from(u64::MAX)) as u64)
                        .unwrap_or(0);
                    s.text = text;
                    if let Some(fixture) = &mut s.keyboard_focus_fixture {
                        fixture.current = actual_focus.unwrap();
                        fixture.edit_text = fixture_text.unwrap();
                    }
                    s.cursor = mux_windows::targets::probe_input().0;
                    s.buttons = mux_windows::targets::probe_buttons();
                    s.test_keys = mux_windows::targets::probe_input().1;
                    // Held-state sampling is diagnostic in this explicit synthetic fixture only.
                    s.held_keys = (8..=254)
                        .filter(|key| GetAsyncKeyState(*key as i32) < 0)
                        .collect();
                    s.frames += 1;
                    std::fs::write(&report, serde_json::to_vec_pretty(&*s)?)
                        .context("Write automation probe report")?;
                    last = Instant::now();
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

#[cfg(test)]
mod root_key_tests {
    use super::*;

    #[test]
    fn root_history_keeps_native_identity_and_bounds_without_changing_edit_history() {
        let mut snapshot = Snapshot::default();
        snapshot.root_key(101, WM_KEYDOWN, 163, (29 << 16) | (1 << 24));
        snapshot.root_key(
            202,
            WM_SYSKEYUP,
            163,
            (29 << 16) | (1 << 24) | (1 << 29) | (1isize << 31),
        );
        snapshot.root_key(101, WM_CHAR, 163, 1);
        assert_eq!(snapshot.root_key_event_count, 2);
        assert!(snapshot.key_events.is_empty());
        assert_eq!(snapshot.key_event_count, 0);
        let down = serde_json::to_value(&snapshot.root_key_events[0]).unwrap();
        assert_eq!(
            down,
            serde_json::json!({
                "hwnd": 101,
                "message": WM_KEYDOWN,
                "vk": 163,
                "pressed": true,
                "scan": 29,
                "extended": true,
                "alt": false
            })
        );
        let up = &snapshot.root_key_events[1];
        assert_eq!(
            (up.hwnd, up.message, up.key.vk, up.key.scan),
            (202, WM_SYSKEYUP, 163, 29)
        );
        assert!(!up.key.pressed && up.key.extended && up.key.alt);
        let edit = serde_json::to_value(KeyEvent::from_message(
            WM_KEYDOWN,
            163,
            (29 << 16) | (1 << 24),
        ))
        .unwrap();
        assert_eq!(
            edit,
            serde_json::json!({
                "vk": 163,
                "pressed": true,
                "scan": 29,
                "extended": true,
                "alt": false
            })
        );
        for index in 0..2049usize {
            snapshot.root_key(1000 + index, WM_KEYDOWN, 65, 30 << 16);
        }
        assert_eq!(snapshot.root_key_event_count, 2051);
        assert_eq!(snapshot.root_key_events.len(), 2048);
        assert_eq!(snapshot.root_key_events.first().unwrap().hwnd, 1001);
        assert_eq!(snapshot.root_key_events.last().unwrap().hwnd, 3048);
        assert!(snapshot.key_events.is_empty());
        assert_eq!(snapshot.key_event_count, 0);
    }
}
