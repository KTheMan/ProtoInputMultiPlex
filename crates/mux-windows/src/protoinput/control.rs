//! Complete host-side command ownership for supervised virtual attachments.
//! No native loader instance or function pointer is used by these connections.
use super::{
    control_pipe::{ControlPipe, SendError},
    injector,
    wire::Command,
};
use crate::queue_fence::Recipient;
use anyhow::{Context, Result, ensure};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_NULL};

struct Channel {
    pipe: ControlPipe,
    identity: crate::targets::ProcessIdentity,
    setup: Option<crate::lifecycle::SetupChannel>,
}

pub(crate) struct Registry {
    channels: RefCell<HashMap<u32, Channel>>,
    next: Cell<u32>,
    cancel: Arc<AtomicBool>,
}

impl Registry {
    pub(crate) fn new(cancel: Arc<AtomicBool>) -> Self {
        Self {
            channels: RefCell::new(HashMap::new()),
            next: Cell::new(1),
            cancel,
        }
    }

    pub(crate) fn open(
        &self,
        identity: &crate::targets::ProcessIdentity,
        runtime: &Path,
        fresh: bool,
        timeout_ms: u32,
    ) -> Result<u32> {
        ensure!(
            (1..=45_000).contains(&timeout_ms),
            "invalid control open deadline"
        );
        ensure!(
            !self.cancel.load(Ordering::Acquire),
            "control open cancelled"
        );
        identity.ensure_alive()?;
        crate::process_arch::ensure_host_target(identity.bits)?;
        ensure!(
            identity.pid != std::process::id(),
            "cannot attach Rust control to itself"
        );
        ensure!(
            !self.channels.borrow().values().any(|channel| {
                channel.identity.pid == identity.pid
                    && channel.identity.creation == identity.creation
            }),
            "target already has a Rust control connection"
        );
        let instance = self.next.get();
        let next = instance
            .checked_add(1)
            .context("Rust control instance IDs exhausted")?;
        let deadline = Instant::now() + Duration::from_millis(timeout_ms.into());
        let mut pipe = ControlPipe::create(identity)?;
        if fresh {
            injector::inject(identity, runtime, deadline, &self.cancel).with_context(|| {
                format!(
                    "inject selected PID {} from {}",
                    identity.pid,
                    runtime.display()
                )
            })?;
        }
        pipe.connect(deadline, &self.cancel).with_context(|| {
            format!(
                "connect resident lifecycle pipe for PID {} (matching native runtime required)",
                identity.pid
            )
        })?;
        // Readiness checks the complete resident build identity, not just a
        // filename or a host-side DLL's optional capability export.
        loop {
            identity.ensure_alive()?;
            ensure!(
                !self.cancel.load(Ordering::Acquire),
                "control readiness cancelled"
            );
            ensure!(
                Instant::now() < deadline,
                "compatible target readiness timed out"
            );
            if let Some(ready) =
                crate::lifecycle::readiness(identity.pid, identity.creation, identity.bits)
            {
                ensure!(
                    matches!(ready.state, 0 | 1 | 5),
                    "target is not available for lifecycle Begin"
                );
                if ready.state != 0 {
                    ensure!(
                        ready.reserved[0] & crate::lifecycle::SETUP_ACK_CAPABILITY != 0,
                        "resident target lacks checked MessageFilter setup capability"
                    );
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let identity = identity.try_clone()?;
        self.channels.borrow_mut().insert(
            instance,
            Channel {
                pipe,
                identity,
                setup: None,
            },
        );
        self.next.set(next);
        Ok(instance)
    }

    fn submit(&self, instance: u32, command: Command) -> std::result::Result<(), SendError> {
        self.submit_until(
            instance,
            command,
            Instant::now() + Duration::from_millis(1500),
            false,
        )
    }

    fn submit_until(
        &self,
        instance: u32,
        command: Command,
        deadline: Instant,
        issuing_setup: bool,
    ) -> std::result::Result<(), SendError> {
        let mut channels = self.channels.borrow_mut();
        let channel = channels
            .get_mut(&instance)
            .context("unknown Rust control attachment")
            .map_err(SendError::NotStarted)?;
        if let Some(setup) = channel.setup.as_ref()
            && !setup_submission_allowed(setup, &command, issuing_setup)
        {
            return Err(SendError::NotStarted(anyhow::anyhow!(
                "incomplete checked setup permits only minimum Stop and confirmed retirement"
            )));
        }
        let frame = command
            .encode_for(channel.identity.bits)
            .map_err(SendError::NotStarted)?;
        let selected = validate_window_selection(&channel.identity, &command)
            .map_err(SendError::NotStarted)?;
        let prime = selected.filter(|_| {
            channel
                .setup
                .as_ref()
                .is_some_and(crate::lifecycle::SetupChannel::confirmed)
        });
        let written = channel
            .pipe
            .send(&frame.header, &frame.body, deadline, &self.cancel);
        finish_window_submission(
            written,
            prime,
            |recipient| prime_selected_window(&channel.identity, recipient, deadline, &self.cancel),
            || channel.pipe.close(),
        )
    }

    pub(crate) fn prepare_setup(&self, instance: u32, generation: u64, session: u64) -> Result<()> {
        let mut channels = self.channels.borrow_mut();
        let channel = channels
            .get_mut(&instance)
            .context("unknown Rust control attachment")?;
        ensure!(
            channel.setup.is_none(),
            "checked setup channel already exists"
        );
        channel.setup = Some(crate::lifecycle::SetupChannel::create(
            &channel.identity,
            generation,
            session,
        )?);
        Ok(())
    }

    pub(crate) fn begin_checked(&self, instance: u32, command: Command) -> Result<bool> {
        let Command::Begin {
            creation,
            generation,
            session,
            commit,
            reply,
            event,
        } = command
        else {
            return Ok(false);
        };
        let command = {
            let mut channels = self.channels.borrow_mut();
            let Some(setup) = channels
                .get_mut(&instance)
                .and_then(|channel| channel.setup.as_mut())
            else {
                return Ok(false);
            };
            // Ownership is conservative even if writing panics or fails midway.
            setup.transferred(true);
            Command::BeginChecked {
                creation,
                generation,
                session,
                commit,
                reply,
                event,
                config_mapping: setup.remote_mapping,
                config_event: setup.remote_event,
            }
        };
        let result = self.transfer(instance, command);
        if matches!(result, Ok(false))
            && let Some(setup) = self
                .channels
                .borrow_mut()
                .get_mut(&instance)
                .and_then(|channel| channel.setup.as_mut())
        {
            setup.transferred(false);
        }
        result
    }

    pub(crate) fn install_message_filter(&self, instance: u32) -> Result<()> {
        let deadline = Instant::now() + Duration::from_millis(1500);
        let command = {
            let mut channels = self.channels.borrow_mut();
            channels
                .get_mut(&instance)
                .context("unknown Rust control attachment")?
                .setup
                .as_mut()
                .context("checked setup channel was not provisioned")?
                .issue()?
        };
        let submitted = self.submit_until(instance, command, deadline, true);
        let mut channels = self.channels.borrow_mut();
        let setup = channels
            .get_mut(&instance)
            .context("checked setup channel disappeared")?
            .setup
            .as_mut()
            .context("checked setup channel disappeared")?;
        finish_setup_submission(setup, submitted, deadline, &self.cancel)
    }

    /// Ordinary commands expose the transport error. They never replay a frame.
    pub(crate) fn send(&self, instance: u32, command: Command) -> Result<()> {
        self.submit(instance, command).map_err(anyhow::Error::new)
    }

    /// Adapt remote-handle commands to existing provider ownership rules.
    /// False is proof that no WriteFile began. An error after transfer started
    /// preserves target ownership and must never trigger remote reclamation.
    pub(crate) fn transfer(&self, instance: u32, command: Command) -> Result<bool> {
        match self.submit(instance, command) {
            Ok(()) => Ok(true),
            Err(SendError::NotStarted(_)) => Ok(false),
            Err(error @ SendError::TransferStarted(_)) => Err(anyhow::Error::new(error)),
        }
    }

    pub(crate) fn close(&self, instance: u32) -> Result<()> {
        let mut channel = self
            .channels
            .borrow_mut()
            .remove(&instance)
            .context("unknown Rust control attachment")?;
        channel.pipe.close()
    }
}

fn validate_window_selection(
    identity: &crate::targets::ProcessIdentity,
    command: &Command,
) -> Result<Option<Recipient>> {
    let Command::Window { hwnd } = command else {
        return Ok(None);
    };
    if *hwnd == 0 {
        return Ok(None);
    }
    ensure!(
        identity.bits == 64 || *hwnd <= u32::MAX.into(),
        "selected window exceeds target pointer width"
    );
    identity.ensure_alive()?;
    let recipient = Recipient::validate(
        usize::try_from(*hwnd).context("selected window exceeds host pointer width")?,
        identity.pid,
    )?;
    identity.ensure_alive()?;
    Ok(Some(recipient))
}

fn prime_selected_window(
    identity: &crate::targets::ProcessIdentity,
    recipient: Recipient,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<()> {
    prime_with_post(identity, recipient, deadline, cancel, || {
        // A GetMessage entered before detour installation must return once before
        // the first input behind this selection. FIFO WM_NULL is wake-up only: no
        // typed input, synchronous target call, pump acknowledgement or sleep.
        ensure!(
            unsafe { PostMessageW(recipient.hwnd as _, WM_NULL, 0, 0) } != 0,
            "post selected target pump wake-up for PID {} HWND 0x{:X}: {}",
            identity.pid,
            recipient.hwnd,
            std::io::Error::last_os_error()
        );
        Ok(())
    })
}

fn validate_priming_target(
    identity: &crate::targets::ProcessIdentity,
    recipient: Recipient,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<()> {
    identity.ensure_alive()?;
    ensure!(
        !cancel.load(Ordering::Acquire),
        "window selection priming cancelled after command submission"
    );
    ensure!(
        Instant::now() < deadline,
        "window selection priming deadline expired after command submission"
    );
    ensure!(
        Recipient::validate(recipient.hwnd, identity.pid)? == recipient,
        "selected window thread changed before priming"
    );
    Ok(())
}

fn prime_with_post(
    identity: &crate::targets::ProcessIdentity,
    recipient: Recipient,
    deadline: Instant,
    cancel: &AtomicBool,
    post: impl FnOnce() -> Result<()>,
) -> Result<()> {
    validate_priming_target(identity, recipient, deadline, cancel)?;
    post()?;
    // Queueing the wake is already an external side effect. A postflight fault
    // is still an uncertain completed selection, never a NotStarted cancellation.
    validate_priming_target(identity, recipient, deadline, cancel)
}

fn finish_window_submission(
    written: std::result::Result<(), SendError>,
    prime: Option<Recipient>,
    post: impl FnOnce(Recipient) -> Result<()>,
    retire: impl FnOnce() -> Result<()>,
) -> std::result::Result<(), SendError> {
    written?;
    if let Some(recipient) = prime
        && let Err(error) = post(recipient)
    {
        let error = match retire() {
            Ok(()) => error,
            Err(close) => error.context(format!(
                "also failed to retire window selection stream: {close:#}"
            )),
        };
        // The complete Window frame already transferred. Never expose this as
        // NotStarted, retry it, or let the target remain eligible for reuse.
        return Err(SendError::TransferStarted(error));
    }
    Ok(())
}

fn finish_setup_submission(
    setup: &mut crate::lifecycle::SetupChannel,
    submitted: std::result::Result<(), SendError>,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<()> {
    match submitted {
        Err(SendError::NotStarted(error)) => {
            setup.abandon_unsubmitted();
            Err(anyhow::Error::new(SendError::NotStarted(error)))
        }
        Err(error @ SendError::TransferStarted(_)) => Err(anyhow::Error::new(error)),
        Ok(()) => setup.wait(deadline, cancel),
    }
}

fn minimum_stop_command(command: &Command) -> bool {
    matches!(
        command,
        Command::Freeze { enabled: true }
            | Command::StopFocus
            | Command::Messages {
                wheel: false,
                buttons: false,
                motion: false,
                keyboard: false
            }
            | Command::Draw { enabled: false }
            | Command::Block { enabled: false, .. }
    )
}

fn setup_command_allowed(setup: &crate::lifecycle::SetupChannel, command: &Command) -> bool {
    setup.allows_normal_commands()
        || minimum_stop_command(command)
        || (!setup.failed()
            && matches!(
                command,
                Command::Retire { .. }
                    | Command::Filter { enabled: false, .. }
                    | Command::Hook { install: false, .. }
            ))
}

fn setup_submission_allowed(
    setup: &crate::lifecycle::SetupChannel,
    command: &Command,
    issuing_setup: bool,
) -> bool {
    (issuing_setup && setup.pending_command_matches(command))
        || setup_command_allowed(setup, command)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct OwnedWindow(windows_sys::Win32::Foundation::HWND);
    impl OwnedWindow {
        fn create() -> Self {
            use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, HWND_MESSAGE};
            let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
            let hwnd = unsafe {
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    class.as_ptr(),
                    0,
                    0,
                    0,
                    1,
                    1,
                    HWND_MESSAGE,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            };
            assert!(!hwnd.is_null(), "create owned message-only window");
            Self(hwnd)
        }
        fn destroy(&mut self) {
            if !self.0.is_null() {
                assert_ne!(
                    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow(self.0) },
                    0
                );
                self.0 = std::ptr::null_mut();
            }
        }
    }
    impl Drop for OwnedWindow {
        fn drop(&mut self) {
            self.destroy();
        }
    }

    #[test]
    fn confirmed_window_selection_posts_only_its_own_empty_wake() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MSG, PM_REMOVE, PeekMessageW};
        let window = OwnedWindow::create();
        let identity = crate::targets::ProcessIdentity::open(std::process::id()).unwrap();
        let recipient = validate_window_selection(
            &identity,
            &Command::Window {
                hwnd: window.0 as usize as u64,
            },
        )
        .unwrap()
        .unwrap();
        let mut message: MSG = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { PeekMessageW(&mut message, window.0, WM_NULL, WM_NULL, PM_REMOVE) },
            0
        );
        finish_window_submission(
            Ok(()),
            Some(recipient),
            |recipient| {
                prime_selected_window(
                    &identity,
                    recipient,
                    Instant::now() + Duration::from_secs(1),
                    &AtomicBool::new(false),
                )
            },
            || panic!("successful priming must not retire the stream"),
        )
        .unwrap();
        assert_ne!(
            unsafe { PeekMessageW(&mut message, window.0, WM_NULL, WM_NULL, PM_REMOVE) },
            0
        );
        assert_eq!(
            (
                message.hwnd,
                message.message,
                message.wParam,
                message.lParam
            ),
            (window.0, WM_NULL, 0, 0)
        );
        assert_eq!(
            unsafe { PeekMessageW(&mut message, window.0, WM_NULL, WM_NULL, PM_REMOVE) },
            0
        );
        assert!(
            validate_window_selection(&identity, &Command::Window { hwnd: 0 })
                .unwrap()
                .is_none()
        );
        finish_window_submission(
            Ok(()),
            None,
            |_| panic!("zero selection must not prime"),
            || panic!("zero selection must not retire"),
        )
        .unwrap();
    }

    #[test]
    fn lost_window_after_submission_is_uncertain_and_retires_once() {
        let mut window = OwnedWindow::create();
        let identity = crate::targets::ProcessIdentity::open(std::process::id()).unwrap();
        let recipient = validate_window_selection(
            &identity,
            &Command::Window {
                hwnd: window.0 as usize as u64,
            },
        )
        .unwrap()
        .unwrap();
        window.destroy();
        let retired = Cell::new(0);
        let error = finish_window_submission(
            Ok(()),
            Some(recipient),
            |recipient| {
                prime_selected_window(
                    &identity,
                    recipient,
                    Instant::now() + Duration::from_secs(1),
                    &AtomicBool::new(false),
                )
            },
            || {
                retired.set(retired.get() + 1);
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.transfer_started());
        assert!(!error.cancelled_before_transfer());
        assert_eq!(retired.get(), 1);
        assert!(
            validate_window_selection(
                &identity,
                &Command::Window {
                    hwnd: recipient.hwnd as u64
                }
            )
            .is_err()
        );
    }

    #[test]
    fn window_priming_preserves_cancellation_transfer_boundary() {
        let window = OwnedWindow::create();
        let identity = crate::targets::ProcessIdentity::open(std::process::id()).unwrap();
        let recipient = validate_window_selection(
            &identity,
            &Command::Window {
                hwnd: window.0 as usize as u64,
            },
        )
        .unwrap()
        .unwrap();
        let retired = Cell::new(0);
        let error = finish_window_submission(
            Err(SendError::NotStarted(anyhow::Error::new(
                super::super::control_pipe::Cancelled,
            ))),
            Some(recipient),
            |_| panic!("unsubmitted selection cannot post a wake"),
            || {
                retired.set(retired.get() + 1);
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.cancelled_before_transfer());
        assert_eq!(retired.get(), 0);
        let error = finish_window_submission(
            Ok(()),
            Some(recipient),
            |recipient| {
                prime_selected_window(
                    &identity,
                    recipient,
                    Instant::now() + Duration::from_secs(1),
                    &AtomicBool::new(true),
                )
            },
            || {
                retired.set(retired.get() + 1);
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.transfer_started());
        assert!(!error.cancelled_before_transfer());
        assert_eq!(retired.get(), 1);
    }

    #[test]
    fn cancellation_after_queued_wake_is_postflight_uncertainty() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MSG, PM_REMOVE, PeekMessageW};
        let window = OwnedWindow::create();
        let identity = crate::targets::ProcessIdentity::open(std::process::id()).unwrap();
        let recipient = validate_window_selection(
            &identity,
            &Command::Window {
                hwnd: window.0 as usize as u64,
            },
        )
        .unwrap()
        .unwrap();
        let cancel = AtomicBool::new(false);
        let retired = Cell::new(0);
        let error = finish_window_submission(
            Ok(()),
            Some(recipient),
            |recipient| {
                prime_with_post(
                    &identity,
                    recipient,
                    Instant::now() + Duration::from_secs(1),
                    &cancel,
                    || {
                        assert_ne!(unsafe { PostMessageW(window.0, WM_NULL, 0, 0) }, 0);
                        cancel.store(true, Ordering::Release);
                        Ok(())
                    },
                )
            },
            || {
                retired.set(retired.get() + 1);
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.transfer_started());
        assert!(!error.cancelled_before_transfer());
        assert_eq!(retired.get(), 1);
        let mut message: MSG = unsafe { std::mem::zeroed() };
        assert_ne!(
            unsafe { PeekMessageW(&mut message, window.0, WM_NULL, WM_NULL, PM_REMOVE) },
            0
        );
        assert_eq!(
            (message.message, message.wParam, message.lParam),
            (WM_NULL, 0, 0)
        );
    }

    #[test]
    fn window_selection_rejects_foreign_original_pid_before_submission() {
        use std::{
            os::windows::process::CommandExt,
            process::{Command as ProcessCommand, Stdio},
        };
        struct OwnedChild(std::process::Child);
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let window = OwnedWindow::create();
        let child = OwnedChild(
            ProcessCommand::new(
                std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
                    .join("System32/cmd.exe"),
            )
            .args(["/d", "/c", "set /p MUX_FOREIGN_WINDOW_EXIT="])
            .creation_flags(0x0800_0000)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
        );
        let identity = crate::targets::ProcessIdentity::open(child.0.id()).unwrap();
        let error = validate_window_selection(
            &identity,
            &Command::Window {
                hwnd: window.0 as usize as u64,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("PID changed"));
    }

    #[test]
    fn checked_submission_gate_admits_only_the_correlated_pending_request() {
        let identity =
            crate::targets::ProcessIdentity::open_for_attach(std::process::id()).unwrap();
        let mut setup = crate::lifecycle::SetupChannel::create(&identity, 8, 9).unwrap();
        let pending = setup.issue().unwrap();
        assert!(setup_submission_allowed(&setup, &pending, true));
        assert!(!setup_submission_allowed(&setup, &pending, false));
        let wrong = Command::InstallMessageFilter {
            creation: identity.creation,
            generation: 8,
            session: 9,
            request: 2,
        };
        assert!(!setup_submission_allowed(&setup, &wrong, true));
        assert!(!setup_command_allowed(
            &setup,
            &Command::Freeze { enabled: false }
        ));
        assert!(!setup_command_allowed(&setup, &Command::Window { hwnd: 1 }));
        assert!(setup_command_allowed(
            &setup,
            &Command::Freeze { enabled: true }
        ));
        assert!(setup_command_allowed(&setup, &Command::StopFocus));
        assert!(!setup_command_allowed(
            &setup,
            &Command::Retire {
                generation: 8,
                session: 9,
                reply: 1,
                event: 2
            }
        ));
        setup.abandon_unsubmitted();
        assert!(setup_command_allowed(
            &setup,
            &Command::Retire {
                generation: 8,
                session: 9,
                reply: 1,
                event: 2
            }
        ));
        assert!(setup_command_allowed(
            &setup,
            &Command::Hook {
                hook: 2,
                install: false
            }
        ));
        assert!(!setup_submission_allowed(&setup, &pending, true));
        assert!(!setup_command_allowed(
            &setup,
            &Command::Freeze { enabled: false }
        ));
    }

    #[test]
    fn unsubmitted_setup_cancellation_preserves_ordinary_stop_without_retry() {
        let identity =
            crate::targets::ProcessIdentity::open_for_attach(std::process::id()).unwrap();
        let mut setup = crate::lifecycle::SetupChannel::create(&identity, 8, 9).unwrap();
        setup.issue().unwrap();
        let error = finish_setup_submission(
            &mut setup,
            Err(SendError::NotStarted(anyhow::Error::new(
                super::super::control_pipe::Cancelled,
            ))),
            Instant::now() + Duration::from_secs(1),
            &AtomicBool::new(true),
        )
        .unwrap_err();
        assert!(
            error
                .downcast_ref::<SendError>()
                .unwrap()
                .cancelled_before_transfer()
        );
        assert!(
            !setup.failed(),
            "proven unsubmitted setup must permit normal lifecycle cleanup"
        );
        assert!(
            setup.issue().is_err(),
            "consumed request cannot be replayed or skipped"
        );
    }

    #[test]
    fn uncertain_setup_submission_preserves_the_retirement_failure_latch() {
        let identity =
            crate::targets::ProcessIdentity::open_for_attach(std::process::id()).unwrap();
        let mut setup = crate::lifecycle::SetupChannel::create(&identity, 8, 9).unwrap();
        setup.issue().unwrap();
        let error = finish_setup_submission(
            &mut setup,
            Err(SendError::TransferStarted(anyhow::anyhow!(
                "partial native setup frame"
            ))),
            Instant::now() + Duration::from_secs(1),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(
            error
                .downcast_ref::<SendError>()
                .unwrap()
                .transfer_started()
        );
        assert!(setup.failed());
        assert!(setup.issue().is_err());
    }

    #[test]
    fn nonexistent_attachments_never_transfer_handle_ownership() {
        let registry = Registry::new(Arc::new(AtomicBool::new(false)));
        assert!(
            !registry
                .transfer(1, Command::CursorStop { attachment: 1 })
                .unwrap()
        );
        assert!(registry.send(1, Command::Freeze { enabled: true }).is_err());
        assert!(registry.close(1).is_err());
    }

    #[test]
    fn invalid_or_cancelled_open_cannot_launch_an_injector() {
        let identity = crate::targets::ProcessIdentity::open(std::process::id()).unwrap();
        let registry = Registry::new(Arc::new(AtomicBool::new(true)));
        let error = registry
            .open(&identity, Path::new("C:\\not-a-runtime"), true, 5000)
            .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        assert!(registry.channels.borrow().is_empty());
        let registry = Registry::new(Arc::new(AtomicBool::new(false)));
        assert!(
            registry
                .open(&identity, Path::new("C:\\not-a-runtime"), true, 0)
                .is_err()
        );
        let error = registry
            .open(&identity, Path::new("C:\\not-a-runtime"), true, 5000)
            .unwrap_err();
        assert!(error.to_string().contains("itself"));
    }
}
