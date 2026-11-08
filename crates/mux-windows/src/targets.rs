//! Visible top-level application windows for explicit target selection.
use anyhow::{Result, ensure};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM},
    UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
    },
};

#[derive(Clone, Debug)]
pub struct Target {
    pub hwnd: usize,
    pub pid: u32,
    pub title: String,
}

/// Read-only eligibility for a new virtual session. It never waits on a mutex,
/// so inspecting an abandoned controller cannot consume abandonment evidence.
#[derive(Clone, Debug, serde::Serialize)]
pub struct TargetEligibility {
    pub pid: u32,
    pub creation: Option<u64>,
    /// Lossless process creation identity for clients using JavaScript numbers.
    pub creation_token: Option<String>,
    pub state: EligibilityState,
    pub reason: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EligibilityState {
    Fresh,
    Reusable,
    Busy,
    Dirty,
    Legacy,
    Unavailable,
}
impl TargetEligibility {
    pub fn can_start(&self, virtual_only: bool) -> bool {
        self.state == EligibilityState::Fresh
            || (virtual_only && self.state == EligibilityState::Reusable)
    }
}
pub fn eligibility(pid: u32) -> TargetEligibility {
    let Ok(identity) = ProcessIdentity::open(pid) else {
        return TargetEligibility {
            pid,
            creation: None,
            creation_token: None,
            state: EligibilityState::Unavailable,
            reason: format!("PID {pid} is unavailable"),
        };
    };
    eligibility_for_identity(&identity)
}
fn eligibility_for_identity(identity: &ProcessIdentity) -> TargetEligibility {
    let pid = identity.pid;
    if let Err(error) = crate::process_arch::ensure_host_target(identity.bits) {
        return TargetEligibility {
            pid,
            creation: Some(identity.creation),
            creation_token: Some(identity.creation.to_string()),
            state: EligibilityState::Unavailable,
            reason: format!("PID {pid}: {error}"),
        };
    }
    let lease = crate::lifecycle::lease_state(pid, identity.creation);
    let ready = crate::lifecycle::readiness(pid, identity.creation, identity.bits);
    let state = classify(lease, ready, || hooks_loaded(pid));
    let reason = format!(
        "PID {pid}: {}",
        match state {
            EligibilityState::Fresh => "ready for first attachment",
            EligibilityState::Reusable => "ready for another virtual session",
            EligibilityState::Busy => "another controller owns this application",
            EligibilityState::Dirty =>
                "cleanup is incomplete or uncommitted; restart the application",
            EligibilityState::Legacy => "legacy hooks remain loaded; restart the application",
            EligibilityState::Unavailable => "application identity or hooks could not be inspected",
        }
    );
    TargetEligibility {
        pid,
        creation: Some(identity.creation),
        creation_token: Some(identity.creation.to_string()),
        state,
        reason,
    }
}
fn classify(
    lease: Option<u32>,
    ready: Option<crate::lifecycle::Readiness>,
    hooks: impl FnOnce() -> Result<bool>,
) -> EligibilityState {
    match (lease, ready) {
        (Some(1), _) => EligibilityState::Dirty,
        (_, Some(record)) if record.state == 2 => EligibilityState::Busy,
        (Some(2), _) => EligibilityState::Busy,
        (_, Some(record)) if record.state == 5 => EligibilityState::Reusable,
        (_, Some(record)) if record.state == 1 => EligibilityState::Fresh,
        (_, Some(_)) => EligibilityState::Dirty,
        _ => match hooks() {
            Ok(false) => EligibilityState::Fresh,
            Ok(true) => EligibilityState::Legacy,
            Err(_) => EligibilityState::Unavailable,
        },
    }
}
pub fn require_start_targets(pids: &[u32], virtual_only: bool) -> Result<()> {
    for &pid in pids {
        let target = eligibility(pid);
        ensure!(target.can_start(virtual_only), "{}", target.reason);
    }
    Ok(())
}

#[cfg(test)]
mod eligibility_tests {
    use super::*;
    #[test]
    fn only_committed_prepared_targets_can_be_reused() {
        let ready = |state| {
            Some(crate::lifecycle::Readiness {
                state,
                ..Default::default()
            })
        };
        assert_eq!(
            classify(Some(3), ready(3), || Ok(true)),
            EligibilityState::Dirty
        );
        assert_eq!(
            classify(Some(3), ready(5), || Ok(true)),
            EligibilityState::Reusable
        );
        assert_eq!(
            classify(Some(1), ready(5), || Ok(true)),
            EligibilityState::Dirty
        );
        assert_eq!(
            classify(Some(2), ready(5), || Ok(true)),
            EligibilityState::Busy
        );
        assert_eq!(
            classify(None, ready(4), || Ok(true)),
            EligibilityState::Dirty
        );
    }
    #[test]
    fn old_and_unreadable_backends_fail_closed() {
        assert_eq!(classify(None, None, || Ok(false)), EligibilityState::Fresh);
        assert_eq!(classify(None, None, || Ok(true)), EligibilityState::Legacy);
        assert_eq!(
            classify(None, None, || anyhow::bail!("inspection failed")),
            EligibilityState::Unavailable
        );
        let reusable = TargetEligibility {
            pid: 1,
            creation: Some(2),
            creation_token: Some("2".into()),
            state: EligibilityState::Reusable,
            reason: String::new(),
        };
        assert!(reusable.can_start(true));
        assert!(!reusable.can_start(false));
    }
}

/// Parses only the canonical decimal representation of a creation FILETIME.
pub fn parse_creation_token(token: &str) -> Result<u64> {
    let creation = token.parse::<u64>()?;
    ensure!(token == creation.to_string(), "invalid creation token");
    Ok(creation)
}

/// A read-only selection retaining the original process object until real input.
/// Windows has no generation ticket for HWND reuse inside one live process.
pub struct ValidatedSelection {
    identity: ProcessIdentity,
    window: mux_core::routing::Window,
}
impl ValidatedSelection {
    pub fn new(pid: u32, hwnd: usize, creation_token: &str) -> Result<Self> {
        let creation = parse_creation_token(creation_token)?;
        let identity = ProcessIdentity::open(pid)?;
        ensure!(
            identity.creation == creation,
            "application creation changed"
        );
        let selected = Self {
            identity,
            window: mux_core::routing::Window { pid, hwnd },
        };
        selected.revalidate()?;
        Ok(selected)
    }

