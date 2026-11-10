//! Disposable hosted-probe crash capture only. The helper never debugs or reopens a PID.
// This source is shared by the probe and its standalone helper; each uses one entrypoint.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::{
    cell::UnsafeCell,
    ffi::{OsStr, c_void},
    fs::{File, OpenOptions},
    io::Write,
    mem::{MaybeUninit, size_of},
    os::windows::process::CommandExt,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::Path,
    process::{Command, Stdio},
    ptr,
    sync::atomic::{AtomicPtr, AtomicU32, Ordering},
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_ALREADY_EXISTS, FILETIME,
        FreeLibrary, GetLastError, HANDLE, HMODULE, INVALID_HANDLE_VALUE, SetLastError,
        WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Storage::FileSystem::{FILE_BEGIN, SetFilePointerEx, WriteFile},
    System::{
        Diagnostics::Debug::{
            CONTEXT, CONTEXT_AMD64, CancelCallback, EXCEPTION_POINTERS, EXCEPTION_RECORD,
            IoFinishCallback, IoStartCallback, IoWriteAllCallback, MINIDUMP_CALLBACK_INFORMATION,
            MINIDUMP_CALLBACK_INPUT, MINIDUMP_CALLBACK_OUTPUT, MINIDUMP_CALLBACK_OUTPUT_0_0,
            MINIDUMP_CALLBACK_OUTPUT_0_1, MINIDUMP_EXCEPTION_INFORMATION, MINIDUMP_TYPE,
            MINIDUMP_USER_STREAM_INFORMATION, MemoryCallback, MiniDumpWithCodeSegs,
            MiniDumpWithFullMemoryInfo, MiniDumpWithThreadInfo, MiniDumpWithUnloadedModules,
            MiniDumpWithoutAuxiliaryState, ModuleCallback, ModuleWriteCodeSegs, ModuleWriteDataSeg,
            ModuleWriteTlsData,
        },
        LibraryLoader::{
            GetModuleFileNameW, GetProcAddress, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
            LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
        },
        Memory::{
            CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEM_COMMIT, MEMORY_BASIC_INFORMATION,
            MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW, PAGE_READWRITE,
            UnmapViewOfFile, VirtualQueryEx,
        },
        SystemInformation::GetTickCount64,
        Threading::{
            CREATE_NO_WINDOW, CreateEventW, EVENT_MODIFY_STATE, GetCurrentProcess,
            GetCurrentThread, GetCurrentThreadId, GetProcessId, GetProcessIdOfThread,
            GetProcessTimes, GetThreadId, GetThreadTimes, OpenEventW, OpenThread,
            PROCESS_QUERY_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_VM_READ,
            SYNCHRONIZATION_SYNCHRONIZE, SetEvent, THREAD_QUERY_INFORMATION, THREAD_SYNCHRONIZE,
            WaitForSingleObject,
        },
    },
};

const CONFIG_ENV: &str = "PROTOINPUTMUX_PRIVATE_DUMP_CONFIG";
const MAGIC: u64 = 0x3150_4d55_444d_4950; // PIMDUMP1
const WAIT_MAGIC: u64 = 0x3154_4941_574d_4950; // PIMWAIT1
const MAPPING_BYTES: usize = 4096;
const MAX_DUMP: u64 = 64 * 1024 * 1024;
const READY: u32 = 1;
const REQUESTED: u32 = 2;
const CAPTURING: u32 = 3;
const COMPLETE: u32 = 4;
const FAILED: u32 = 5;
const AV: u32 = 0xc000_0005;
const TOOL_ROOT: &str = "C:\\Program Files (x86)\\Windows Kits\\10\\Debuggers\\x64";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    schema: String,
    source_commit: String,
    helper: String,
    deadline_tick_ms: u64,
    module_paths: Vec<String>,
}

fn own_module(path: &str) -> bool {
    matches!(
        Path::new(path)
            .file_name()
            .and_then(OsStr::to_str)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some(
            "protoinputmultiplex.exe"
                | "protoinputhooks64.dll"
                | "protoinputloader64.dll"
                | "easyhook64.dll"
                | "protoinputmultiplex_cursor_observer64.dll"
        )
    )
}

