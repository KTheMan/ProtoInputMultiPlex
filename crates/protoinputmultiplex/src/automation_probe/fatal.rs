//! Private, opt-in hosted probe record. No dump, recovery or exception mutation.
use std::{
    cell::UnsafeCell,
    ffi::c_void,
    fs::OpenOptions,
    mem::{MaybeUninit, size_of},
    os::windows::io::{AsRawHandle, IntoRawHandle},
    path::Path,
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering},
};
use windows_sys::Win32::{
    Foundation::FILETIME,
    Storage::FileSystem::WriteFile,
    System::{
        Diagnostics::Debug::{
            EXCEPTION_CONTINUE_SEARCH, EXCEPTION_EXECUTE_HANDLER, EXCEPTION_POINTERS, GetErrorMode,
            SetUnhandledExceptionFilter,
        },
        Memory::{MEM_IMAGE, MEMORY_BASIC_INFORMATION, VirtualQuery},
        ProcessStatus::K32GetMappedFileNameW,
        Threading::{GetCurrentProcess, GetProcessTimes},
    },
};

const MAGIC: u64 = 0x4c41_5441_464d_4950; // PIMFATAL, little endian.
const AV: u32 = 0xc000_0005;
const PATH_UNITS: usize = 512;
const RECORD_BYTES: usize = 1152;
type Filter = unsafe extern "system" fn(*const EXCEPTION_POINTERS) -> i32;

#[repr(C)]
struct Record {
    words: [u64; 16],
    path: [u16; PATH_UNITS],
}
impl Record {
    const ZERO: Self = Self {
        words: [0; 16],
        path: [0; PATH_UNITS],
    };
}
const _: () = {
    assert!(size_of::<Record>() == RECORD_BYTES);
    assert!(std::mem::offset_of!(Record, path) == 128);
};
struct Storage(UnsafeCell<Record>);
// Initialized before registration; only the first qualifying callback may write.
unsafe impl Sync for Storage {}
static RECORD: Storage = Storage(UnsafeCell::new(Record::ZERO));
static INITIALIZED: AtomicBool = AtomicBool::new(false);
static CLAIMED: AtomicBool = AtomicBool::new(false);
static FILE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static PREVIOUS: AtomicUsize = AtomicUsize::new(0);

fn filter_with(
    exception: Option<(u32, usize)>,
    previous: impl FnOnce() -> i32,
    claimed: &AtomicBool,
    capture: impl FnOnce(usize, i32),
) -> i32 {
    let result = previous();
    if let Some((AV, address)) = exception
        && matches!(
            result,
            EXCEPTION_CONTINUE_SEARCH | EXCEPTION_EXECUTE_HANDLER
        )
        && claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    {
        capture(address, result);
    }
    result
}

/// No allocation, formatting, Rust lock, retry or context write in this callback.
unsafe extern "system" fn unhandled(info: *const EXCEPTION_POINTERS) -> i32 {
    let exception = unsafe { info.as_ref() }
        .and_then(|pointers| unsafe { pointers.ExceptionRecord.as_ref() })
        .map(|record| {
            (
                record.ExceptionCode as u32,
                record.ExceptionAddress as usize,
            )
        });
    let previous = PREVIOUS.load(Ordering::Acquire);
    filter_with(
        exception,
        || {
            if previous == 0 {
                EXCEPTION_CONTINUE_SEARCH
            } else {
                let filter: Filter = unsafe { std::mem::transmute(previous) };
                unsafe { filter(info) }
            }
        },
        &CLAIMED,
        |address, result| unsafe { capture(address, result, previous != 0) },
    )
}

