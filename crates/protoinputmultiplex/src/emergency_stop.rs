//! A per-logon, per-desktop emergency-stop owner shared by independent controllers.
//!
//! Pipe connections are leases, not persistent reference counts. The detached helper
//! owns the hotkey until the final lease disappears; no subscriber owns its lifetime.
//! Established subscriptions never reconnect: a missing or unresponsive broker is a
//! safety failure which callers must stop on before allowing another session start.
use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::{OsStr, c_void},
    fs::File,
    io::{Read, Write},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
        process::CommandExt,
    },
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, FILETIME, HANDLE, HWND, INVALID_HANDLE_VALUE, LPARAM, LRESULT, LocalFree,
        WPARAM,
    },
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        },
        GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_GROUPS, TOKEN_QUERY, TokenGroups,
    },
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    },
    System::{
        LibraryLoader::GetModuleHandleW,
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId,
            GetNamedPipeServerProcessId, PIPE_NOWAIT, PIPE_READMODE_MESSAGE,
            PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PeekNamedPipe, SetNamedPipeHandleState,
        },
        Threading::{
            GetCurrentProcess, GetCurrentThreadId, GetProcessTimes, OpenProcess, OpenProcessToken,
            PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW,
            WaitForSingleObject,
        },
    },
    UI::{
        Input::KeyboardAndMouse::{
            MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey, UnregisterHotKey, VK_F11,
        },
        WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, HWND_MESSAGE, MSG,
            PM_REMOVE, PeekMessageW, RegisterClassW, WM_HOTKEY, WNDCLASSW,
        },
    },
};

const FRAME_BYTES: usize = 40;
const MAGIC: &[u8; 8] = b"PIMSTOP1";
const HELLO: u32 = 1;
const WELCOME: u32 = 2;
const HEARTBEAT: u32 = 3;
const HOTKEY_ID: i32 = 0x504d;
const MODIFIERS: u32 = MOD_CONTROL | MOD_ALT | MOD_SHIFT;
const POLL: Duration = Duration::from_millis(10);
const HEARTBEAT_PERIOD: Duration = Duration::from_millis(250);
const HEALTH_TIMEOUT: Duration = Duration::from_millis(1500);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(750);
const BOOT_GRACE: Duration = Duration::from_secs(3);
const MAX_CLIENTS: usize = 64;
static PULSE: AtomicU64 = AtomicU64::new(0);