    /// Rechecks eligibility and client geometry without attaching or sending input.
    pub fn revalidate(&self) -> Result<Surface> {
        use windows_sys::Win32::{
            Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
            UI::{
                Input::KeyboardAndMouse::IsWindowEnabled,
                WindowsAndMessaging::{GA_ROOT, GetAncestor, IsWindow},
            },
        };
        self.identity.ensure_alive()?;
        ensure!(
            windows()?
                .iter()
                .any(|target| { target.pid == self.window.pid && target.hwnd == self.window.hwnd }),
            "selected window is no longer listed"
        );
        let hwnd = self.window.hwnd as HWND;
        ensure!(
            unsafe {
                IsWindow(hwnd) != 0
                    && GetAncestor(hwnd, GA_ROOT) == hwnd
                    && IsWindowEnabled(hwnd) != 0
            } && eligible_process(self.window.pid),
            "selected window is not an enabled application root"
        );
        let mut cloaked = 0u32;
        let result = unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED as u32,
                &mut cloaked as *mut _ as *mut _,
                std::mem::size_of_val(&cloaked) as u32,
            )
        };
        ensure!(
            result >= 0 && cloaked == 0,
            "selected window is hidden or cannot be inspected"
        );
        let found = surface(self.window)?;
        // The retained original handle must still be alive after all HWND/PID reads.
        self.identity.ensure_alive()?;
        Ok(found)
    }

    /// Inspects readiness using this retained identity, without acquiring a lease.
    /// An owned attachment can be checked by the caller instead of rejecting its Busy lease.
    pub fn eligibility(&self) -> Result<TargetEligibility> {
        self.identity.ensure_alive()?;
        let ready = eligibility_for_identity(&self.identity);
        self.identity.ensure_alive()?;
        Ok(ready)
    }

    pub fn window(&self) -> mux_core::routing::Window {
        self.window
    }

    pub fn creation(&self) -> u64 {
        self.identity.creation
    }

    pub fn creation_token(&self) -> String {
        self.identity.creation.to_string()
    }
}