fn config(path: &Path) -> Option<Config> {
    if !path.is_absolute() || std::fs::metadata(path).ok()?.len() > 64 * 1024 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() > 64 * 1024 {
        return None;
    }
    let value: Config = serde_json::from_slice(&bytes).ok()?;
    let now = unsafe { GetTickCount64() };
    if value.schema != "pim.private-dump-config.v1"
        || value.source_commit.len() != 40
        || !value
            .source_commit
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        || !Path::new(&value.helper).is_absolute()
        || Path::new(&value.helper).file_name() != Some(OsStr::new("provider_private_dump.exe"))
        || value.module_paths.is_empty()
        || value.module_paths.len() > 8
        || value.module_paths.iter().any(|p| {
            !Path::new(p).is_absolute()
                || !own_module(p)
                || p.encode_utf16().count() > 1024
                || p.contains('\0')
        })
        || value.deadline_tick_ms <= now
        || value.deadline_tick_ms.saturating_sub(now) > 420_000
    {
        return None;
    }
    Some(value)
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct AlignedContext(CONTEXT);

#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub(super) struct Snapshot {
    exception: EXCEPTION_RECORD,
    context: AlignedContext,
    thread_id: u32,
    original_flags: u32,
    thread_creation: u64,
    result: i32,
    reserved: u32,
}

#[repr(C, align(16))]
struct Shared {
    magic: u64,
    version: u32,
    bytes: u32,
    pid: u32,
    reserved: u32,
    creation: u64,
    end: u64,
    handles: UnsafeCell<[u64; 3]>,
    handles_ready: AtomicU32,
    state: AtomicU32,
    snapshot: UnsafeCell<Snapshot>,
}
const _: () = {
    assert!(size_of::<CONTEXT>() == 1232);
    assert!(std::mem::align_of::<AlignedContext>() == 16);
    assert!(size_of::<EXCEPTION_RECORD>() == 152);
    assert!(size_of::<MINIDUMP_EXCEPTION_INFORMATION>() == 16);
    assert!(size_of::<Shared>() <= MAPPING_BYTES);
    assert!(std::mem::offset_of!(Snapshot, context) % 16 == 0);
    assert!(std::mem::offset_of!(Shared, snapshot) % 16 == 0);
};

struct Handle(HANDLE);
impl Handle {
    fn new(raw: HANDLE) -> Option<Self> {
        (!raw.is_null() && raw != INVALID_HANDLE_VALUE).then_some(Self(raw))
    }
    fn into_raw(self) -> HANDLE {
        let raw = self.0;
        std::mem::forget(self);
        raw
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
struct View(*mut Shared);
impl Drop for View {
    fn drop(&mut self) {
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.0.cast(),
            });
        }
    }
}
struct Library(HMODULE);
impl Drop for Library {
    fn drop(&mut self) {
        unsafe {
            FreeLibrary(self.0);
        }
    }
}
fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}
fn named_event(name: &str) -> Option<Handle> {
    let name = wide(OsStr::new(name));
    let (raw, error) = unsafe {
        let raw = CreateEventW(ptr::null(), 1, 0, name.as_ptr());
        (raw, GetLastError())
    };
    let handle = Handle::new(raw)?;
    (error != ERROR_ALREADY_EXISTS).then_some(handle)
}
fn creation(process: HANDLE, end: u64) -> Option<u64> {
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    if unsafe { GetTickCount64() } >= end {
        return None;
    }
    if unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
        return None;
    }
    Some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}
fn thread_creation(thread: HANDLE, end: u64) -> Option<u64> {
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    if unsafe { GetTickCount64() } >= end {
        return None;
    }
    if unsafe { GetThreadTimes(thread, &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
        return None;
    }
    Some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}
fn duplicate(source: HANDLE, target: HANDLE, access: u32, options: u32) -> Option<u64> {
    let mut remote = ptr::null_mut();
    (unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            source,
            target,
            &mut remote,
            access,
            0,
            options,
        )
    } != 0)
        .then_some(remote as u64)
}

struct Sender {
    view: View,
    _mapping: Handle,
    request: Handle,
    complete: Handle,
    wait_file: File,
}
static SENDER: AtomicPtr<Sender> = AtomicPtr::new(ptr::null_mut());

/// Optional normal-startup preparation. There is deliberately no readiness wait.
pub(super) fn prepare(report: &Path, pid: u32, born: u64) {
    if !matches!(
        report.file_name().and_then(OsStr::to_str),
        Some("target-0.json" | "target-1.json")
    ) {
        return;
    }
    let Some(path) = std::env::var_os(CONFIG_ENV) else {
        return;
    };
    let path = Path::new(&path);
    let Some(cfg) = config(path) else {
        return;
    };
    let _ = prepare_inner(report, path, &cfg, pid, born);
}