// These desktop identity APIs require no UI access beyond querying the caller's
// own desktop. Keep their narrow ABI local rather than adding unrelated features.
#[link(name = "user32")]
unsafe extern "system" {
    fn GetThreadDesktop(thread_id: u32) -> HANDLE;
    fn GetProcessWindowStation() -> HANDLE;
    fn GetUserObjectInformationW(
        object: HANDLE,
        index: i32,
        info: *mut c_void,
        length: u32,
        needed: *mut u32,
    ) -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetTickCount64() -> u64;
}
fn uptime_millis() -> u64 {
    unsafe { GetTickCount64() }
}
fn fresh_heartbeat(sent: u64, now: u64) -> bool {
    now.checked_sub(sent)
        .is_some_and(|age| age <= HEALTH_TIMEOUT.as_millis() as u64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Frame {
    kind: u32,
    pid: u32,
    creation: u64,
    sequence: u64,
    sent: u64,
}
impl Frame {
    fn bytes(self) -> [u8; FRAME_BYTES] {
        let mut bytes = [0; FRAME_BYTES];
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..12].copy_from_slice(&self.kind.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.pid.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.creation.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.sequence.to_le_bytes());
        bytes[32..].copy_from_slice(&self.sent.to_le_bytes());
        bytes
    }
    fn parse(bytes: [u8; FRAME_BYTES]) -> Result<Self> {
        ensure!(&bytes[..8] == MAGIC, "emergency-stop protocol mismatch");
        let frame = Self {
            kind: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            pid: u32::from_le_bytes(bytes[12..16].try_into().unwrap()),
            creation: u64::from_le_bytes(bytes[16..24].try_into().unwrap()),
            sequence: u64::from_le_bytes(bytes[24..32].try_into().unwrap()),
            sent: u64::from_le_bytes(bytes[32..].try_into().unwrap()),
        };
        ensure!(
            matches!(frame.kind, HELLO | WELCOME | HEARTBEAT),
            "invalid emergency-stop frame"
        );
        Ok(frame)
    }
}

struct Handle(HANDLE);
// Process handles support cross-thread waits/queries/closure; ownership remains
// unique and no thread-affine synchronization handle is stored here.
unsafe impl Send for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
struct Process {
    handle: Handle,
    pid: u32,
    creation: u64,
}
impl Process {
    fn open(pid: u32) -> Result<Self> {
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        ensure!(
            !handle.is_null(),
            "open emergency-stop peer: {}",
            std::io::Error::last_os_error()
        );
        let handle = Handle(handle);
        let creation = creation_time(handle.0)?;
        let mut path = vec![0u16; 32768];
        let mut length = path.len() as u32;
        ensure!(
            unsafe { QueryFullProcessImageNameW(handle.0, 0, path.as_mut_ptr(), &mut length) } != 0,
            "identify emergency-stop peer"
        );
        path.truncate(length as usize);
        let own = std::env::current_exe()?;
        ensure!(
            String::from_utf16_lossy(&path).eq_ignore_ascii_case(&own.to_string_lossy()),
            "emergency-stop peer belongs to another executable"
        );
        let result = Self {
            handle,
            pid,
            creation,
        };
        ensure!(result.alive(), "emergency-stop peer has exited");
        Ok(result)
    }
    fn alive(&self) -> bool {
        unsafe { WaitForSingleObject(self.handle.0, 0) == 258 }
    }
    fn matches(&self, frame: Frame) -> bool {
        self.pid == frame.pid && self.creation == frame.creation
    }
}
fn creation_time(handle: HANDLE) -> Result<u64> {
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
        "read emergency-stop peer creation time"
    );
    Ok((u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime))
}

struct Pipe(File);
impl Pipe {
    fn handle(&self) -> HANDLE {
        self.0.as_raw_handle().cast()
    }
    fn available(&self) -> Result<u32> {
        let mut count = 0;
        ensure!(
            unsafe {
                PeekNamedPipe(
                    self.handle(),
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut count,
                    std::ptr::null_mut(),
                )
            } != 0,
            "emergency-stop connection lost: {}",
            std::io::Error::last_os_error()
        );
        Ok(count)
    }
    fn receive(&mut self) -> Result<Option<Frame>> {
        let count = self.available()?;
        if count == 0 {
            return Ok(None);
        }
        ensure!(
            count >= FRAME_BYTES as u32,
            "truncated emergency-stop message"
        );
        let mut bytes = [0; FRAME_BYTES];
        // Message mode rejects oversized messages instead of accepting prefixes.
        let read = self
            .0
            .read(&mut bytes)
            .context("read emergency-stop message")?;
        ensure!(read == FRAME_BYTES, "invalid emergency-stop message size");
        Ok(Some(Frame::parse(bytes)?))
    }
    fn send(&mut self, frame: Frame) -> Result<()> {
        let written = self
            .0
            .write(&frame.bytes())
            .context("write emergency-stop message")?;
        ensure!(
            written == FRAME_BYTES,
            "emergency-stop peer is unresponsive"
        );
        Ok(())
    }
}