#[cfg(test)]
mod selection_tests {
    use super::{EligibilityState, Target, TargetEligibility, parse_creation_token, sort_targets};

    #[test]
    fn creation_token_is_lossless_and_canonical() {
        for value in [0, 1, (1u64 << 53) + 1, u64::MAX] {
            assert_eq!(parse_creation_token(&value.to_string()).unwrap(), value);
        }
        for token in [
            "",
            "00",
            "01",
            "+1",
            "-1",
            " 1",
            "1 ",
            "1\n",
            "1.0",
            "18446744073709551616",
        ] {
            assert!(parse_creation_token(token).is_err(), "{token:?}");
        }
    }

    #[test]
    fn eligibility_keeps_numeric_creation_and_lossless_string_token() {
        let creation = (1u64 << 53) + 1;
        let eligibility = TargetEligibility {
            pid: 1,
            creation: Some(creation),
            creation_token: Some(creation.to_string()),
            state: EligibilityState::Fresh,
            reason: String::new(),
        };
        let value = serde_json::to_value(eligibility).unwrap();
        assert_eq!(value["creation"].as_u64(), Some(creation));
        assert_eq!(value["creation_token"].as_str(), Some("9007199254740993"));
    }

    #[test]
    fn same_title_roots_remain_distinct_inventory_targets() {
        let mut targets = vec![
            Target {
                hwnd: 2,
                pid: 1,
                title: "Document".into(),
            },
            Target {
                hwnd: 1,
                pid: 1,
                title: "Document".into(),
            },
        ];
        sort_targets(&mut targets);
        assert_eq!(
            targets.iter().map(|target| target.hwnd).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }
}

/// Original process object and creation FILETIME, retained across native operations.
pub(crate) struct ProcessIdentity {
    pub pid: u32,
    pub creation: u64,
    pub bits: u32,
    handle: windows_sys::Win32::Foundation::HANDLE,
}
impl ProcessIdentity {
    pub fn open(pid: u32) -> Result<Self> {
        Self::open_with_access(pid, false)
    }
    pub fn open_for_attach(pid: u32) -> Result<Self> {
        Self::open_with_access(pid, true)
    }
    fn open_with_access(pid: u32, attach: bool) -> Result<Self> {
        use windows_sys::Win32::{
            Foundation::FILETIME,
            System::Threading::{
                GetProcessTimes, OpenProcess, PROCESS_DUP_HANDLE,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            },
        };
        let handle = unsafe {
            OpenProcess(
                (if attach { PROCESS_DUP_HANDLE } else { 0 })
                    | PROCESS_QUERY_LIMITED_INFORMATION
                    | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        ensure!(
            !handle.is_null(),
            "cannot open original PID {pid}: {}",
            std::io::Error::last_os_error()
        );
        let mut identity = Self {
            pid,
            creation: 0,
            bits: 0,
            handle,
        };
        let mut times = [FILETIME::default(); 4];
        unsafe {
            ensure!(
                GetProcessTimes(
                    handle,
                    &mut times[0],
                    &mut times[1],
                    &mut times[2],
                    &mut times[3]
                ) != 0,
                "read original process identity"
            );
        }
        identity.creation =
            (u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime);
        identity.bits = crate::process_arch::process_bits(handle)?;
        identity.ensure_alive()?;
        Ok(identity)
    }
    pub fn handle(&self) -> windows_sys::Win32::Foundation::HANDLE {
        self.handle
    }
    /// Retains the same process object, without reopening its numeric PID.
    pub fn try_clone(&self) -> Result<Self> {
        use windows_sys::Win32::{
            Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle},
            System::Threading::GetCurrentProcess,
        };
        self.ensure_alive()?;
        let mut handle = std::ptr::null_mut();
        ensure!(
            unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    self.handle,
                    GetCurrentProcess(),
                    &mut handle,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                )
            } != 0,
            "retain original PID {}: {}",
            self.pid,
            std::io::Error::last_os_error()
        );
        let identity = Self {
            pid: self.pid,
            creation: self.creation,
            bits: self.bits,
            handle,
        };
        identity.ensure_alive()?;
        Ok(identity)
    }
    pub fn ensure_alive(&self) -> Result<()> {
        ensure!(
            unsafe { windows_sys::Win32::System::Threading::WaitForSingleObject(self.handle, 0) }
                == windows_sys::Win32::Foundation::WAIT_TIMEOUT,
            "original PID {} exited",
            self.pid
        );
        Ok(())
    }
}
impl Drop for ProcessIdentity {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

/// Reject targets retaining an earlier hooks DLL; upstream cannot safely reattach them.
pub fn require_fresh_target(pid: u32) -> Result<()> {
    ensure!(
        !hooks_loaded(pid)?,
        "PID {pid} still has ProtoInput hooks loaded. Restart that application before another session"
    );
    Ok(())
}
pub(crate) fn hooks_loaded(pid: u32) -> Result<bool> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_BAD_LENGTH, ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE},
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW,
            TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32,
        },
    };
    unsafe {
        let mut snapshot = INVALID_HANDLE_VALUE;
        for _ in 0..3 {
            snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid);
            if snapshot != INVALID_HANDLE_VALUE
                || std::io::Error::last_os_error().raw_os_error() != Some(ERROR_BAD_LENGTH as i32)
            {
                break;
            }
        }
        ensure!(
            snapshot != INVALID_HANDLE_VALUE,
            "inspect modules of PID {pid}: {}",
            std::io::Error::last_os_error()
        );
        let mut entry: MODULEENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of_val(&entry) as u32;
        let mut more = Module32FirstW(snapshot, &mut entry);
        let mut injected = false;
        while more != 0 {
            let end = entry
                .szModule
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(entry.szModule.len());
            let name = String::from_utf16_lossy(&entry.szModule[..end]);
            injected |= name.eq_ignore_ascii_case("ProtoInputHooks64.dll")
                || name.eq_ignore_ascii_case("ProtoInputHooks32.dll");
            more = Module32NextW(snapshot, &mut entry);
        }
        let error = std::io::Error::last_os_error();
        CloseHandle(snapshot);
        ensure!(
            error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32),
            "inspect modules of PID {pid}: {error}"
        );
        Ok(injected)
    }
}

