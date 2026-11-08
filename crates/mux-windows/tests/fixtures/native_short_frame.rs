//! Actual native receiver short-read rejection before any lifecycle Begin.
//! No production attachment, device input, lease, quarantine or commit is tested.
use super::*;
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::{
    Foundation::{
        ERROR_BROKEN_PIPE, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED,
        GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX, WriteFile,
    },
    System::{
        IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        },
        Memory::{FILE_MAP_READ, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile},
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
            PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
            PeekNamedPipe,
        },
        Threading::{
            CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateEventW, CreateProcessW, GetExitCodeProcess,
            PROCESS_INFORMATION, ResumeThread, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
        },
    },
};

const CURRENT_BUILD: u64 = 0x2026_1107_0000_0001;
const LEGACY_BEGIN_ID: u32 = 30;
const LEGACY_BEGIN_BYTES: u32 = 48;

struct Handle(HANDLE);

impl Handle {
    fn checked(raw: HANDLE, operation: &str) -> Result<Self> {
        ensure!(
            !raw.is_null() && raw != INVALID_HANDLE_VALUE,
            "{operation}: {}",
            std::io::Error::last_os_error()
        );
        Ok(Self(raw))
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

struct Injector {
    process: Handle,
    _thread: Handle,
    job: Option<Handle>,
}

impl Injector {
    fn launch(runtime: &Path, pid: u32, bits: u32) -> Result<Self> {
        let image = runtime.join(format!("ProtoInputIJ{bits}.exe"));
        for name in [
            format!("ProtoInputIJ{bits}.exe"),
            format!("ProtoInputIJP{bits}.dll"),
            format!("ProtoInputHooks{bits}.dll"),
        ] {
            ensure!(
                runtime.join(&name).is_file(),
                "matching test runtime lacks {name}"
            );
        }
        let mut application: Vec<u16> = image.as_os_str().encode_wide().collect();
        ensure!(!application.contains(&0), "injector path contains NUL");
        application.push(0);
        // Pinned Main.cpp expects argv[0]=PID and argv[1]=0, with no image token.
        let mut command = wide(&format!("{pid} 0"));
        let startup = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut created = PROCESS_INFORMATION::default();
        ensure!(
            unsafe {
                CreateProcessW(
                    application.as_ptr(),
                    command.as_mut_ptr(),
                    ptr::null(),
                    ptr::null(),
                    0,
                    CREATE_NO_WINDOW | CREATE_SUSPENDED,
                    ptr::null(),
                    ptr::null(),
                    &startup,
                    &mut created,
                )
            } != 0,
            "create owned native short-frame injector: {}",
            std::io::Error::last_os_error()
        );
        // Own both handles before the fallible Job setup. Drop always terminates
        // the exact root process, including when an empty Job was never assigned.
        let mut injector = Self {
            process: Handle(created.hProcess),
            _thread: Handle(created.hThread),
            job: None,
        };
        let job = Handle::checked(
            unsafe { CreateJobObjectW(ptr::null(), ptr::null()) },
            "create short-frame injector Job",
        )?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        ensure!(
            unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of_val(&limits) as u32,
                )
            } != 0,
            "configure short-frame injector Job: {}",
            std::io::Error::last_os_error()
        );
        ensure!(
            unsafe { AssignProcessToJobObject(job.0, injector.process.0) } != 0,
            "contain short-frame injector: {}",
            std::io::Error::last_os_error()
        );
        injector.job = Some(job);
        ensure!(
            unsafe { ResumeThread(injector._thread.0) } != u32::MAX,
            "resume short-frame injector: {}",
            std::io::Error::last_os_error()
        );
        Ok(injector)
    }