struct Progress {
    peer_pid: u32,
    peer_creation: u64,
    last_heartbeat_tick: u64,
    received_sequence: u64,
    delivered_sequence: u64,
    failed: bool,
}
impl Progress {
    fn new(welcome: Frame) -> Self {
        Self {
            peer_pid: welcome.pid,
            peer_creation: welcome.creation,
            last_heartbeat_tick: welcome.sent,
            received_sequence: welcome.sequence,
            delivered_sequence: welcome.sequence,
            failed: false,
        }
    }
    fn accept(&mut self, frame: Frame) -> Result<()> {
        let result = (|| {
            ensure!(!self.failed, "emergency-stop subscription has failed");
            ensure!(
                frame.kind == HEARTBEAT
                    && frame.pid == self.peer_pid
                    && frame.creation == self.peer_creation,
                "invalid emergency-stop heartbeat"
            );
            ensure!(
                frame.sequence >= self.received_sequence,
                "emergency-stop sequence went backwards"
            );
            ensure!(
                frame.sent >= self.last_heartbeat_tick,
                "emergency-stop heartbeat clock went backwards"
            );
            self.received_sequence = frame.sequence;
            self.last_heartbeat_tick = frame.sent;
            Ok(())
        })();
        self.failed |= result.is_err();
        result
    }
    fn next(&mut self, now: u64) -> Result<bool> {
        if !fresh_heartbeat(self.last_heartbeat_tick, now) {
            self.failed = true;
        }
        ensure!(
            !self.failed,
            "emergency-stop subscription failed or broker heartbeat timed out"
        );
        if self.delivered_sequence < self.received_sequence {
            self.delivered_sequence += 1;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
/// A healthy connection to the fixed Ctrl+Alt+Shift+F11 stop broker.
pub struct Subscription {
    pipe: Pipe,
    broker: Process,
    progress: Progress,
}
impl Subscription {
    pub fn connect() -> Result<Self> {
        let scope = Scope::current()?;
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        let mut next_spawn = Instant::now();
        let mut last_error = anyhow::anyhow!("emergency-stop broker unavailable");
        while Instant::now() < deadline {
            match Self::try_connect(&scope, deadline) {
                Ok(subscription) => return Ok(subscription),
                Err(error) => last_error = error,
            }
            if Instant::now() >= next_spawn {
                Command::new(std::env::current_exe()?)
                    .arg("emergency-broker")
                    .creation_flags(0x0800_0000) // CREATE_NO_WINDOW; independent of subscriber lifetime.
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .context("launch emergency-stop broker")?;
                next_spawn = Instant::now() + Duration::from_millis(500);
            }
            thread::sleep(POLL);
        }
        Err(last_error).context("cannot acquire shared Ctrl+Alt+Shift+F11 emergency stop (another application may own the chord)")
    }
    fn try_connect(scope: &Scope, deadline: Instant) -> Result<Self> {
        let handle = unsafe {
            CreateFileW(
                scope.pipe.as_ptr(),
                0xc000_0000,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        ensure!(
            handle != INVALID_HANDLE_VALUE,
            "connect emergency-stop broker: {}",
            std::io::Error::last_os_error()
        );
        let mut pipe = Pipe(unsafe { File::from_raw_handle(handle.cast()) });
        let mode = PIPE_READMODE_MESSAGE | PIPE_NOWAIT;
        ensure!(
            unsafe {
                SetNamedPipeHandleState(pipe.handle(), &mode, std::ptr::null(), std::ptr::null())
            } != 0,
            "configure emergency-stop connection"
        );
        let mut pid = 0;
        ensure!(
            unsafe { GetNamedPipeServerProcessId(pipe.handle(), &mut pid) } != 0,
            "identify emergency-stop broker"
        );
        let broker = Process::open(pid)?;
        pipe.send(Frame {
            kind: HELLO,
            pid: std::process::id(),
            creation: creation_time(unsafe { GetCurrentProcess() })?,
            sequence: 0,
            sent: 0,
        })?;
        let handshake_deadline = deadline.min(Instant::now() + HANDSHAKE_TIMEOUT);
        loop {
            ensure!(
                broker.alive(),
                "emergency-stop broker exited during handshake"
            );
            if let Some(frame) = pipe.receive()? {
                ensure!(
                    frame.kind == WELCOME && broker.matches(frame),
                    "invalid emergency-stop handshake"
                );
                ensure!(
                    fresh_heartbeat(frame.sent, uptime_millis()),
                    "stale emergency-stop handshake"
                );
                return Ok(Self {
                    pipe,
                    broker,
                    progress: Progress::new(frame),
                });
            }
            ensure!(
                Instant::now() < handshake_deadline,
                "emergency-stop broker handshake timed out"
            );
            thread::sleep(POLL);
        }
    }
    /// Consume one pending stop pulse. Any error is terminal and remains sticky.
    pub fn poll(&mut self) -> Result<bool> {
        ensure!(
            !self.progress.failed,
            "emergency-stop subscription has failed"
        );
        let result = self.poll_inner();
        if result.is_err() {
            self.progress.failed = true;
        }
        result
    }
    fn poll_inner(&mut self) -> Result<bool> {
        ensure!(self.broker.alive(), "emergency-stop broker exited");
        let mut drained = 0;
        for _ in 0..64 {
            let Some(frame) = self.pipe.receive()? else {
                break;
            };
            self.progress.accept(frame)?;
            drained += 1;
        }
        ensure!(
            drained < 64 || self.pipe.available()? == 0,
            "emergency-stop broker traffic limit exceeded"
        );
        // Use the sender's monotonic timestamp: a busy consumer may drain fresh
        // buffered heartbeats, but stale traffic cannot revive a hung broker.
        self.progress.next(uptime_millis())
    }
}

struct Scope {
    pipe: Vec<u16>,
    window_name: Vec<u16>,
    security: SecurityDescriptor,
}
impl Scope {
    fn current() -> Result<Self> {
        let sid = logon_sid()?;
        let station = user_object_name(unsafe { GetProcessWindowStation() })?;
        let desktop = user_object_name(unsafe { GetThreadDesktop(GetCurrentThreadId()) })?;
        let exe = std::env::current_exe()?;
        let metadata = exe.metadata()?;
        let identity = format!(
            "{sid}|{station}|{desktop}|{}|{}|{:?}|{}",
            exe.to_string_lossy().to_lowercase(),
            metadata.len(),
            metadata.modified()?,
            env!("CARGO_PKG_VERSION")
        );
        // Stable FNV-1a is a namespace discriminator, not an authentication secret.
        let hash = identity.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
        let name = format!("ProtoInputMultiPlex.EmergencyStop.v1.{hash:016x}");
        Ok(Self {
            pipe: wide(format!(r"\\.\pipe\{name}")),
            window_name: wide(name),
            security: SecurityDescriptor::new(&sid)?,
        })
    }
}
fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}
fn user_object_name(handle: HANDLE) -> Result<String> {
    ensure!(!handle.is_null(), "current desktop identity unavailable");
    let mut needed = 0;
    unsafe {
        GetUserObjectInformationW(handle, 2, std::ptr::null_mut(), 0, &mut needed);
    }
    ensure!(
        needed > 0 && needed <= 65536,
        "invalid desktop identity size"
    );
    let mut name = vec![0u16; (needed as usize).div_ceil(2)];
    ensure!(
        unsafe {
            GetUserObjectInformationW(handle, 2, name.as_mut_ptr().cast(), needed, &mut needed)
        } != 0,
        "read current desktop identity"
    );
    let length = name.iter().position(|ch| *ch == 0).unwrap_or(name.len());
    String::from_utf16(&name[..length]).context("decode desktop identity")
}
fn logon_sid() -> Result<String> {
    let mut token = std::ptr::null_mut();
    ensure!(
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } != 0,
        "open current logon token"
    );
    let token = Handle(token);
    let mut required = 0;
    unsafe {
        GetTokenInformation(token.0, TokenGroups, std::ptr::null_mut(), 0, &mut required);
    }
    ensure!(required > 0, "current logon token groups unavailable");
    let mut words = vec![0usize; (required as usize).div_ceil(std::mem::size_of::<usize>())];
    ensure!(
        unsafe {
            GetTokenInformation(
                token.0,
                TokenGroups,
                words.as_mut_ptr().cast(),
                required,
                &mut required,
            )
        } != 0,
        "read current logon groups"
    );
    let groups = words.as_ptr().cast::<TOKEN_GROUPS>();
    let entries = unsafe { std::ptr::addr_of!((*groups).Groups) }
        .cast::<windows_sys::Win32::Security::SID_AND_ATTRIBUTES>();
    for index in 0..unsafe { (*groups).GroupCount as usize } {
        let group = unsafe { entries.add(index).read() };
        if group.Attributes & 0xc000_0000 != 0xc000_0000 {
            continue;
        }
        let mut sid = std::ptr::null_mut();
        ensure!(
            unsafe { ConvertSidToStringSidW(group.Sid, &mut sid) } != 0,
            "encode current logon SID"
        );
        let mut length = 0;
        unsafe {
            while *sid.add(length) != 0 {
                length += 1;
            }
        }
        let result = String::from_utf16(unsafe { std::slice::from_raw_parts(sid, length) });
        unsafe {
            LocalFree(sid.cast());
        }
        return result.context("decode current logon SID");
    }
    bail!("current logon SID unavailable")
}
struct SecurityDescriptor(*mut c_void);
impl SecurityDescriptor {
    fn new(sid: &str) -> Result<Self> {
        let sddl = wide(format!("D:P(A;;GA;;;{sid})"));
        let mut descriptor = std::ptr::null_mut();
        ensure!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    1,
                    &mut descriptor,
                    std::ptr::null_mut(),
                )
            } != 0,
            "create protected emergency-stop logon DACL"
        );
        Ok(Self(descriptor))
    }
    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}
impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}

struct Window(HWND);
impl Window {
    fn create(scope: &Scope) -> Result<Self> {
        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: scope.window_name.as_ptr(),
            ..Default::default()
        };
        ensure!(
            unsafe { RegisterClassW(&class) } != 0,
            "register emergency-stop broker window"
        );
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                scope.window_name.as_ptr(),
                scope.window_name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            )
        };
        ensure!(!hwnd.is_null(), "create emergency-stop broker window");
        let window = Self(hwnd);
        ensure!(
            unsafe { RegisterHotKey(hwnd, HOTKEY_ID, MODIFIERS | MOD_NOREPEAT, u32::from(VK_F11)) }
                != 0,
            "Ctrl+Alt+Shift+F11 is unavailable: {}",
            std::io::Error::last_os_error()
        );
        Ok(window)
    }
}
impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            UnregisterHotKey(self.0, HOTKEY_ID);
            DestroyWindow(self.0);
        }
    }
}
fn is_stop_message(message: u32, wparam: WPARAM, lparam: LPARAM) -> bool {
    message == WM_HOTKEY
        && wparam == HOTKEY_ID as usize
        && lparam as usize == ((usize::from(VK_F11) << 16) | MODIFIERS as usize)
}
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if is_stop_message(message, wparam, lparam) {
        // Exhaustion must be fatal rather than making a pulse disappear.
        if PULSE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .is_err()
        {
            std::process::abort();
        }
        return 0;
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

struct Client {
    pipe: Pipe,
    process: Process,
    accepted: Instant,
    ready: bool,
    last_sent: Instant,
    sequence: u64,
}
impl Client {
    fn service(&mut self, own: Frame, sequence: u64) -> Result<bool> {
        ensure!(self.process.alive(), "emergency-stop subscriber exited");
        if !self.ready {
            if let Some(hello) = self.pipe.receive()? {
                ensure!(
                    hello.kind == HELLO
                        && hello.sequence == 0
                        && hello.sent == 0
                        && self.process.matches(hello),
                    "invalid emergency-stop subscriber handshake"
                );
                self.pipe.send(Frame {
                    kind: WELCOME,
                    sequence,
                    sent: uptime_millis(),
                    ..own
                })?;
                self.ready = true;
                self.last_sent = Instant::now();
                self.sequence = sequence;
            } else {
                ensure!(
                    self.accepted.elapsed() <= HEALTH_TIMEOUT,
                    "emergency-stop subscriber handshake timed out"
                );
            }
        } else {
            ensure!(
                self.pipe.available()? == 0,
                "unexpected emergency-stop subscriber traffic"
            );
            if sequence != self.sequence || self.last_sent.elapsed() >= HEARTBEAT_PERIOD {
                self.pipe.send(Frame {
                    kind: HEARTBEAT,
                    sequence,
                    sent: uptime_millis(),
                    ..own
                })?;
                self.last_sent = Instant::now();
                self.sequence = sequence;
            }
        }
        Ok(self.ready)
    }
}
fn listener(scope: &Scope, first: bool) -> Result<Pipe> {
    let attributes = scope.security.attributes();
    let handle = unsafe {
        CreateNamedPipeW(
            scope.pipe.as_ptr(),
            PIPE_ACCESS_DUPLEX
                | if first {
                    FILE_FLAG_FIRST_PIPE_INSTANCE
                } else {
                    0
                },
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
            (MAX_CLIENTS + 1) as u32,
            4096,
            4096,
            0,
            &attributes,
        )
    };
    ensure!(
        handle != INVALID_HANDLE_VALUE,
        "create emergency-stop broker pipe: {}",
        std::io::Error::last_os_error()
    );
    Ok(Pipe(unsafe { File::from_raw_handle(handle.cast()) }))
}

/// Entry point for the hidden helper command. Never call this on the GUI thread.
pub fn broker_main() -> Result<()> {
    let scope = Scope::current()?;
    // FIRST_PIPE_INSTANCE elects one broker without a stale on-disk lock. Keeping
    // at least one instance open prevents any raced helper taking over the name.
    let mut pending = listener(&scope, true)?;
    let _window = Window::create(&scope)?;
    let own = Frame {
        kind: HEARTBEAT,
        pid: std::process::id(),
        creation: creation_time(unsafe { GetCurrentProcess() })?,
        sequence: 0,
        sent: 0,
    };
    let booted = Instant::now();
    let mut had_participant = false;
    let mut clients: Vec<Client> = Vec::new();
    loop {
        let mut message = MSG::default();
        while unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
            unsafe {
                DispatchMessageW(&message);
            }
        }
        if clients.len() < MAX_CLIENTS {
            let connected =
                unsafe { ConnectNamedPipe(pending.handle(), std::ptr::null_mut()) } != 0;
            let error = std::io::Error::last_os_error().raw_os_error();
            if connected || error == Some(535) {
                // ERROR_PIPE_CONNECTED
                let replacement = listener(&scope, false)?;
                let pipe = std::mem::replace(&mut pending, replacement);
                let mut pid = 0;
                if unsafe { GetNamedPipeClientProcessId(pipe.handle(), &mut pid) } != 0
                    && let Ok(process) = Process::open(pid)
                {
                    clients.push(Client {
                        pipe,
                        process,
                        accepted: Instant::now(),
                        ready: false,
                        last_sent: Instant::now(),
                        sequence: 0,
                    });
                }
            } else if error != Some(536) {
                // ERROR_PIPE_LISTENING
                // An unaccepted client may disconnect before the next tick.
                // Replace that instance while the old one still owns the name.
                let replacement = listener(&scope, false)?;
                pending = replacement;
            }
        }
        let sequence = PULSE.load(Ordering::Relaxed);
        clients.retain_mut(|client| match client.service(own, sequence) {
            Ok(ready) => {
                had_participant |= ready;
                true
            }
            Err(_) => false,
        });
        if (had_participant && clients.is_empty())
            || (!had_participant && booted.elapsed() >= BOOT_GRACE)
        {
            return Ok(());
        }
        thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_protocol_round_trip_and_rejection() {
        let frame = Frame {
            kind: HEARTBEAT,
            pid: 1234,
            creation: u64::MAX - 3,
            sequence: 99,
            sent: 12345,
        };
        assert_eq!(Frame::parse(frame.bytes()).unwrap(), frame);
        let mut bytes = frame.bytes();
        bytes[0] ^= 1;
        assert!(Frame::parse(bytes).is_err());
        let mut bytes = frame.bytes();
        bytes[8..12].copy_from_slice(&99u32.to_le_bytes());
        assert!(Frame::parse(bytes).is_err());
    }
    #[test]
    fn heartbeat_age_uses_sender_time_and_rejects_future() {
        assert!(fresh_heartbeat(5000, 5000));
        assert!(fresh_heartbeat(5000, 6500));
        assert!(!fresh_heartbeat(5000, 6501));
        assert!(!fresh_heartbeat(5001, 5000));
    }
    fn welcome() -> Frame {
        Frame {
            kind: WELCOME,
            pid: 123,
            creation: 456,
            sequence: 7,
            sent: 1000,
        }
    }
    #[test]
    fn independent_subscribers_receive_each_pulse_once() {
        let mut first = Progress::new(welcome());
        let mut second = Progress::new(welcome());
        let heartbeat = Frame {
            kind: HEARTBEAT,
            sequence: 9,
            sent: 3000,
            ..welcome()
        };
        first.accept(heartbeat).unwrap();
        second.accept(heartbeat).unwrap();
        for _ in 0..2 {
            assert!(first.next(3000).unwrap());
        }
        assert!(!first.next(3000).unwrap());
        // One subscriber consuming its pulses never consumes another's pulses.
        assert!(second.next(3000).unwrap());
        assert!(second.next(3000).unwrap());
        assert!(!second.next(3000).unwrap());
    }
    #[test]
    fn invalid_peer_sequence_clock_and_stale_health_are_terminal() {
        let valid = Frame {
            kind: HEARTBEAT,
            sent: 2000,
            ..welcome()
        };
        for invalid in [
            Frame { pid: 124, ..valid },
            Frame {
                creation: 457,
                ..valid
            },
            Frame {
                sequence: 6,
                ..valid
            },
            Frame { sent: 999, ..valid },
            Frame {
                kind: WELCOME,
                ..valid
            },
        ] {
            let mut state = Progress::new(welcome());
            assert!(state.accept(invalid).is_err());
            assert!(state.accept(valid).is_err());
            assert!(state.next(2000).is_err());
        }
        let mut stale = Progress::new(welcome());
        assert!(stale.next(2501).is_err());
        assert!(stale.accept(valid).is_err());
        let mut future = Progress::new(welcome());
        assert!(future.next(999).is_err());
    }
    #[test]
    fn stop_message_requires_exact_id_and_chord() {
        let chord = ((usize::from(VK_F11) << 16) | MODIFIERS as usize) as LPARAM;
        assert!(is_stop_message(WM_HOTKEY, HOTKEY_ID as usize, chord));
        assert!(!is_stop_message(WM_HOTKEY, HOTKEY_ID as usize + 1, chord));
        assert!(!is_stop_message(
            WM_HOTKEY,
            HOTKEY_ID as usize,
            chord ^ MOD_ALT as isize
        ));
        assert!(!is_stop_message(
            WM_HOTKEY,
            HOTKEY_ID as usize,
            chord ^ (1 << 16)
        ));
        assert!(!is_stop_message(0, HOTKEY_ID as usize, chord));
    }
}