/// Holds process handles so PID reuse cannot hide a target exit.
pub struct ProcessWatch(Vec<(u32, windows_sys::Win32::Foundation::HANDLE)>);
impl ProcessWatch {
    pub fn open(pids: impl IntoIterator<Item = u32>) -> Result<Self> {
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
        let mut watch = Self(Vec::new());
        for pid in pids {
            let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
            ensure!(
                !handle.is_null(),
                "cannot watch PID {pid}: {}",
                std::io::Error::last_os_error()
            );
            watch.0.push((pid, handle));
        }
        Ok(watch)
    }
    /// Recovery-only display metadata from the same retained process object.
    /// Query permission is optional; failure must never weaken the exit watch.
    pub fn open_with_application_name(pid: u32) -> Result<(Self, Option<String>)> {
        use windows_sys::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW};
        open_recovery_watch_with(
            |access| {
                let handle = unsafe { OpenProcess(access, 0, pid) };
                ensure!(
                    !handle.is_null(),
                    "cannot watch PID {pid}: {}",
                    std::io::Error::last_os_error()
                );
                // Own the handle before any optional query or allocation for it.
                Ok(Self(vec![(pid, handle)]))
            },
            |watch| {
                let handle = watch.handle(pid)?;
                let mut image = vec![0u16; 32768];
                let mut length = image.len() as u32;
                if unsafe { QueryFullProcessImageNameW(handle, 0, image.as_mut_ptr(), &mut length) }
                    == 0
                    || length as usize > image.len()
                {
                    return None;
                }
                application_basename(&String::from_utf16_lossy(&image[..length as usize]))
            },
        )
    }
    pub fn exited(&self) -> Option<u32> {
        use windows_sys::Win32::{
            Foundation::WAIT_OBJECT_0, System::Threading::WaitForSingleObject,
        };
        self.0
            .iter()
            .find(|(_, handle)| unsafe { WaitForSingleObject(*handle, 0) } == WAIT_OBJECT_0)
            .map(|(pid, _)| *pid)
    }

    /// Returns the borrowed process handle for a watched PID. The caller must
    /// keep this watcher alive for the full duration of any synchronous use.
    pub fn handle(&self, pid: u32) -> Option<windows_sys::Win32::Foundation::HANDLE> {
        self.0
            .iter()
            .find(|(watched_pid, _)| *watched_pid == pid)
            .map(|(_, handle)| *handle)
    }
}
impl Drop for ProcessWatch {
    fn drop(&mut self) {
        for (_, handle) in &self.0 {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(*handle);
            }
        }
    }
}

