//! OS thread object, rather than an SDK pre-exit notification, fences DLL code.
use std::{
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    thread::JoinHandle,
};
use windows_sys::Win32::{
    Foundation::{DuplicateHandle, HANDLE, WAIT_OBJECT_0},
    System::Threading::{GetCurrentProcess, THREAD_SYNCHRONIZE, WaitForSingleObject},
};

pub(super) fn terminated(worker: &JoinHandle<()>) -> bool {
    // Rust's is_finished alone can precede native TLS/thread epilogue. Probe
    // the actual thread object first. Zero timeout never waits on the UI thread.
    (unsafe { WaitForSingleObject(worker.as_raw_handle() as HANDLE, 0) }) == WAIT_OBJECT_0
        && worker.is_finished()
}

pub(super) fn duplicate(worker: &JoinHandle<()>) -> Result<OwnedHandle, super::ClientError> {
    let process = unsafe { GetCurrentProcess() };
    let mut handle = std::ptr::null_mut();
    // Explicit minimal access, non-inheritable, no DUPLICATE_SAME_ACCESS.
    if unsafe {
        DuplicateHandle(
            process,
            worker.as_raw_handle() as HANDLE,
            process,
            &mut handle,
            THREAD_SYNCHRONIZE,
            0,
            0,
        )
    } == 0
    {
        return Err(super::ClientError::Limit);
    }
    // SAFETY: successful DuplicateHandle transfers one independently owned handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}
