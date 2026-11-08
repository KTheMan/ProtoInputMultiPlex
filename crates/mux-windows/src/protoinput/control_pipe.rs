//! The host's single-owner lifecycle pipe. A write completion is transport
//! progress, never a target acknowledgement or permission to reuse a target.
use anyhow::{Context, Result, bail, ensure};
use std::{
    fmt, ptr,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, GetLastError,
        HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_OUTBOUND, WriteFile,
    },
    System::{
        IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
            PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
        },
        Threading::{CreateEventW, WaitForSingleObject},
    },
};

/// Once submission starts, duplicated remote handles cannot be reclaimed by the
/// sender: even a failed/partial write can have exposed a command to the target.
#[derive(Debug)]
pub(super) enum SendError {
    NotStarted(anyhow::Error),
    TransferStarted(anyhow::Error),
}

impl SendError {
    #[cfg(test)]
    pub(super) fn transfer_started(&self) -> bool {
        matches!(self, Self::TransferStarted(_))
    }

    pub(super) fn cancelled_before_transfer(&self) -> bool {
        matches!(self, Self::NotStarted(error) if error.is::<Cancelled>())
    }
}

#[derive(Debug)]
pub(super) struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("control-pipe operation cancelled before transfer")
    }
}

impl std::error::Error for Cancelled {}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotStarted(error) => write!(f, "control-pipe transfer not started: {error:#}"),
            Self::TransferStarted(error) => write!(f, "control-pipe transfer uncertain: {error:#}"),
        }
    }
}

impl std::error::Error for SendError {}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

pub(super) struct ControlPipe {
    pipe: Option<OwnedHandle>,
    original: crate::targets::ProcessIdentity,
    connected: bool,
    poisoned: bool,
}