unsafe fn capture(address: usize, result: i32, previous: bool) {
    let file = FILE.load(Ordering::Acquire);
    if file.is_null() {
        return;
    }
    let record = unsafe { &mut *RECORD.0.get() };
    record.words[7] = u64::from(previous);
    record.words[8] = u64::from(AV);
    record.words[9] = result as u64;
    record.words[10] = address as u64;
    let mut memory = MaybeUninit::<MEMORY_BASIC_INFORMATION>::zeroed();
    let queried = unsafe {
        VirtualQuery(
            address as *const c_void,
            memory.as_mut_ptr(),
            size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    if queried == size_of::<MEMORY_BASIC_INFORMATION>() {
        let memory = unsafe { memory.assume_init() };
        record.words[11] = memory.AllocationBase as u64;
        record.words[12] = u64::from(memory.Type);
        if memory.Type == MEM_IMAGE && !memory.AllocationBase.is_null() {
            let length = unsafe {
                K32GetMappedFileNameW(
                    GetCurrentProcess(),
                    memory.AllocationBase,
                    record.path.as_mut_ptr(),
                    PATH_UNITS as u32,
                )
            };
            record.words[13] = u64::from(length);
            record.words[14] = if length == 0 {
                0
            } else if length < PATH_UNITS as u32 {
                1
            } else {
                2
            };
        }
    }
    let mut written = 0;
    let _ = unsafe {
        WriteFile(
            file,
            ptr::from_ref(record).cast(),
            RECORD_BYTES as u32,
            &mut written,
            ptr::null_mut(),
        )
    };
}

pub(super) fn install(report: &Path) {
    if !matches!(
        std::env::var("PROTOINPUTMULTIPLEX_QUALIFICATION_FATAL_RECORD").as_deref(),
        Ok("private-av-v1")
    ) {
        return;
    }
    if !matches!(std::env::var("GITHUB_ACTIONS").as_deref(), Ok("true"))
        || !matches!(
            std::env::var("RUNNER_ENVIRONMENT").as_deref(),
            Ok("github-hosted")
        )
        || INITIALIZED.swap(true, Ordering::AcqRel)
    {
        return;
    }
    let Ok(file) = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(report.with_extension("fatal.private.bin"))
    else {
        return;
    };
    let mut times = [FILETIME::default(); 4];
    let observed = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut times[0],
            &mut times[1],
            &mut times[2],
            &mut times[3],
        )
    };
    if observed == 0 {
        return;
    }
    let creation = (u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime);
    if creation == 0 {
        return;
    }
    let record = unsafe { &mut *RECORD.0.get() };
    record.words = [
        MAGIC,
        1,
        RECORD_BYTES as u64,
        u64::from(std::process::id()),
        creation,
        1,
        u64::from(unsafe { GetErrorMode() }),
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        MAGIC,
    ];
    let mut written = 0;
    let stored = unsafe {
        WriteFile(
            file.as_raw_handle(),
            ptr::from_ref(record).cast(),
            RECORD_BYTES as u32,
            &mut written,
            ptr::null_mut(),
        )
    };
    if stored == 0 || written != RECORD_BYTES as u32 {
        return;
    }
    record.words[5] = 2;
    // The file and code remain valid for this disposable process's lifetime.
    FILE.store(file.into_raw_handle(), Ordering::Release);
    let previous = unsafe { SetUnhandledExceptionFilter(Some(unhandled)) };
    PREVIOUS.store(
        previous.map_or(0, |filter| filter as usize),
        Ordering::Release,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_preserves_previous_result_and_captures_only_first_nonresuming_av() {
        let claimed = AtomicBool::new(false);
        let calls = std::cell::Cell::new(0);
        for (code, result) in [(AV, -1), (1, 0), (AV, 17)] {
            let actual = filter_with(
                Some((code, 9)),
                || {
                    calls.set(calls.get() + 1);
                    result
                },
                &claimed,
                |_, _| panic!("ineligible exception must not be attributed"),
            );
            assert_eq!(actual, result);
            assert!(!claimed.load(Ordering::Acquire));
        }
        let mut observed = None;
        let result = filter_with(
            Some((AV, 9)),
            || {
                calls.set(calls.get() + 1);
                0
            },
            &claimed,
            |address, result| observed = Some((address, result)),
        );
        assert_eq!(result, 0);
        assert_eq!(observed, Some((9, 0)));
        assert_eq!(calls.get(), 4);
        let repeated = filter_with(Some((AV, 10)), || 1, &claimed, |_, _| unreachable!());
        assert_eq!(repeated, 1);
        let execute = AtomicBool::new(false);
        let mut address = None;
        let result = filter_with(Some((AV, 17)), || 1, &execute, |ip, _| address = Some(ip));
        assert_eq!(result, 1);
        assert_eq!(address, Some(17));
    }
}