fn prepare_inner(report: &Path, path: &Path, cfg: &Config, pid: u32, born: u64) -> Option<()> {
    let name = format!("Local\\PIMPrivateDump-{pid}-{born}");
    let wide_name = wide(OsStr::new(&name));
    let (raw, error) = unsafe {
        let raw = CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            ptr::null(),
            PAGE_READWRITE,
            0,
            MAPPING_BYTES as u32,
            wide_name.as_ptr(),
        );
        (raw, GetLastError())
    };
    let mapping = Handle::new(raw)?;
    if error == ERROR_ALREADY_EXISTS {
        return None;
    }
    let mapped = unsafe { MapViewOfFile(mapping.0, FILE_MAP_ALL_ACCESS, 0, 0, MAPPING_BYTES) };
    if mapped.Value.is_null() {
        return None;
    }
    let view = View(mapped.Value.cast());
    unsafe {
        ptr::write(
            view.0,
            Shared {
                magic: MAGIC,
                version: 1,
                bytes: size_of::<Shared>() as u32,
                pid,
                reserved: 0,
                creation: born,
                end: cfg.deadline_tick_ms,
                handles: UnsafeCell::new([0; 3]),
                handles_ready: AtomicU32::new(0),
                state: AtomicU32::new(0),
                snapshot: UnsafeCell::new(std::mem::zeroed()),
            },
        );
    }
    let request = named_event(&(name.clone() + "-request"))?;
    let complete = named_event(&(name.clone() + "-complete"))?;
    let dump = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(report.with_extension("dump.private.dmp"))
        .ok()?;
    let status = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(report.with_extension("dump-status.private.json"))
        .ok()?;
    let wait_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(report.with_extension("dump-wait.private.bin"))
        .ok()?;
    let mut command = Command::new(&cfg.helper);
    command
        .args([
            OsStr::new("--mapping"),
            OsStr::new(&name),
            OsStr::new("--config"),
            path.as_os_str(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    for key in [
        "_NT_SYMBOL_PATH",
        "_NT_ALT_SYMBOL_PATH",
        "_NT_SOURCE_PATH",
        "_NT_EXECUTABLE_IMAGE_PATH",
        "DBGHELP_HOMEDIR",
    ] {
        command.env_remove(key);
    }
    let child = command.spawn().ok()?;
    // Only these duplicated handles are delivered; the helper never opens a PID.
    let process_handle = duplicate(
        unsafe { GetCurrentProcess() },
        child.as_raw_handle(),
        PROCESS_QUERY_INFORMATION | PROCESS_VM_READ | PROCESS_SYNCHRONIZE,
        0,
    )?;
    let dump_handle = duplicate(
        dump.as_raw_handle(),
        child.as_raw_handle(),
        0,
        DUPLICATE_SAME_ACCESS,
    )?;
    let status_handle = duplicate(
        status.as_raw_handle(),
        child.as_raw_handle(),
        0,
        DUPLICATE_SAME_ACCESS,
    )?;
    unsafe {
        ptr::write(
            (*view.0).handles.get(),
            [process_handle, dump_handle, status_handle],
        );
        (*view.0).handles_ready.store(1, Ordering::Release);
    }
    let sender = Box::new(Sender {
        view,
        _mapping: mapping,
        request,
        complete,
        wait_file,
    });
    SENDER.store(Box::into_raw(sender), Ordering::Release);
    // Child drop closes our child handle; it never kills, waits for or retries it.
    Some(())
}

/// Isolated callback-local copy before the previous filter can mutate its pointers.
/// No heap, formatting, lock, memory walk or XSTATE-tail dereference occurs here.
pub(super) unsafe fn snapshot(info: *const EXCEPTION_POINTERS) -> Option<Snapshot> {
    let sender = SENDER.load(Ordering::Acquire);
    if sender.is_null() || unsafe { (*(*sender).view.0).state.load(Ordering::Acquire) } != READY {
        return None;
    }
    let end = unsafe { (*(*sender).view.0).end };
    let saved_error = unsafe { GetLastError() };
    let result = unsafe { snapshot_inner(info, end) };
    unsafe {
        SetLastError(saved_error);
    }
    result
}

unsafe fn snapshot_inner(info: *const EXCEPTION_POINTERS, end: u64) -> Option<Snapshot> {
    if unsafe { GetTickCount64() } >= end {
        return None;
    }
    let pointers = unsafe { info.as_ref() }?;
    if pointers.ExceptionRecord.is_null() || pointers.ContextRecord.is_null() {
        return None;
    }
    let exception = unsafe { ptr::read_unaligned(pointers.ExceptionRecord) };
    let context = unsafe { ptr::read_unaligned(pointers.ContextRecord) };
    if !supported_context(&exception, &context) {
        return None;
    }
    let thread_creation = thread_creation(unsafe { GetCurrentThread() }, end)?;
    if thread_creation == 0 {
        return None;
    }
    Some(Snapshot {
        exception,
        context: AlignedContext(context),
        thread_id: unsafe { GetCurrentThreadId() },
        original_flags: context.ContextFlags,
        thread_creation,
        result: 0,
        reserved: 0,
    })
}

fn supported_context(exception: &EXCEPTION_RECORD, context: &CONTEXT) -> bool {
    exception.ExceptionCode as u32 == AV
        && exception.ExceptionRecord.is_null()
        && exception.NumberParameters <= 15
        && context.ContextFlags & 0x00ff_0000 == CONTEXT_AMD64
        && context.ContextFlags & (CONTEXT_AMD64 | 3) == (CONTEXT_AMD64 | 3)
}

fn analysis_exception(mut exception: EXCEPTION_RECORD) -> EXCEPTION_RECORD {
    exception.ExceptionInformation[exception.NumberParameters as usize..].fill(0);
    exception
}

/// Called only by the existing first-eligible winner. Return disposition is untouched.
pub(super) unsafe fn publish(snapshot: Option<Snapshot>, result: i32) {
    let Some(mut snapshot) = snapshot else {
        return;
    };
    let sender = SENDER.load(Ordering::Acquire);
    if sender.is_null() || !matches!(result, 0 | 1) {
        return;
    }
    let sender = unsafe { &*sender };
    let shared = unsafe { &*sender.view.0 };
    let now = unsafe { GetTickCount64() };
    if shared.state.load(Ordering::Acquire) != READY || now >= shared.end {
        return;
    }
    snapshot.result = result;
    unsafe {
        ptr::write(shared.snapshot.get(), snapshot);
    }
    shared.state.store(REQUESTED, Ordering::Release);
    let result = if unsafe { SetEvent(sender.request.0) } == 0 {
        u32::MAX
    } else {
        // This bounds the REQUESTED wait only. DbgHelp can suspend this thread,
        // so neither callback return nor process liveness has a hard 5s bound.
        let now = unsafe { GetTickCount64() };
        if now >= shared.end {
            WAIT_TIMEOUT
        } else {
            unsafe {
                WaitForSingleObject(
                    sender.complete.0,
                    shared.end.saturating_sub(now).min(5000) as u32,
                )
            }
        }
    };
    if unsafe { GetTickCount64() } >= shared.end {
        return;
    }
    let acceptance_state = shared.state.load(Ordering::Acquire);
    let words = [
        WAIT_MAGIC,
        1,
        u64::from(shared.pid),
        shared.creation,
        u64::from(snapshot.thread_id),
        u64::from(result),
        u64::from(acceptance_state),
        shared.end,
    ];
    let mut written = 0;
    if unsafe { GetTickCount64() } >= shared.end {
        return;
    }
    let _ = unsafe {
        WriteFile(
            sender.wait_file.as_raw_handle(),
            words.as_ptr().cast(),
            64,
            &mut written,
            ptr::null_mut(),
        )
    };
}

type Dump = unsafe extern "system" fn(
    HANDLE,
    u32,
    HANDLE,
    MINIDUMP_TYPE,
    *const MINIDUMP_EXCEPTION_INFORMATION,
    *const MINIDUMP_USER_STREAM_INFORMATION,
    *const MINIDUMP_CALLBACK_INFORMATION,
) -> i32;

struct Output {
    file: File,
    end: u64,
    extent: u64,
    failed: bool,
    finished: bool,
    pc_page: Option<(u64, u32)>,
    modules: Vec<String>,
}
fn extent(offset: u64, length: u32) -> Option<u64> {
    offset
        .checked_add(u64::from(length))
        .filter(|end| *end <= MAX_DUMP)
}
fn normalize(path: &str) -> String {
    path.strip_prefix("\\\\?\\")
        .unwrap_or(path)
        .replace('/', "\\")
        .to_ascii_lowercase()
}
unsafe fn callback_path(path: *const u16) -> Option<String> {
    if path.is_null() {
        return None;
    }
    for length in 0..=1024 {
        if unsafe { *path.add(length) } == 0 {
            return String::from_utf16(unsafe { std::slice::from_raw_parts(path, length) }).ok();
        }
    }
    None
}

unsafe extern "system" fn callback(
    param: *mut c_void,
    input: *const MINIDUMP_CALLBACK_INPUT,
    output: *mut MINIDUMP_CALLBACK_OUTPUT,
) -> i32 {
    if param.is_null() || input.is_null() || output.is_null() {
        return 0;
    }
    let state = unsafe { &mut *param.cast::<Output>() };
    let input = unsafe { ptr::read_unaligned(input) };
    let output = unsafe { &mut *output };
    let expired = unsafe { GetTickCount64() } >= state.end;
    match input.CallbackType as i32 {
        IoStartCallback => {
            output.Anonymous.Status = 1;
            1
        }
        IoWriteAllCallback => {
            let io = unsafe { input.Anonymous.Io };
            let Some(end) = extent(io.Offset, io.BufferBytes) else {
                state.failed = true;
                return 0;
            };
            if expired || state.failed || (io.BufferBytes != 0 && io.Buffer.is_null()) {
                state.failed = true;
                return 0;
            }
            let mut written = 0;
            let ok = unsafe {
                SetFilePointerEx(
                    state.file.as_raw_handle(),
                    io.Offset as i64,
                    ptr::null_mut(),
                    FILE_BEGIN,
                )
            } != 0
                && unsafe { GetTickCount64() } < state.end
                && unsafe {
                    WriteFile(
                        state.file.as_raw_handle(),
                        io.Buffer.cast(),
                        io.BufferBytes,
                        &mut written,
                        ptr::null_mut(),
                    )
                } != 0
                && written == io.BufferBytes;
            if !ok {
                state.failed = true;
                return 0;
            }
            state.extent = state.extent.max(end);
            output.Anonymous.Status = 0;
            1
        }
        IoFinishCallback => {
            state.finished = !state.failed && !expired;
            output.Anonymous.Status = if state.finished { 0 } else { -2147467259 };
            i32::from(state.finished)
        }
        CancelCallback => {
            output.Anonymous.Anonymous2 = MINIDUMP_CALLBACK_OUTPUT_0_1 {
                CheckCancel: 1,
                Cancel: i32::from(expired || state.failed),
            };
            1
        }
        ModuleCallback => {
            let module = unsafe { input.Anonymous.Module };
            let admitted = unsafe { callback_path(module.FullPath) }
                .is_some_and(|p| state.modules.iter().any(|m| *m == normalize(&p)));
            let mut flags = unsafe { output.Anonymous.ModuleWriteFlags };
            flags &= !((ModuleWriteDataSeg | ModuleWriteTlsData) as u32);
            if !admitted {
                flags &= !(ModuleWriteCodeSegs as u32);
            }
            output.Anonymous.ModuleWriteFlags = flags;
            1
        }
        MemoryCallback => {
            if expired {
                return 0;
            }
            if let Some((base, size)) = state.pc_page.take() {
                output.Anonymous.Anonymous1 = MINIDUMP_CALLBACK_OUTPUT_0_0 {
                    MemoryBase: base,
                    MemorySize: size,
                };
                1
            } else {
                0
            }
        }
        _ => 1,
    }
}

fn executable_page(process: HANDLE, address: u64) -> Option<(u64, u32)> {
    let mut memory = MaybeUninit::<MEMORY_BASIC_INFORMATION>::zeroed();
    if unsafe {
        VirtualQueryEx(
            process,
            address as *const c_void,
            memory.as_mut_ptr(),
            size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    } != size_of::<MEMORY_BASIC_INFORMATION>()
    {
        return None;
    }
    let memory = unsafe { memory.assume_init() };
    let region = memory.BaseAddress as u64;
    let end = region.checked_add(memory.RegionSize as u64)?;
    if memory.State != MEM_COMMIT
        || memory.Protect & 0x100 != 0
        || !matches!(memory.Protect & 0xff, 0x20 | 0x40 | 0x80)
        || address < region
        || address >= end
    {
        return None;
    }
    let base = (address & !4095).max(region);
    let size = (end - base).min(4096) as u32;
    (size != 0).then_some((base, size))
}

#[derive(Serialize)]
struct Status {
    schema: &'static str,
    source_commit: String,
    status: &'static str,
    pid: u32,
    creation: u64,
    thread_id: u32,
    thread_creation: u64,
    exception_code: u32,
    exception_address: u64,
    exception_flags: u32,
    exception_parameter_count: u32,
    exception_parameters: [u64; 15],
    filter_result: i32,
    original_rip: u64,
    original_rsp: u64,
    original_context_flags: u32,
    integer_registers: [u64; 17],
    basic_context_only: bool,
    thread_owner_pid_verified: bool,
    dump_bytes: u64,
    one_attempt: bool,
    deadline_tick_ms: u64,
}

fn load_known(path: &str, end: u64) -> Option<Library> {
    let flags = LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32;
    let wide_path = wide(OsStr::new(path));
    if unsafe { GetTickCount64() } >= end {
        return None;
    }
    let library = Library(unsafe { LoadLibraryExW(wide_path.as_ptr(), ptr::null_mut(), flags) });
    if library.0.is_null() {
        return None;
    }
    let mut loaded_path = [0u16; 1024];
    if unsafe { GetTickCount64() } >= end {
        return None;
    }
    let length = unsafe {
        GetModuleFileNameW(
            library.0,
            loaded_path.as_mut_ptr(),
            loaded_path.len() as u32,
        )
    } as usize;
    if length == 0
        || length >= loaded_path.len()
        || normalize(&String::from_utf16(&loaded_path[..length]).ok()?) != normalize(path)
    {
        return None;
    }
    Some(library)
}

fn load_dump(end: u64) -> Option<(Library, Library, Dump)> {
    let core = load_known(&(TOOL_ROOT.to_owned() + "\\dbgcore.dll"), end)?;
    let help = load_known(&(TOOL_ROOT.to_owned() + "\\dbghelp.dll"), end)?;
    if unsafe { GetTickCount64() } >= end {
        return None;
    }
    let proc = unsafe { GetProcAddress(help.0, c"MiniDumpWriteDump".as_ptr().cast()) }?;
    Some((core, help, unsafe {
        std::mem::transmute::<unsafe extern "system" fn() -> isize, Dump>(proc)
    }))
}

/// The standalone helper has no console output, debugger attach, PID reopen or retries.
pub fn helper_main() {
    let _ = helper_inner();
}
fn helper_inner() -> Option<()> {
    if std::env::var("GITHUB_ACTIONS").as_deref() != Ok("true")
        || std::env::var("RUNNER_ENVIRONMENT").as_deref() != Ok("github-hosted")
    {
        return None;
    }
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 || args[0] != "--mapping" || args[2] != "--config" {
        return None;
    }
    let name = args[1].to_str()?;
    if name.len() > 100
        || !name.starts_with("Local\\PIMPrivateDump-")
        || !name[21..].bytes().all(|c| c.is_ascii_digit() || c == b'-')
    {
        return None;
    }
    let cfg = config(Path::new(&args[3]))?;
    let wide_name = wide(&args[1]);
    let mapping =
        Handle::new(unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS, 0, wide_name.as_ptr()) })?;
    let mapped = unsafe { MapViewOfFile(mapping.0, FILE_MAP_ALL_ACCESS, 0, 0, MAPPING_BYTES) };
    if mapped.Value.is_null() {
        return None;
    }
    let view = View(mapped.Value.cast());
    let shared = unsafe { &*view.0 };
    // A raced startup is explicitly unarmed. There is no wait/poll for handles or READY.
    if shared.handles_ready.load(Ordering::Acquire) != 1
        || shared.magic != MAGIC
        || shared.version != 1
        || shared.bytes as usize != size_of::<Shared>()
        || shared.reserved != 0
        || shared.pid == 0
        || shared.creation == 0
        || shared.end != cfg.deadline_tick_ms
        || name != format!("Local\\PIMPrivateDump-{}-{}", shared.pid, shared.creation)
    {
        return None;
    }
    let handles = unsafe { ptr::read(shared.handles.get()) };
    if shared.state.load(Ordering::Acquire) != 0
        || handles.contains(&0)
        || handles[0] == handles[1]
        || handles[0] == handles[2]
        || handles[1] == handles[2]
    {
        return None;
    }
    let process = Handle::new(handles[0] as HANDLE)?;
    let dump = unsafe { File::from_raw_handle(Handle::new(handles[1] as HANDLE)?.into_raw()) };
    let mut status =
        unsafe { File::from_raw_handle(Handle::new(handles[2] as HANDLE)?.into_raw()) };
    if unsafe { GetTickCount64() } >= shared.end
        || unsafe { GetProcessId(process.0) } != shared.pid
        || creation(process.0, shared.end)? != shared.creation
        || unsafe { GetTickCount64() } >= shared.end
        || unsafe { WaitForSingleObject(process.0, 0) } != WAIT_TIMEOUT
    {
        return None;
    }
    let request_name = wide(OsStr::new(&(name.to_owned() + "-request")));
    let complete_name = wide(OsStr::new(&(name.to_owned() + "-complete")));
    if unsafe { GetTickCount64() } >= shared.end {
        return None;
    }
    let request =
        Handle::new(unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, 0, request_name.as_ptr()) })?;
    if unsafe { GetTickCount64() } >= shared.end {
        return None;
    }
    let complete =
        Handle::new(unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, complete_name.as_ptr()) })?;
    let (_core, _help, write_dump) = load_dump(shared.end)?;
    let now = unsafe { GetTickCount64() };
    if now >= shared.end {
        return None;
    }
    shared.state.store(READY, Ordering::Release);
    let now = unsafe { GetTickCount64() };
    if now >= shared.end
        || unsafe {
            WaitForSingleObject(
                request.0,
                shared.end.saturating_sub(now).min(u64::from(u32::MAX - 1)) as u32,
            )
        } != WAIT_OBJECT_0
        || shared.state.load(Ordering::Acquire) != REQUESTED
    {
        return None;
    }
    let snapshot = unsafe { ptr::read(shared.snapshot.get()) };
    if snapshot.reserved != 0
        || snapshot.thread_id == 0
        || snapshot.thread_creation == 0
        || !supported_context(&snapshot.exception, &snapshot.context.0)
        || !matches!(snapshot.result, 0 | 1)
        || snapshot.original_flags != snapshot.context.0.ContextFlags
        || unsafe { GetTickCount64() } >= shared.end
    {
        return None;
    }
    let thread = Handle::new(unsafe {
        OpenThread(
            THREAD_QUERY_INFORMATION | THREAD_SYNCHRONIZE,
            0,
            snapshot.thread_id,
        )
    })?;
    if unsafe { GetTickCount64() } >= shared.end
        || unsafe { GetProcessIdOfThread(thread.0) } != shared.pid
        || unsafe { GetTickCount64() } >= shared.end
        || unsafe { GetThreadId(thread.0) } != snapshot.thread_id
        || thread_creation(thread.0, shared.end)? != snapshot.thread_creation
        || unsafe { GetTickCount64() } >= shared.end
        || unsafe { WaitForSingleObject(thread.0, 0) } != WAIT_TIMEOUT
        || unsafe { GetTickCount64() } >= shared.end
        || unsafe { WaitForSingleObject(process.0, 0) } != WAIT_TIMEOUT
    {
        return None;
    }
    // Preserve the original fixed context privately. The analysis copy advertises
    // only fixed AMD64 control/integer state; no variable XSTATE tail is followed.
    let mut context = snapshot.context;
    context.0.ContextFlags = CONTEXT_AMD64 | 3;
    let mut exception_record = analysis_exception(snapshot.exception);
    let mut pointers = EXCEPTION_POINTERS {
        ExceptionRecord: &mut exception_record,
        ContextRecord: &mut context.0,
    };
    let exception = MINIDUMP_EXCEPTION_INFORMATION {
        ThreadId: snapshot.thread_id,
        ExceptionPointers: &mut pointers,
        ClientPointers: 0,
    };
    if unsafe { GetTickCount64() } >= shared.end {
        return None;
    }
    let mut output = Output {
        file: dump,
        end: shared.end,
        extent: 0,
        failed: false,
        finished: false,
        pc_page: executable_page(process.0, context.0.Rip),
        modules: cfg.module_paths.iter().map(|p| normalize(p)).collect(),
    };
    let callbacks = MINIDUMP_CALLBACK_INFORMATION {
        CallbackRoutine: Some(callback),
        CallbackParam: ptr::from_mut(&mut output).cast(),
    };
    let flags = MiniDumpWithThreadInfo
        | MiniDumpWithFullMemoryInfo
        | MiniDumpWithUnloadedModules
        | MiniDumpWithCodeSegs
        | MiniDumpWithoutAuxiliaryState;
    shared.state.store(CAPTURING, Ordering::Release);
    let ok = unsafe { GetTickCount64() } < shared.end
        && unsafe {
            write_dump(
                process.0,
                shared.pid,
                output.file.as_raw_handle(),
                flags,
                &exception,
                ptr::null(),
                &callbacks,
            )
        } != 0;
    let persisted = ok
        && output.finished
        && !output.failed
        && output.extent > 0
        && output.extent <= MAX_DUMP
        && unsafe { GetTickCount64() } < shared.end
        && output.file.set_len(output.extent).is_ok()
        && unsafe { GetTickCount64() } < shared.end
        && output.file.sync_all().is_ok()
        && unsafe { GetTickCount64() } < shared.end;
    let context = &snapshot.context.0;
    let result = Status {
        schema: "pim.private-owned-minidump.v1",
        source_commit: cfg.source_commit,
        status: if persisted { "complete" } else { "failed" },
        pid: shared.pid,
        creation: shared.creation,
        thread_id: snapshot.thread_id,
        thread_creation: snapshot.thread_creation,
        exception_code: snapshot.exception.ExceptionCode as u32,
        exception_address: snapshot.exception.ExceptionAddress as u64,
        exception_flags: snapshot.exception.ExceptionFlags,
        exception_parameter_count: snapshot.exception.NumberParameters,
        exception_parameters: snapshot
            .exception
            .ExceptionInformation
            .map(|value| value as u64),
        filter_result: snapshot.result,
        original_rip: snapshot.context.0.Rip,
        original_rsp: snapshot.context.0.Rsp,
        original_context_flags: snapshot.original_flags,
        integer_registers: [
            context.Rax,
            context.Rcx,
            context.Rdx,
            context.Rbx,
            context.Rsp,
            context.Rbp,
            context.Rsi,
            context.Rdi,
            context.R8,
            context.R9,
            context.R10,
            context.R11,
            context.R12,
            context.R13,
            context.R14,
            context.R15,
            context.Rip,
        ],
        basic_context_only: true,
        thread_owner_pid_verified: true,
        dump_bytes: output.extent,
        one_attempt: true,
        deadline_tick_ms: shared.end,
    };
    let bytes = serde_json::to_vec(&result).ok()?;
    let recorded = bytes.len() <= 4096
        && unsafe { GetTickCount64() } < shared.end
        && status.write_all(&bytes).is_ok()
        && unsafe { GetTickCount64() } < shared.end
        && status.sync_all().is_ok();
    shared.state.store(
        if persisted && recorded && unsafe { GetTickCount64() } < shared.end {
            COMPLETE
        } else {
            FAILED
        },
        Ordering::Release,
    );
    unsafe {
        SetEvent(complete.0);
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dump_extent_rejects_overflow_and_crossing_cap() {
        assert_eq!(extent(MAX_DUMP - 4, 4), Some(MAX_DUMP));
        assert_eq!(extent(MAX_DUMP - 4, 5), None);
        assert_eq!(extent(u64::MAX, 1), None);
    }
    #[test]
    fn code_segments_require_exact_admitted_module_path() {
        let path = normalize(r"C:\private\runtime\ProtoInputHooks64.dll");
        assert_eq!(
            path,
            normalize(r"\\?\C:\private\runtime\ProtoInputHooks64.dll")
        );
        assert_ne!(path, normalize(r"C:\other\ProtoInputHooks64.dll"));
        assert!(own_module(&path));
        assert!(!own_module(r"C:\private\runtime\foreign.dll"));
    }
    #[test]
    fn fixed_context_rejects_wrong_arch_missing_registers_and_exception_chains() {
        let mut exception = EXCEPTION_RECORD {
            ExceptionCode: AV as i32,
            NumberParameters: 2,
            ..Default::default()
        };
        let mut context = CONTEXT {
            ContextFlags: CONTEXT_AMD64 | 3,
            ..Default::default()
        };
        assert!(supported_context(&exception, &context));
        context.ContextFlags = CONTEXT_AMD64 | 1;
        assert!(!supported_context(&exception, &context));
        context.ContextFlags = 0x0001_0003;
        assert!(!supported_context(&exception, &context));
        context.ContextFlags = CONTEXT_AMD64 | 3;
        exception.ExceptionRecord = ptr::dangling_mut();
        assert!(!supported_context(&exception, &context));
        exception.ExceptionRecord = ptr::null_mut();
        exception.NumberParameters = 16;
        assert!(!supported_context(&exception, &context));
    }
    #[test]
    fn analysis_exception_only_zeros_unused_parameter_tail_in_its_copy() {
        let original = EXCEPTION_RECORD {
            NumberParameters: 2,
            ExceptionInformation: [41; 15],
            ..Default::default()
        };
        let copy = analysis_exception(original);
        assert_eq!(&copy.ExceptionInformation[..2], &[41, 41]);
        assert_eq!(&copy.ExceptionInformation[2..], &[0; 13]);
        assert_eq!(original.ExceptionInformation, [41; 15]);
    }
}