fn open_recovery_watch_with(
    mut open: impl FnMut(u32) -> Result<ProcessWatch>,
    query: impl FnOnce(&ProcessWatch) -> Option<String>,
) -> Result<(ProcessWatch, Option<String>)> {
    use windows_sys::Win32::System::Threading::{
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };
    if let Ok(watch) = open(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION) {
        let name = query(&watch);
        Ok((watch, name))
    } else {
        // A synchronize-only fallback retains the original safety contract and
        // intentionally has no label, even if permissions later become available.
        Ok((open(PROCESS_SYNCHRONIZE)?, None))
    }
}

fn application_basename(image: &str) -> Option<String> {
    let name: String = image
        .rsplit(['\\', '/'])
        .next()?
        .chars()
        .filter(|ch| {
            !ch.is_control() && !matches!(*ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .collect();
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
mod recovery_watch_tests {
    use super::*;
    use windows_sys::Win32::System::Threading::{
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };

    #[test]
    fn query_right_denial_falls_back_to_a_valid_synchronize_only_watch() {
        let pid = std::process::id();
        let mut access_attempts = Vec::new();
        let (watch, name) = open_recovery_watch_with(
            |access| {
                access_attempts.push(access);
                if access & PROCESS_QUERY_LIMITED_INFORMATION != 0 {
                    anyhow::bail!("simulated optional query-right denial");
                }
                ProcessWatch::open([pid])
            },
            |_| panic!("fallback must not query a synchronize-only handle"),
        )
        .unwrap();
        assert_eq!(
            access_attempts,
            [
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                PROCESS_SYNCHRONIZE
            ]
        );
        assert!(name.is_none());
        assert!(watch.handle(pid).is_some());
        assert!(watch.exited().is_none());
    }

    #[test]
    fn query_failure_preserves_the_same_owned_watch_without_reopening() {
        let pid = std::process::id();
        let mut opens = 0;
        let mut queried_handle = None;
        let (watch, name) = open_recovery_watch_with(
            |_| {
                opens += 1;
                ProcessWatch::open([pid])
            },
            |watch| {
                queried_handle = watch.handle(pid);
                None // Simulated QueryFullProcessImageNameW failure.
            },
        )
        .unwrap();
        assert_eq!(opens, 1);
        assert_eq!(watch.handle(pid), queried_handle);
        assert!(watch.exited().is_none());
        assert!(name.is_none());
    }

    #[test]
    fn labels_are_sanitized_basenames_and_empty_names_are_absent() {
        assert_eq!(
            application_basename("C:\\private\\folder\\work\n\t\u{202e}.exe"),
            Some("work.exe".into())
        );
        assert_eq!(
            application_basename("C:/private/demo.exe"),
            Some("demo.exe".into())
        );
        assert!(application_basename("C:\\private\\").is_none());
        assert!(application_basename("\n\t\u{2066}").is_none());
    }
}

/// Poll APIs intercepted by ProtoInput. Only the test keys W/A/S/D/Space are sampled.
pub fn probe_input() -> ((i32, i32), u8) {
    use windows_sys::Win32::{
        Foundation::POINT,
        UI::{Input::KeyboardAndMouse::GetAsyncKeyState, WindowsAndMessaging::GetCursorPos},
    };
    unsafe {
        let mut cursor = POINT { x: 0, y: 0 };
        GetCursorPos(&mut cursor);
        let mut keys = 0;
        for (index, key) in [0x57, 0x41, 0x53, 0x44, 0x20].iter().enumerate() {
            if GetAsyncKeyState(*key) < 0 {
                keys |= 1 << index;
            }
        }
        ((cursor.x, cursor.y), keys)
    }
}

pub fn windows() -> Result<Vec<Target>> {
    unsafe extern "system" fn collect(window: HWND, data: LPARAM) -> i32 {
        unsafe {
            if IsWindowVisible(window) == 0
                || !windows_sys::Win32::UI::WindowsAndMessaging::GetWindow(
                    window,
                    windows_sys::Win32::UI::WindowsAndMessaging::GW_OWNER,
                )
                .is_null()
                || windows_sys::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
                    window,
                    windows_sys::Win32::UI::WindowsAndMessaging::GWL_EXSTYLE,
                ) as u32
                    & windows_sys::Win32::UI::WindowsAndMessaging::WS_EX_TOOLWINDOW
                    != 0
            {
                return 1;
            }
            // The injected backend creates this private GUI with WS_VISIBLE
            // before hiding it. It must never become an assigned input window
            // if enumeration happens during that brief creation interval.
            let mut class = [0u16; 256];
            let class_len = GetClassNameW(window, class.as_mut_ptr(), class.len() as i32);
            if class_len > 0
                && class[..class_len as usize]
                    .iter()
                    .copied()
                    .eq("PROTOINPUTUI".encode_utf16())
            {
                return 1;
            }
            let mut text = [0u16; 1024];
            let len = GetWindowTextW(window, text.as_mut_ptr(), text.len() as i32);
            let mut pid = 0;
            GetWindowThreadProcessId(window, &mut pid);
            if len > 0 && pid != 0 && pid != std::process::id() {
                let targets = &mut *(data as *mut Vec<Target>);
                targets.push(Target {
                    hwnd: window as usize,
                    pid,
                    title: String::from_utf16_lossy(&text[..len as usize]),
                });
            }
            1
        }
    }
    let mut targets = Vec::<Target>::new();
    ensure!(
        unsafe { EnumWindows(Some(collect), &mut targets as *mut _ as LPARAM) } != 0,
        "enumerate windows: {}",
        std::io::Error::last_os_error()
    );
    sort_targets(&mut targets);
    Ok(targets)
}

fn sort_targets(targets: &mut Vec<Target>) {
    targets.sort_by(|a, b| {
        a.title
            .cmp(&b.title)
            .then(a.pid.cmp(&b.pid))
            .then(a.hwnd.cmp(&b.hwnd))
    });
    targets.dedup_by_key(|target| target.hwnd);
}

/// Physical client coordinates; called from the DPI-aware routing worker.
pub fn client_bounds(hwnd: usize, expected_pid: u32) -> Result<mux_core::desktop::Bounds> {
    use windows_sys::Win32::{
        Foundation::{POINT, RECT},
        Graphics::Gdi::ClientToScreen,
        UI::WindowsAndMessaging::{GetClientRect, IsIconic},
    };
    unsafe {
        let window = hwnd as HWND;
        let mut pid = 0;
        GetWindowThreadProcessId(window, &mut pid);
        ensure!(
            pid == expected_pid
                && IsWindowVisible(window) != 0
                && IsIconic(windows_sys::Win32::UI::WindowsAndMessaging::GetAncestor(
                    window,
                    windows_sys::Win32::UI::WindowsAndMessaging::GA_ROOT
                )) == 0,
            "target window closed, hidden or minimized"
        );
        let mut rect: RECT = std::mem::zeroed();
        let mut origin = POINT { x: 0, y: 0 };
        ensure!(
            GetClientRect(window, &mut rect) != 0 && ClientToScreen(window, &mut origin) != 0,
            "read target client bounds"
        );
        let bounds = mux_core::desktop::Bounds {
            left: origin.x,
            top: origin.y,
            right: origin.x + rect.right,
            bottom: origin.y + rect.bottom,
        };
        ensure!(
            bounds.valid() && rect.right < 30000 && rect.bottom < 30000,
            "target has invalid client bounds"
        );
        Ok(bounds)
    }
}
pub fn desktop_bounds() -> mux_core::desktop::Bounds {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    unsafe {
        let left = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let top = GetSystemMetrics(SM_YVIRTUALSCREEN);
        mux_core::desktop::Bounds {
            left,
            top,
            right: left + GetSystemMetrics(SM_CXVIRTUALSCREEN),
            bottom: top + GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    }
}

/// Sample only mouse button state inside the disposable routing probe.
pub fn probe_buttons() -> u8 {
    let mut result = 0;
    for (i, key) in [1, 2, 4, 5, 6].into_iter().enumerate() {
        if unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(key) } < 0 {
            result |= 1 << i;
        }
    }
    result
}

/// Topmost window at a virtual cursor. Never reads or moves the shared pointer.
#[derive(Debug, Clone)]
pub struct Surface {
    pub window: mux_core::routing::Window,
    pub client: mux_core::desktop::Bounds,
    pub outer: mux_core::desktop::Bounds,
    pub title: String,
    pub region: i32,
}
pub fn surface(window: mux_core::routing::Window) -> Result<Surface> {
    use windows_sys::Win32::{Foundation::RECT, UI::WindowsAndMessaging::GetWindowRect};
    let client = client_bounds(window.hwnd, window.pid)?;
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    ensure!(
        unsafe { GetWindowRect(window.hwnd as HWND, &mut rect) } != 0,
        "read window bounds"
    );
    let root = unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::GetAncestor(
            window.hwnd as HWND,
            windows_sys::Win32::UI::WindowsAndMessaging::GA_ROOT,
        )
    };
    let mut name = [0u16; 512];
    let len = unsafe { GetWindowTextW(root, name.as_mut_ptr(), 512) };
    Ok(Surface {
        window,
        client,
        outer: mux_core::desktop::Bounds {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        },
        title: String::from_utf16_lossy(&name[..len.max(0) as usize]),
        region: 1,
    })
}
/// Finds an enabled, visible descendant beneath a virtual point within an explicitly assigned root.
pub fn child_at_point(
    window: mux_core::routing::Window,
    x: i32,
    y: i32,
) -> Result<mux_core::routing::Window> {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    let bounds = client_bounds(window.hwnd, window.pid)?;
    if !bounds.contains(x, y) {
        return Ok(window);
    }
    let mut child = window.hwnd as HWND;
    for _ in 0..16 {
        let mut point = windows_sys::Win32::Foundation::POINT { x, y };
        if unsafe { windows_sys::Win32::Graphics::Gdi::ScreenToClient(child, &mut point) } == 0 {
            break;
        }
        let next = unsafe {
            ChildWindowFromPointEx(
                child,
                point,
                CWP_SKIPINVISIBLE | CWP_SKIPDISABLED | CWP_SKIPTRANSPARENT,
            )
        };
        if next.is_null() || next == child {
            break;
        }
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(next, &mut pid);
        }
        if pid != window.pid {
            break;
        }
        child = next;
    }
    client_bounds(child as usize, window.pid)?;
    Ok(mux_core::routing::Window {
        hwnd: child as usize,
        pid: window.pid,
    })
}

pub fn at_point(
    x: i32,
    y: i32,
    excluded_pids: &[u32],
    allowed_pids: &[u32],
) -> Result<Option<Surface>> {
    use windows_sys::Win32::{
        Foundation::RECT,
        Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
        UI::WindowsAndMessaging::*,
    };
    struct Search {
        x: i32,
        y: i32,
        excluded: Vec<u32>,
        allowed: Vec<u32>,
        found: Option<mux_core::routing::Window>,
    }
    unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> i32 {
        unsafe {
            let s = &mut *(data as *mut Search);
            if IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 {
                return 1;
            }
            let mut cloaked = 0u32;
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED as u32,
                &mut cloaked as *mut _ as *mut _,
                4,
            );
            if cloaked != 0 {
                return 1;
            }
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            // Layered click-through surfaces (including external cursor overlays) do not occlude input.
            if style & (WS_EX_TRANSPARENT | WS_EX_LAYERED) == (WS_EX_TRANSPARENT | WS_EX_LAYERED) {
                return 1;
            }
            let mut r: RECT = std::mem::zeroed();
            if GetWindowRect(hwnd, &mut r) == 0 {
                return 1;
            }
            if s.x < r.left || s.x >= r.right || s.y < r.top || s.y >= r.bottom {
                return 1;
            }
            if windows_sys::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(hwnd) == 0
                || s.excluded.contains(&pid)
                || (!s.allowed.is_empty() && !s.allowed.contains(&pid))
            {
                return 0;
            }
            s.found = Some(mux_core::routing::Window {
                hwnd: hwnd as usize,
                pid,
            });
            0
        }
    }
    let mut search = Search {
        x,
        y,
        excluded: excluded_pids.to_vec(),
        allowed: allowed_pids.to_vec(),
        found: None,
    };
    unsafe {
        EnumWindows(Some(collect), &mut search as *mut _ as LPARAM);
    }
    let Some(window) = search.found else {
        return Ok(None);
    };
    if !eligible_process(window.pid) {
        return Ok(None);
    }
    let Ok(mut found) = surface(window) else {
        return Ok(None);
    };
    let mut region = 0usize;
    let point = ((y as u16 as u32) << 16) | (x as u16 as u32);
    let ok = unsafe {
        SendMessageTimeoutW(
            window.hwnd as HWND,
            WM_NCHITTEST,
            0,
            point as isize,
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            500,
            &mut region,
        )
    };
    ensure!(
        ok != 0,
        "Window hit test did not respond for PID {}: {}",
        window.pid,
        std::io::Error::last_os_error()
    );
    found.region = region as i32;
    if found.region == HTCLIENT as i32 {
        let mut child = window.hwnd as HWND;
        for _ in 0..16 {
            let mut point = windows_sys::Win32::Foundation::POINT { x, y };
            if unsafe { windows_sys::Win32::Graphics::Gdi::ScreenToClient(child, &mut point) } == 0
            {
                break;
            }
            let next = unsafe {
                ChildWindowFromPointEx(
                    child,
                    point,
                    CWP_SKIPINVISIBLE | CWP_SKIPDISABLED | CWP_SKIPTRANSPARENT,
                )
            };
            if next.is_null() || next == child {
                break;
            }
            let mut pid = 0;
            unsafe {
                GetWindowThreadProcessId(next, &mut pid);
            }
            if pid != window.pid {
                break;
            }
            child = next;
        }
        if child as usize != window.hwnd {
            found.window.hwnd = child as usize;
            found.client = client_bounds(child as usize, window.pid)?;
        }
    }
    Ok(Some(found))
}
fn eligible_process(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut name = [0u16; 32768];
        let mut len = name.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, name.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        if ok == 0 {
            return false;
        }
        let full = String::from_utf16_lossy(&name[..len as usize]).to_lowercase();
        let name = full.rsplit('\\').next().unwrap_or("");
        !matches!(
            name,
            "explorer.exe"
                | "dwm.exe"
                | "csrss.exe"
                | "winlogon.exe"
                | "logonui.exe"
                | "sihost.exe"
                | "shellexperiencehost.exe"
                | "startmenuexperiencehost.exe"
                | "searchhost.exe"
                | "applicationframehost.exe"
                | "rustdesk.exe"
                | "codex.exe"
                | "windowsterminal.exe"
                | "openconsole.exe"
                | "powershell.exe"
                | "pwsh.exe"
                | "conhost.exe"
                | "taskmgr.exe"
        )
    }
}
/// Move/resize a virtually captured window without activating it or moving the OS cursor.
pub fn place(window: mux_core::routing::Window, bounds: mux_core::desktop::Bounds) -> Result<()> {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    client_bounds(window.hwnd, window.pid)?;
    ensure!(bounds.valid(), "invalid window rectangle");
    ensure!(
        unsafe {
            SetWindowPos(
                window.hwnd as HWND,
                std::ptr::null_mut(),
                bounds.left,
                bounds.top,
                bounds.right - bounds.left,
                bounds.bottom - bounds.top,
                SWP_NOACTIVATE | SWP_NOZORDER,
            )
        } != 0,
        "move virtual target window"
    );
    Ok(())
}
