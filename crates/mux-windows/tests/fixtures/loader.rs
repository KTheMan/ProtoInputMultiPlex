// A Rust cdylib test double: verifies ABI and lifecycle without injecting any process.
use std::sync::Mutex;

static CALLS: Mutex<Vec<(u32, u32, u32)>> = Mutex::new(Vec::new());
static FENCES: Mutex<Vec<[u64; 4]>> = Mutex::new(Vec::new());
static INJECTIONS: Mutex<(u32, u32)> = Mutex::new((0, 0));

fn record(op: u32, instance: u32, value: u32) {
    CALLS.lock().unwrap().push((op, instance, value));
}

#[unsafe(no_mangle)]
pub extern "C" fn TestReset(fail: u32) {
    CALLS.lock().unwrap().clear();
    FENCES.lock().unwrap().clear();
    *INJECTIONS.lock().unwrap() = (0, fail);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn TestRead(index: usize, out: *mut u32) -> bool {
    if let Some(&(op, instance, value)) = CALLS.lock().unwrap().get(index) {
        unsafe {
            out.write(op);
            out.add(1).write(instance);
            out.add(2).write(value);
        }
        true
    } else {
        false
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn TestReadFence(index: usize, out: *mut u64) -> bool {
    if let Some(args) = FENCES.lock().unwrap().get(index) {
        unsafe { std::ptr::copy_nonoverlapping(args.as_ptr(), out, args.len()) };
        true
    } else {
        false
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn EasyHookInjectRuntime(pid: u32, folder: *const u16) -> u32 {
    let mut n = 0;
    while unsafe { *folder.add(n) } != 0 {
        n += 1;
    }
    // Catch path encoding/termination mistakes at the real FFI boundary.
    if n == 0 || unsafe { *folder.add(n - 1) } != 92 {
        return 0;
    }
    let mut state = INJECTIONS.lock().unwrap();
    state.0 += 1;
    if state.1 == state.0 {
        record(0, 0, pid);
        return 0;
    }
    record(0, state.0, pid);
    state.0
}

#[unsafe(no_mangle)]
pub extern "C" fn SetupState(i: u32, v: i32) {
    record(1, i, v as u32);
}
#[unsafe(no_mangle)]
pub extern "C" fn InstallHook(i: u32, v: u32) {
    record(2, i, v);
}
#[unsafe(no_mangle)]
pub extern "C" fn UninstallHook(i: u32, v: u32) {
    record(3, i, v);
}
#[unsafe(no_mangle)]
pub extern "C" fn EnableMessageFilter(i: u32, v: u32) {
    record(4, i, v);
}
#[unsafe(no_mangle)]
pub extern "C" fn DisableMessageFilter(i: u32, v: u32) {
    record(5, i, v);
}
#[unsafe(no_mangle)]
pub extern "C" fn AddSelectedMouseHandle(i: u32, v: u32) {
    record(6, i, v);
}
#[unsafe(no_mangle)]
pub extern "C" fn AddSelectedKeyboardHandle(i: u32, v: u32) {
    record(7, i, v);
}
#[unsafe(no_mangle)]
pub extern "C" fn SetDrawFakeCursor(i: u32, v: bool) {
    record(8, i, v as u32);
}
#[unsafe(no_mangle)]
pub extern "C" fn SetExternalFreezeFakeInput(i: u32, v: bool) {
    record(9, i, v as u32);
}
#[unsafe(no_mangle)]
pub extern "C" fn SetupMessagesToSend(i: u32, a: bool, b: bool, c: bool, d: bool) {
    record(
        10,
        i,
        a as u32 | (b as u32) << 1 | (c as u32) << 2 | (d as u32) << 3,
    );
}
#[unsafe(no_mangle)]
pub extern "C" fn StartFocusMessageLoop(
    i: u32,
    ms: i32,
    _: bool,
    _: bool,
    _: bool,
    _: bool,
    _: bool,
) {
    record(11, i, ms as u32);
}
#[unsafe(no_mangle)]
pub extern "C" fn StopFocusMessageLoop(i: u32) {
    record(12, i, 0);
}

#[repr(C)]
pub struct Packet {
    device: u32,
    kind: u32,
    flags: u32,
    x: i32,
    y: i32,
    data: u32,
}

#[unsafe(no_mangle)]
pub extern "C" fn MuxInputVersion() -> u32 {
    3
}

/// Deliberately reject ownership, so the bridge must not claim an acknowledgement.
#[unsafe(no_mangle)]
pub extern "C" fn MuxQueueFence(i: u32, hwnd: u64, event: u64, cookie: u64) -> bool {
    FENCES
        .lock()
        .unwrap()
        .push([u64::from(i), hwnd, event, cookie]);
    false
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn MuxSubmitInput(i: u32, packet: *const Packet) {
    let packet = unsafe { &*packet };
    record(13, i, packet.device);
    record(14, packet.kind, packet.flags);
    record(15, packet.x as u32, packet.y as u32);
    record(16, i, packet.data);
}

#[unsafe(no_mangle)]
pub extern "C" fn UpdateMainWindowHandle(i: u32, h: u64) {
    record(17, i, h as u32);
}
#[unsafe(no_mangle)]
pub extern "C" fn AllowFakeCursorOutOfBounds(i: u32, a: bool, b: bool) {
    record(18, i, a as u32 | (b as u32) << 1);
}
#[unsafe(no_mangle)]
pub extern "C" fn EnableMessageBlock(i: u32, m: u32) {
    record(19, i, m);
}
#[unsafe(no_mangle)]
pub extern "C" fn DisableMessageBlock(i: u32, m: u32) {
    record(20, i, m);
}