impl ControlPipe {
    /// Create before injection so a resident target cannot connect to a different
    /// server first. Retain the original process object, never reopen its PID.
    pub(super) fn create(original: &crate::targets::ProcessIdentity) -> Result<Self> {
        let original = original.try_clone()?;
        ensure!(
            original.pid != 0 && original.creation != 0,
            "invalid process identity"
        );
        let name: Vec<u16> = format!(
            "\\\\.\\pipe\\ProtoInputMultiPlex.Lifecycle.{}.{}\0",
            original.pid, original.creation
        )
        .encode_utf16()
        .collect();
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_OUTBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                8192,
                8192,
                0,
                ptr::null(),
            )
        };
        ensure!(
            handle != INVALID_HANDLE_VALUE,
            "create lifecycle control pipe: {}",
            std::io::Error::last_os_error()
        );
        Ok(Self {
            pipe: Some(OwnedHandle(handle)),
            original,
            connected: false,
            poisoned: false,
        })
    }

    pub(super) fn connect(&mut self, deadline: Instant, cancel: &AtomicBool) -> Result<()> {
        ensure!(!self.connected, "control pipe already connected");
        self.preflight(deadline, cancel)?;
        let pipe = self.handle()?;
        let result = (|| {
            let mut operation = Operation::new(pipe)?;
            let accepted = unsafe { ConnectNamedPipe(pipe, &mut operation.overlapped) };
            if accepted == 0 {
                match unsafe { GetLastError() } {
                    ERROR_PIPE_CONNECTED => {}
                    ERROR_IO_PENDING => {
                        operation.pending = true;
                        operation.wait(deadline, cancel, &self.original)?;
                    }
                    error => bail!("connect lifecycle control pipe: {}", os_error(error)),
                }
            }
            self.original.ensure_alive()?;
            let mut actual_pid = 0;
            ensure!(
                unsafe { GetNamedPipeClientProcessId(pipe, &mut actual_pid) } != 0,
                "authenticate lifecycle pipe client: {}",
                std::io::Error::last_os_error()
            );
            ensure!(
                actual_pid == self.original.pid,
                "unexpected lifecycle pipe client PID {actual_pid}"
            );
            self.original.ensure_alive()?;
            Ok(())
        })();
        if result.is_err() {
            self.poison();
        } else {
            self.connected = true;
        }
        result
    }

    /// Exactly one header write followed by exactly one body write. The native
    /// receiver does one ReadFile per segment, so remainder retries are unsafe.
    pub(super) fn send(
        &mut self,
        header: &[u8; 8],
        body: &[u8],
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> std::result::Result<(), SendError> {
        let preflight = (|| {
            ensure!(self.connected, "control pipe is not connected");
            self.preflight(deadline, cancel)?;
            ensure!(
                body.len() <= u32::MAX as usize,
                "control-pipe body too large"
            );
            ensure!(
                u32::from_le_bytes(header[4..].try_into().expect("fixed header")) as usize
                    == body.len(),
                "control-pipe header/body length mismatch"
            );
            Ok(())
        })();
        preflight.map_err(SendError::NotStarted)?;
        let pipe = self.handle().map_err(SendError::NotStarted)?;
        let mut started = false;
        let result = (|| {
            self.write_segment(pipe, header, deadline, cancel, &mut started)?;
            self.write_segment(pipe, body, deadline, cancel, &mut started)?;
            Ok(())
        })();
        match result {
            Ok(()) => Ok(()),
            Err(error) if started => {
                self.poison();
                Err(SendError::TransferStarted(error))
            }
            Err(error) => Err(SendError::NotStarted(error)),
        }
    }

    fn write_segment(
        &self,
        pipe: HANDLE,
        bytes: &[u8],
        deadline: Instant,
        cancel: &AtomicBool,
        started: &mut bool,
    ) -> Result<()> {
        self.preflight(deadline, cancel)?;
        let mut operation = Operation::new(pipe)?;
        // Mark before WriteFile, including immediate error/short-write paths.
        *started = true;
        let accepted = unsafe {
            WriteFile(
                pipe,
                bytes.as_ptr(),
                bytes.len() as u32,
                ptr::null_mut(),
                &mut operation.overlapped,
            )
        };
        if accepted == 0 {
            let error = unsafe { GetLastError() };
            ensure!(
                error == ERROR_IO_PENDING,
                "write control-pipe segment: {}",
                os_error(error)
            );
        }
        // Also retain storage if an allegedly synchronous completion cannot yet
        // be confirmed by GetOverlappedResult.
        operation.pending = true;
        let written = if accepted == 0 {
            operation.wait(deadline, cancel, &self.original)?
        } else {
            operation.completed()?
        };
        ensure!(
            written as usize == bytes.len(),
            "short control-pipe segment write"
        );
        self.preflight(deadline, cancel)?;
        Ok(())
    }

    fn handle(&self) -> Result<HANDLE> {
        ensure!(!self.poisoned, "control pipe is poisoned");
        self.pipe
            .as_ref()
            .map(|handle| handle.0)
            .context("control pipe is closed")
    }

    fn preflight(&self, deadline: Instant, cancel: &AtomicBool) -> Result<()> {
        self.handle()?;
        if cancel.load(Ordering::Acquire) {
            return Err(anyhow::Error::new(Cancelled));
        }
        ensure!(
            Instant::now() < deadline,
            "control-pipe operation timed out"
        );
        self.original.ensure_alive()
    }

    fn poison(&mut self) {
        self.poisoned = true;
        let _ = self.close();
    }

    /// Operations never outlive their call. Pending cancellation is drained before
    /// this can close the handle or free the operation's event/buffer/storage.
    pub(super) fn close(&mut self) -> Result<()> {
        self.connected = false;
        if let Some(pipe) = self.pipe.take() {
            unsafe { DisconnectNamedPipe(pipe.0) };
            let handle = pipe.0;
            std::mem::forget(pipe);
            ensure!(
                unsafe { CloseHandle(handle) } != 0,
                "close lifecycle control pipe: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(())
    }
}

impl Drop for ControlPipe {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

/// Kept in one stack location from submission through completion. Drop also
/// drains cancellation during unwinding; OVERLAPPED and caller bytes stay alive.
struct Operation {
    pipe: HANDLE,
    overlapped: OVERLAPPED,
    _event: OwnedHandle,
    pending: bool,
}

impl Operation {
    fn new(pipe: HANDLE) -> Result<Self> {
        let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        ensure!(
            !event.is_null(),
            "create control-pipe I/O event: {}",
            std::io::Error::last_os_error()
        );
        Ok(Self {
            pipe,
            overlapped: OVERLAPPED {
                hEvent: event,
                ..Default::default()
            },
            _event: OwnedHandle(event),
            pending: false,
        })
    }

    fn completed(&mut self) -> Result<u32> {
        let mut bytes = 0;
        let ok = unsafe { GetOverlappedResult(self.pipe, &self.overlapped, &mut bytes, 0) };
        if ok == 0 {
            let error = unsafe { GetLastError() };
            if error != ERROR_IO_INCOMPLETE {
                self.pending = false;
            }
            bail!("complete control-pipe I/O: {}", os_error(error));
        }
        self.pending = false;
        Ok(bytes)
    }

    fn wait(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
        original: &crate::targets::ProcessIdentity,
    ) -> Result<u32> {
        loop {
            ensure!(
                !cancel.load(Ordering::Acquire),
                "control-pipe operation cancelled"
            );
            original.ensure_alive()?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(!remaining.is_zero(), "control-pipe operation timed out");
            let wait_ms = remaining.as_millis().clamp(1, 10) as u32;
            match unsafe { WaitForSingleObject(self.overlapped.hEvent, wait_ms) } {
                WAIT_OBJECT_0 => return self.completed(),
                WAIT_TIMEOUT => {}
                WAIT_FAILED => bail!("wait control-pipe I/O: {}", std::io::Error::last_os_error()),
                status => bail!("unexpected control-pipe wait status {status}"),
            }
        }
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        if self.pending {
            // CancelIoEx may race completion and report NOT_FOUND. Either way the
            // final wait is mandatory before these addresses can be reclaimed.
            // Windows cancellation completion itself has no hard deadline.
            unsafe {
                CancelIoEx(self.pipe, &self.overlapped);
                let mut bytes = 0;
                GetOverlappedResult(self.pipe, &self.overlapped, &mut bytes, 1);
            }
            self.pending = false;
        }
    }
}

fn os_error(error: u32) -> std::io::Error {
    std::io::Error::from_raw_os_error(error as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::OpenOptions,
        os::windows::io::AsRawHandle,
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::{Mutex, mpsc},
        thread,
        time::Duration,
    };

    // The production pipe name is exact, so self-process peers must serialize.
    static SELF_PIPE: Mutex<()> = Mutex::new(());

    fn with_peer<T>(
        peer: impl FnOnce(std::fs::File, mpsc::Receiver<()>) -> Result<T> + Send + 'static,
        action: impl FnOnce(&mut ControlPipe) -> Result<()>,
    ) -> Result<T>
    where
        T: Send + 'static,
    {
        let _guard = SELF_PIPE.lock().unwrap_or_else(|error| error.into_inner());
        let identity = crate::targets::ProcessIdentity::open(std::process::id())?;
        let path = format!(
            "\\\\.\\pipe\\ProtoInputMultiPlex.Lifecycle.{}.{}",
            identity.pid, identity.creation
        );
        let mut pipe = ControlPipe::create(&identity)?;
        let (release, released) = mpsc::channel();
        let worker = thread::spawn(move || {
            let file = OpenOptions::new().read(true).open(path)?;
            peer(file, released)
        });
        let result = catch_unwind(AssertUnwindSafe(|| action(&mut pipe)));
        // Always disconnect before joining a reader, including assertion/unwind
        // paths. The peer cannot retain a blocking read after the test exits.
        drop(pipe);
        let _ = release.send(());
        let peer_result = worker.join().expect("owned pipe peer panicked");
        match result {
            Ok(result) => result?,
            Err(panic) => resume_unwind(panic),
        }
        peer_result
    }

    #[test]
    fn exact_segments_reach_authenticated_peer_and_first_instance_is_exclusive() -> Result<()> {
        let frame = super::super::wire::Command::Hook {
            hook: 3,
            install: true,
        }
        .encode_for(64)?;
        let (observed, observation) = mpsc::channel();
        let received = with_peer(
            move |file, release| {
                // Match the native receiver: one ReadFile for the fixed header,
                // then one for its announced body, with no short-read retries.
                let mut header = [0u8; 8];
                read_native_segment(&file, &mut header)?;
                ensure!(u32::from_le_bytes(header[..4].try_into().unwrap()) == 0);
                let size = u32::from_le_bytes(header[4..].try_into().unwrap()) as usize;
                ensure!(size == 8);
                let mut body = vec![0u8; size];
                read_native_segment(&file, &mut body)?;
                ensure!(body == [3, 0, 0, 0, 1, 0, 0, 0]);
                observed.send(())?;
                let _ = release.recv();
                Ok((header, body))
            },
            |pipe| {
                ensure!(ControlPipe::create(&pipe.original).is_err());
                let cancel = AtomicBool::new(false);
                let deadline = Instant::now() + Duration::from_secs(2);
                pipe.connect(deadline, &cancel)?;
                ensure!(matches!(
                    pipe.send(&[0; 8], &frame.body, deadline, &cancel),
                    Err(SendError::NotStarted(_))
                ));
                cancel.store(true, Ordering::Release);
                ensure!(matches!(
                    pipe.send(&frame.header, &frame.body, deadline, &cancel),
                    Err(SendError::NotStarted(_))
                ));
                cancel.store(false, Ordering::Release);
                pipe.send(&frame.header, &frame.body, deadline, &cancel)?;
                // Completion means bytes entered the pipe, not that the target
                // consumed them. Keep the server alive for the peer's receipt.
                observation.recv_timeout(Duration::from_secs(2))?;
                Ok(())
            },
        )?;
        assert_eq!(received.0, frame.header);
        assert_eq!(received.1, frame.body);
        Ok(())
    }

    fn read_native_segment(file: &std::fs::File, bytes: &mut [u8]) -> Result<()> {
        let mut read = 0;
        ensure!(
            unsafe {
                windows_sys::Win32::Storage::FileSystem::ReadFile(
                    file.as_raw_handle(),
                    bytes.as_mut_ptr(),
                    bytes.len() as u32,
                    &mut read,
                    ptr::null_mut(),
                )
            } != 0,
            "native-style peer read: {}",
            std::io::Error::last_os_error()
        );
        ensure!(read as usize == bytes.len(), "native-style peer short read");
        Ok(())
    }

    fn fill_nonreading_pipe(
        pipe: &mut ControlPipe,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> SendError {
        let body = [1u8; 4096];
        let header = [77, 0, 0, 0, 0, 16, 0, 0];
        for _ in 0..128 {
            if let Err(error) = pipe.send(&header, &body, deadline, cancel) {
                return error;
            }
        }
        panic!("nonreading owned peer unexpectedly accepted 512 KiB");
    }

    #[test]
    fn nonreading_peer_times_out_and_poisoned_pipe_never_retries() -> Result<()> {
        with_peer(
            |_file, release| {
                let _ = release.recv();
                Ok(())
            },
            |pipe| {
                let cancel = AtomicBool::new(false);
                pipe.connect(Instant::now() + Duration::from_secs(2), &cancel)?;
                let error = fill_nonreading_pipe(
                    pipe,
                    Instant::now() + Duration::from_millis(150),
                    &cancel,
                );
                ensure!(
                    error.transfer_started(),
                    "blocked write must retain uncertain ownership"
                );
                ensure!(pipe.poisoned && pipe.pipe.is_none());
                ensure!(matches!(
                    pipe.send(
                        &[0; 8],
                        &[],
                        Instant::now() + Duration::from_secs(1),
                        &cancel
                    ),
                    Err(SendError::NotStarted(_))
                ));
                Ok(())
            },
        )
    }

    #[test]
    fn pending_write_cancellation_is_drained_before_pipe_closes() -> Result<()> {
        with_peer(
            |_file, release| {
                let _ = release.recv();
                Ok(())
            },
            |pipe| {
                let cancel = AtomicBool::new(false);
                pipe.connect(Instant::now() + Duration::from_secs(2), &cancel)?;
                thread::scope(|scope| {
                    scope.spawn(|| {
                        thread::sleep(Duration::from_millis(100));
                        cancel.store(true, Ordering::Release);
                    });
                    let error = fill_nonreading_pipe(
                        pipe,
                        Instant::now() + Duration::from_secs(2),
                        &cancel,
                    );
                    ensure!(error.transfer_started());
                    ensure!(error.to_string().contains("cancelled"));
                    ensure!(pipe.poisoned && pipe.pipe.is_none());
                    Ok(())
                })
            },
        )
    }

    #[test]
    fn connect_timeout_drains_pending_operation_and_releases_first_instance() -> Result<()> {
        let _guard = SELF_PIPE.lock().unwrap_or_else(|error| error.into_inner());
        let identity = crate::targets::ProcessIdentity::open(std::process::id())?;
        let mut pipe = ControlPipe::create(&identity)?;
        let cancel = AtomicBool::new(false);
        ensure!(
            pipe.connect(Instant::now() + Duration::from_millis(50), &cancel)
                .is_err()
        );
        ensure!(pipe.poisoned && pipe.pipe.is_none());
        let _replacement = ControlPipe::create(&identity)?;
        Ok(())
    }

    #[test]
    fn eof_after_authenticated_connect_poisoned_instead_of_retrying() -> Result<()> {
        let (closed, was_closed) = mpsc::channel();
        let (disconnect, disconnected) = mpsc::channel();
        with_peer(
            move |file, _release| {
                disconnected.recv_timeout(Duration::from_secs(2))?;
                drop(file);
                closed.send(())?;
                Ok(())
            },
            |pipe| {
                let cancel = AtomicBool::new(false);
                let deadline = Instant::now() + Duration::from_secs(2);
                pipe.connect(deadline, &cancel)?;
                disconnect.send(())?;
                was_closed.recv_timeout(Duration::from_secs(2))?;
                ensure!(matches!(
                    pipe.send(&[0; 8], &[], deadline, &cancel),
                    Err(SendError::TransferStarted(_))
                ));
                ensure!(pipe.poisoned && pipe.pipe.is_none());
                Ok(())
            },
        )
    }
}