    fn wait(&self, deadline: Instant, watch: &targets::ProcessWatch) -> Result<()> {
        loop {
            ensure!(
                watch.exited().is_none(),
                "original short-frame target exited during injection"
            );
            ensure!(
                Instant::now() < deadline,
                "native short-frame injection timed out"
            );
            match unsafe { WaitForSingleObject(self.process.0, 10) } {
                WAIT_OBJECT_0 => {
                    let mut code = 0;
                    ensure!(
                        unsafe { GetExitCodeProcess(self.process.0, &mut code) } != 0,
                        "read owned injector result: {}",
                        std::io::Error::last_os_error()
                    );
                    ensure!(
                        code == 0,
                        "native short-frame injector failed: {code:#010x}"
                    );
                    return Ok(());
                }
                WAIT_TIMEOUT => {}
                _ => bail!(
                    "wait short-frame injector: {}",
                    std::io::Error::last_os_error()
                ),
            }
        }
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        unsafe {
            if WaitForSingleObject(self.process.0, 0) != WAIT_OBJECT_0 {
                TerminateProcess(self.process.0, 1223);
            }
        }
        // Close the Job to contain any helper descendants before reaping root.
        self.job.take();
        unsafe { WaitForSingleObject(self.process.0, u32::MAX) };
    }
}

struct Operation<'a> {
    pipe: &'a Handle,
    overlapped: OVERLAPPED,
    _event: Handle,
    pending: bool,
}

impl<'a> Operation<'a> {
    fn new(pipe: &'a Handle) -> Result<Self> {
        let event = Handle::checked(
            unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) },
            "create short-frame I/O event",
        )?;
        Ok(Self {
            pipe,
            overlapped: OVERLAPPED {
                hEvent: event.0,
                ..Default::default()
            },
            _event: event,
            pending: false,
        })
    }

    fn complete(&mut self) -> Result<u32> {
        let mut transferred = 0;
        let ok = unsafe { GetOverlappedResult(self.pipe.0, &self.overlapped, &mut transferred, 0) };
        if ok == 0 {
            let error = unsafe { GetLastError() };
            if error != ERROR_IO_INCOMPLETE {
                self.pending = false;
            }
            bail!(
                "complete short-frame pipe I/O: {}",
                std::io::Error::from_raw_os_error(error as i32)
            );
        }
        self.pending = false;
        Ok(transferred)
    }

    fn wait(&mut self, deadline: Instant, watch: &targets::ProcessWatch) -> Result<u32> {
        loop {
            ensure!(
                watch.exited().is_none(),
                "original short-frame target exited during I/O"
            );
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(!remaining.is_zero(), "short-frame pipe I/O timed out");
            match unsafe {
                WaitForSingleObject(
                    self.overlapped.hEvent,
                    remaining.as_millis().clamp(1, 10) as u32,
                )
            } {
                WAIT_OBJECT_0 => return self.complete(),
                WAIT_TIMEOUT => {}
                _ => bail!(
                    "wait short-frame pipe I/O: {}",
                    std::io::Error::last_os_error()
                ),
            }
        }
    }
}

impl Drop for Operation<'_> {
    fn drop(&mut self) {
        if self.pending {
            // The OS may complete cancellation after the requested deadline.
            // Keep OVERLAPPED/event/caller bytes alive through the final drain.
            unsafe {
                CancelIoEx(self.pipe.0, &self.overlapped);
                let mut transferred = 0;
                GetOverlappedResult(self.pipe.0, &self.overlapped, &mut transferred, 1);
            }
        }
    }
}

struct ProbePipe(Handle);

