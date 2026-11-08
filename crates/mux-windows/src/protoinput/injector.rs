//! Launch the existing native injector without adopting its loader/pipe registry.
//! Its nonstandard raw command line is part of the pinned helper ABI.
use anyhow::{Context, Result, bail, ensure};
use std::{
    os::windows::ffi::OsStrExt,
    path::Path,
    ptr,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_CANCELLED, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
        Threading::{
            CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateProcessW, GetExitCodeProcess,
            PROCESS_INFORMATION, ResumeThread, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
        },
    },
};

pub(super) fn inject(
    identity: &crate::targets::ProcessIdentity,
    runtime: &Path,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<()> {
    inject_with_started(identity, runtime, deadline, cancel, |_| Ok(()))
}

fn inject_with_started(
    identity: &crate::targets::ProcessIdentity,
    runtime: &Path,
    deadline: Instant,
    cancel: &AtomicBool,
    on_started: impl FnOnce(&Helper) -> Result<()>,
) -> Result<()> {
    crate::process_arch::ensure_host_target(identity.bits)?;
    identity.ensure_alive()?;
    preflight(deadline, cancel)?;
    let runtime = runtime.canonicalize().context("resolve injector runtime")?;
    let helper = runtime.join(format!("ProtoInputIJ{}.exe", identity.bits));
    ensure!(
        helper.is_file(),
        "injector helper missing: {}",
        helper.display()
    );
    let mut child = Helper::spawn_with_job_setup(&helper, identity.pid, |child| {
        child.contain()?;
        // Process creation may have taken time. Do not resume stale work.
        preflight(deadline, cancel)?;
        identity.ensure_alive()
    })?;
    on_started(&child)?;
    loop {
        if let Err(error) = preflight(deadline, cancel).and_then(|()| identity.ensure_alive()) {
            child
                .stop()
                .context("stop owned injector after cancellation or target exit")?;
            return Err(error);
        }
        match unsafe { WaitForSingleObject(child.process, 10) } {
            WAIT_OBJECT_0 => {
                preflight(deadline, cancel)?;
                let mut status = 0;
                ensure!(
                    unsafe { GetExitCodeProcess(child.process, &mut status) } != 0,
                    "read injector result: {}",
                    std::io::Error::last_os_error()
                );
                ensure!(
                    status == 0,
                    "native injector failed with status {status:#010x}"
                );
                identity.ensure_alive()?;
                return Ok(());
            }
            WAIT_TIMEOUT => {}
            _ => bail!(
                "wait for native injector: {}",
                std::io::Error::last_os_error()
            ),
        }
    }
}

fn preflight(deadline: Instant, cancel: &AtomicBool) -> Result<()> {
    ensure!(!cancel.load(Ordering::Acquire), "injector cancelled");
    ensure!(Instant::now() < deadline, "injector deadline expired");
    Ok(())
}

fn command_line(pid: u32) -> Result<Vec<u16>> {
    ensure!(pid != 0, "invalid injector target PID");
    // Main.cpp parses GetCommandLineW itself and expects argv[0] to be the PID.
    Ok(format!("{pid} 0\0").encode_utf16().collect())
}

struct Helper {
    process: HANDLE,
    thread: HANDLE,
    job: HANDLE,
    job_assigned: bool,
}

impl Helper {
    #[cfg(test)]
    fn spawn(path: &Path, pid: u32) -> Result<Self> {
        Self::spawn_with_job_setup(path, pid, Self::contain)
    }

    fn spawn_with_job_setup(
        path: &Path,
        pid: u32,
        setup: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<Self> {
        ensure!(path.is_absolute(), "injector path must be absolute");
        let mut application: Vec<u16> = path.as_os_str().encode_wide().collect();
        ensure!(!application.contains(&0), "injector path contains NUL");
        application.push(0);
        let mut command = command_line(pid)?;
        let startup = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut process = PROCESS_INFORMATION::default();
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
                    &mut process,
                )
            } != 0,
            "start native injector: {}",
            std::io::Error::last_os_error()
        );
        let mut child = Self {
            process: process.hProcess,
            thread: process.hThread,
            job: ptr::null_mut(),
            job_assigned: false,
        };
        setup(&mut child)?;
        ensure!(
            child.job_assigned,
            "injector cannot run outside its owned Job"
        );
        ensure!(
            unsafe { ResumeThread(child.thread) } != u32::MAX,
            "resume native injector: {}",
            std::io::Error::last_os_error()
        );
        Ok(child)
    }

    fn contain(&mut self) -> Result<()> {
        // Contain descendants even in a standalone test process. In production
        // this nested Job is also covered by the supervisor's worker Job.
        self.job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        ensure!(
            !self.job.is_null(),
            "create injector Job: {}",
            std::io::Error::last_os_error()
        );
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        ensure!(
            unsafe {
                SetInformationJobObject(
                    self.job,
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of_val(&limits) as u32,
                )
            } != 0,
            "configure injector Job: {}",
            std::io::Error::last_os_error()
        );
        ensure!(
            unsafe { AssignProcessToJobObject(self.job, self.process) } != 0,
            "contain native injector: {}",
            std::io::Error::last_os_error()
        );
        self.job_assigned = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if unsafe { WaitForSingleObject(self.process, 0) } == WAIT_OBJECT_0 {
            return Ok(());
        }
        let stopped = unsafe {
            if !self.job_assigned {
                TerminateProcess(self.process, ERROR_CANCELLED)
            } else {
                TerminateJobObject(self.job, ERROR_CANCELLED)
            }
        };
        ensure!(
            stopped != 0 || unsafe { WaitForSingleObject(self.process, 0) } == WAIT_OBJECT_0,
            "terminate owned injector: {}",
            std::io::Error::last_os_error()
        );
        // Termination and I/O cleanup must finish before releasing owned handles.
        // This runs only in the disposable worker, under its supervisor deadline.
        ensure!(
            unsafe { WaitForSingleObject(self.process, u32::MAX) } == WAIT_OBJECT_0,
            "wait for terminated injector: {}",
            std::io::Error::last_os_error()
        );
        Ok(())
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.stop();
        unsafe {
            if !self.job.is_null() {
                CloseHandle(self.job);
            }
            CloseHandle(self.thread);
            CloseHandle(self.process);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::PathBuf, process::Command, sync::atomic::AtomicU64, time::Duration};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf, PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn build_fixture(mode: &str) -> Fixture {
        let path = std::env::temp_dir().join(format!(
            "protoinputmux-injector {}-{}-{mode}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let runtime = path.join(format!("中文 path-{mode}"));
        std::fs::create_dir_all(&runtime).unwrap();
        let fixture = Fixture(path, runtime);
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/injector.rs");
        let target = match (cfg!(target_arch = "x86"), cfg!(target_env = "msvc")) {
            (false, false) => "x86_64-pc-windows-gnu",
            (true, false) => "i686-pc-windows-gnu",
            (false, true) => "x86_64-pc-windows-msvc",
            (true, true) => "i686-pc-windows-msvc",
        };
        // GNU's linker can fail on Unicode output paths. Compile in the ASCII
        // parent, then copy so the actual CreateProcessW launch still tests one.
        let output = fixture.0.join("injector-fixture.exe");
        let status = Command::new("rustc")
            .args(["--edition=2024", "--crate-name", "injector_fixture"])
            .args(["--target", target])
            .arg(source)
            .arg("-o")
            .arg(&output)
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
        std::fs::copy(output, fixture.1.join(helper_name())).unwrap();
        fixture
    }

    fn helper_name() -> String {
        format!(
            "ProtoInputIJ{}.exe",
            if cfg!(target_arch = "x86") { 32 } else { 64 }
        )
    }

    #[test]
    fn raw_command_line_has_no_executable_token() {
        assert_eq!(
            String::from_utf16(&command_line(123).unwrap()).unwrap(),
            "123 0\0"
        );
        assert!(command_line(0).is_err());
        let fixture = build_fixture("ok");
        let identity = crate::targets::ProcessIdentity::open(std::process::id()).unwrap();
        inject(
            &identity,
            &fixture.1,
            Instant::now() + Duration::from_secs(5),
            &AtomicBool::new(false),
        )
        .unwrap();
    }

    #[test]
    fn helper_failure_is_reported_and_timeout_reaps_owned_process() {
        let fixture = build_fixture("fail");
        let identity = crate::targets::ProcessIdentity::open(std::process::id()).unwrap();
        let error = inject(
            &identity,
            &fixture.1,
            Instant::now() + Duration::from_secs(5),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.to_string().contains("0x00000011"), "{error:#}");
        let fixture = build_fixture("wait");
        let mut child = Helper::spawn(&fixture.1.join(helper_name()), identity.pid).unwrap();
        assert_eq!(
            unsafe { WaitForSingleObject(child.process, 50) },
            WAIT_TIMEOUT
        );
        child.stop().unwrap();
        assert_eq!(
            unsafe { WaitForSingleObject(child.process, 0) },
            WAIT_OBJECT_0
        );
        let error = inject(
            &identity,
            &fixture.1,
            Instant::now() + Duration::from_millis(80),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.to_string().contains("deadline"), "{error:#}");
    }

    #[test]
    fn cancellation_and_expiry_precede_helper_launch() {
        let identity = crate::targets::ProcessIdentity::open(std::process::id()).unwrap();
        let nonexistent = Path::new("C:\\missing-protoinputmux-injector-test");
        let error = inject(
            &identity,
            nonexistent,
            Instant::now() + Duration::from_secs(1),
            &AtomicBool::new(true),
        )
        .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        let error = inject(
            &identity,
            nonexistent,
            Instant::now() - Duration::from_millis(1),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.to_string().contains("deadline"));
    }

    #[test]
    fn cancellation_stops_an_already_running_helper() {
        let fixture = build_fixture("wait");
        let identity = crate::targets::ProcessIdentity::open(std::process::id()).unwrap();
        let cancel = AtomicBool::new(false);
        let name: Vec<u16> = format!(
            "Local\\ProtoInputMultiPlex.InjectorFixture.{}\0",
            fixture.0.file_name().unwrap().to_string_lossy()
        )
        .encode_utf16()
        .collect();
        struct Event(HANDLE);
        impl Drop for Event {
            fn drop(&mut self) {
                unsafe { CloseHandle(self.0) };
            }
        }
        let started = Event(unsafe {
            windows_sys::Win32::System::Threading::CreateEventW(ptr::null(), 1, 0, name.as_ptr())
        });
        assert!(!started.0.is_null());
        let mut original = None;
        let error = inject_with_started(
            &identity,
            &fixture.1,
            Instant::now() + Duration::from_secs(5),
            &cancel,
            |helper| {
                ensure!(
                    unsafe { WaitForSingleObject(started.0, 3000) } == WAIT_OBJECT_0,
                    "helper fixture did not execute its startup marker"
                );
                original = Some(crate::targets::ProcessIdentity::open(unsafe {
                    windows_sys::Win32::System::Threading::GetProcessId(helper.process)
                })?);
                cancel.store(true, Ordering::Release);
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("cancelled"), "{error:#}");
        assert_eq!(
            unsafe { WaitForSingleObject(original.unwrap().handle(), 0) },
            WAIT_OBJECT_0,
            "cancelled running helper survived return"
        );
    }

    #[test]
    fn failed_job_setup_reaps_suspended_helper_even_with_unassigned_job() {
        let fixture = build_fixture("wait");
        for create_unassigned_job in [false, true] {
            let mut original = None;
            let result = Helper::spawn_with_job_setup(
                &fixture.1.join(helper_name()),
                std::process::id(),
                |helper| {
                    let pid = unsafe {
                        windows_sys::Win32::System::Threading::GetProcessId(helper.process)
                    };
                    original = Some(crate::targets::ProcessIdentity::open(pid)?);
                    if create_unassigned_job {
                        helper.job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
                        ensure!(!helper.job.is_null(), "create test Job");
                    }
                    bail!("injected Job setup failure");
                },
            );
            assert!(result.is_err());
            let original = original.unwrap();
            assert_eq!(
                unsafe { WaitForSingleObject(original.handle(), 0) },
                WAIT_OBJECT_0,
                "failed Job setup left its suspended helper alive"
            );
        }
    }
}