impl ProbePipe {
    fn create(target: &SelectedTarget) -> Result<Self> {
        let name = wide(&format!(
            "\\\\.\\pipe\\ProtoInputMultiPlex.Lifecycle.{}.{}",
            target.pid, target.creation
        ));
        // Fixture-only duplex permits a nonwriting read-side liveness probe.
        // Native CreateFile(GENERIC_READ) is valid on a duplex server:
        // https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-client
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                8192,
                8192,
                0,
                ptr::null(),
            )
        };
        Ok(Self(Handle::checked(
            handle,
            "create first-instance short-frame pipe",
        )?))
    }

    fn connect(
        &self,
        target: &SelectedTarget,
        deadline: Instant,
        watch: &targets::ProcessWatch,
    ) -> Result<()> {
        let mut operation = Operation::new(&self.0)?;
        if unsafe { ConnectNamedPipe(self.0.0, &mut operation.overlapped) } == 0 {
            match unsafe { GetLastError() } {
                ERROR_PIPE_CONNECTED => {}
                ERROR_IO_PENDING => {
                    operation.pending = true;
                    operation.wait(deadline, watch)?;
                }
                error => bail!(
                    "connect short-frame pipe: {}",
                    std::io::Error::from_raw_os_error(error as i32)
                ),
            }
        }
        let mut pid = 0;
        ensure!(
            unsafe { GetNamedPipeClientProcessId(self.0.0, &mut pid) } != 0 && pid == target.pid,
            "short-frame pipe did not authenticate its original target PID"
        );
        ensure!(
            watch.exited().is_none(),
            "authenticated original target exited"
        );
        Ok(())
    }

    fn send_fragment(
        &self,
        bytes: &[u8],
        deadline: Instant,
        watch: &targets::ProcessWatch,
    ) -> Result<()> {
        ensure!(
            Instant::now() < deadline,
            "short-frame send deadline expired"
        );
        ensure!(
            watch.exited().is_none(),
            "original target exited before short-frame send"
        );
        let mut operation = Operation::new(&self.0)?;
        let accepted = unsafe {
            WriteFile(
                self.0.0,
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
                "short-frame fragment write: {}",
                std::io::Error::from_raw_os_error(error as i32)
            );
        }
        operation.pending = true;
        let transferred = if accepted == 0 {
            operation.wait(deadline, watch)?
        } else {
            operation.complete()?
        };
        ensure!(
            transferred as usize == bytes.len(),
            "fixture fragment was itself written short"
        );
        Ok(())
    }

    fn peer_closed(&self) -> Result<bool> {
        // No bytes are written or removed by this probe. OVERLAPPED creation
        // avoids the synchronous-handle multi-thread blocking caveat documented
        // at https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-peeknamedpipe.
        let ok = unsafe {
            PeekNamedPipe(
                self.0.0,
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if ok != 0 {
            return Ok(false);
        }
        let error = unsafe { GetLastError() };
        ensure!(
            error == ERROR_BROKEN_PIPE,
            "unexpected short-frame PeekNamedPipe error {error}"
        );
        Ok(true)
    }
}

impl Drop for ProbePipe {
    fn drop(&mut self) {
        unsafe { DisconnectNamedPipe(self.0.0) };
    }
}

struct ReadyView {
    _mapping: Handle,
    view: windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS,
}

impl ReadyView {
    fn open(target: &SelectedTarget) -> Result<Self> {
        let name = wide(&format!(
            "Local\\ProtoInputMultiPlex.Lifecycle.{}.{}",
            target.pid, target.creation
        ));
        let mapping = Handle::checked(
            unsafe { OpenFileMappingW(FILE_MAP_READ, 0, name.as_ptr()) },
            "open short-frame readiness",
        )?;
        let view = unsafe { MapViewOfFile(mapping.0, FILE_MAP_READ, 0, 0, 128) };
        ensure!(
            !view.Value.is_null(),
            "map short-frame readiness: {}",
            std::io::Error::last_os_error()
        );
        Ok(Self {
            _mapping: mapping,
            view,
        })
    }

    fn assert_unclaimed(&self, target: &SelectedTarget, bits: u32) -> Result<()> {
        for _ in 0..32 {
            let record = unsafe { ptr::read_volatile(self.view.Value.cast::<[u8; 128]>()) };
            std::sync::atomic::fence(Ordering::Acquire);
            let second =
                unsafe { ptr::read_volatile(self.view.Value.cast::<u8>().add(12).cast::<u32>()) };
            let word = |offset| u32::from_le_bytes(record[offset..offset + 4].try_into().unwrap());
            let value = |offset| u64::from_le_bytes(record[offset..offset + 8].try_into().unwrap());
            if word(12) != 0 && word(12) & 1 == 0 && word(12) == second {
                ensure!(
                    word(0) == 0x4d58_5244 && word(4) == 1 && word(8) == 128,
                    "invalid short-frame readiness ABI"
                );
                ensure!(
                    word(24) == bits && value(32) == target.creation && value(56) == CURRENT_BUILD,
                    "short-frame readiness is not the matching original build/architecture/creation"
                );
                ensure!(
                    word(16) == 1
                        && word(20) == 0
                        && word(28) == 0
                        && value(40) == 0
                        && value(48) == 0,
                    "short-frame target acquired lifecycle state before a complete Begin"
                );
                return Ok(());
            }
            std::thread::yield_now();
        }
        bail!("short-frame readiness did not produce a coherent snapshot")
    }
}

impl Drop for ReadyView {
    fn drop(&mut self) {
        unsafe { UnmapViewOfFile(self.view) };
    }
}

fn fixture_bits() -> Result<u32> {
    let bytes = std::fs::read(target_executable()?)?;
    ensure!(
        bytes.get(..2) == Some(b"MZ"),
        "target fixture lacks DOS header"
    );
    let offset = bytes
        .get(0x3c..0x40)
        .context("target fixture lacks PE offset")?;
    let offset = u32::from_le_bytes(offset.try_into()?) as usize;
    let pe = bytes
        .get(offset..)
        .context("target fixture PE offset out of bounds")?;
    ensure!(
        pe.get(..4) == Some(b"PE\0\0"),
        "target fixture lacks PE header"
    );
    match pe.get(4..6) {
        Some([0x4c, 0x01]) => Ok(32),
        Some([0x64, 0x86]) => Ok(64),
        _ => bail!("short-frame fixture requires an Intel x86/x64 target"),
    }
}

#[test]
#[ignore = "opt-in native receiver short-frame injection into newly owned targets only"]
fn native_receiver_rejects_short_header_and_body_before_begin() -> Result<()> {
    let runtime = PathBuf::from(std::env::var_os(RUNTIME).context("native runtime opt-in")?)
        .canonicalize()?;
    let bits = fixture_bits()?;
    for short_body in [false, true] {
        let mut fixture = spawn_target()?;
        let target = &fixture.selected;
        let watch = targets::ProcessWatch::open([target.pid])?;
        validate_owned_target(target, std::process::id())?;
        let before = evidence(target)?;
        let pipe = ProbePipe::create(target)?;
        let deadline = Instant::now() + POLL_LIMIT;
        let helper = Injector::launch(&runtime, target.pid, bits)?;
        helper.wait(deadline, &watch)?;
        pipe.connect(target, deadline, &watch)?;
        validate_owned_target(target, std::process::id())?;
        let ready = ReadyView::open(target)?;
        ready.assert_unclaimed(target, bits)?;
        // Establish that the live read-only client does not already look broken
        // on this duplex probe before interpreting a subsequent broken-pipe edge.
        ensure!(
            !pipe.peer_closed()?,
            "short-frame peer closed before any fragment"
        );
        let mut header = LEGACY_BEGIN_ID.to_le_bytes().to_vec();
        header.extend_from_slice(&LEGACY_BEGIN_BYTES.to_le_bytes());
        let rejection_started = Instant::now();
        if short_body {
            pipe.send_fragment(&header, deadline, &watch)?;
            pipe.send_fragment(&[0; 8], deadline, &watch)?;
        } else {
            pipe.send_fragment(&header[..4], deadline, &watch)?;
        }
        // ReadFile may complete when the writer completes its fragment, rather
        // than accumulating the requested8/48 bytes. We never supply the rest:
        // https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-readfile.
        // Crucially the server stays open until AFTER this nonwriting teardown
        // observation. EOF caused by sender close cannot satisfy this assertion.
        loop {
            ensure!(
                watch.exited().is_none(),
                "target exit cannot prove short-frame rejection"
            );
            if pipe.peer_closed()? {
                break;
            }
            ensure!(
                Instant::now() < deadline,
                "native receiver retained the truncated stream"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        ensure!(
            Instant::now() < deadline,
            "native short-frame rejection missed its deadline"
        );
        ensure!(
            fixture.process.child.try_wait()?.is_none(),
            "original short-frame target did not survive"
        );
        validate_owned_target(target, std::process::id())?;
        ready.assert_unclaimed(target, bits)?;
        ensure!(
            evidence(target)? == before,
            "short frame produced input messages"
        );
        println!(
            "MIGRATION_NATIVE_SHORT_FRAME case={} controller={}bit target={bits}bit original={} receiver_closed_while_server_open_ns={} current_build=0x{CURRENT_BUILD:016x} generation=0 session=0 input_unchanged; no_lease_no_commit_no_quarantine_claim",
            if short_body {
                "body8_of48"
            } else {
                "header4_of8"
            },
            usize::BITS,
            target.pid,
            rejection_started.elapsed().as_nanos(),
        );
        // Only after all evidence has been checked may these RAII owners close
        // the test server, helper resources, and eventually the owned target.
    }
    Ok(())
}
