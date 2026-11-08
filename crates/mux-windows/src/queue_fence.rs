//! Bounded acknowledgement for queued virtual input cleanup.
//!
//! A fence receipt means ProtoInput processed the fence after earlier input and
//! the target's message hook removed the marker. It does not prove arbitrary
//! application code accepted or acted on those messages.

use anyhow::{Context, Result, bail, ensure};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE,
    },
    System::Threading::{
        CreateEventW, GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForMultipleObjects,
    },
    UI::WindowsAndMessaging::{GetWindowThreadProcessId, IsWindow},
};

pub(crate) type QueueFence = unsafe extern "C" fn(u32, u64, u64, u64) -> bool;

const MAX_RECIPIENT_THREADS: usize = 4096;
const PROCESS_ACCESS: u32 =
    PROCESS_DUP_HANDLE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
const EVENT_MODIFY_STATE: u32 = 0x0002;
const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 258;
const FENCE_TIMEOUT: Duration = Duration::from_secs(1);

static NEXT_COOKIE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Recipient {
    pub pid: u32,
    pub thread_id: u32,
    pub hwnd: usize,
}

impl Recipient {
    pub fn validate(hwnd: usize, expected_pid: u32) -> Result<Self> {
        ensure!(hwnd != 0, "cannot fence a null target window");
        let mut pid = 0;
        let thread_id = unsafe {
            ensure!(
                IsWindow(hwnd as _) != 0,
                "target window 0x{hwnd:X} no longer exists"
            );
            GetWindowThreadProcessId(hwnd as _, &mut pid)
        };
        ensure!(thread_id != 0, "cannot resolve target window thread");
        ensure!(pid == expected_pid, "target window PID changed");
        Ok(Self {
            pid,
            thread_id,
            hwnd,
        })
    }
}

pub(crate) fn remember(recipients: &mut Vec<Recipient>, recipient: Recipient) -> Result<()> {
    if let Some(existing) = recipients
        .iter_mut()
        .find(|current| current.pid == recipient.pid && current.thread_id == recipient.thread_id)
    {
        existing.hwnd = recipient.hwnd;
        return Ok(());
    }
    ensure!(
        recipients.len() < MAX_RECIPIENT_THREADS,
        "too many target GUI threads to track for deterministic cleanup"
    );
    recipients.push(recipient);
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct FenceRequest {
    pub instance: u32,
    pub recipient: Recipient,
    /// Borrowed identity handle owned by ProcessWatch for this PID. It must
    /// outlive `drain`; it prevents a recycled PID from receiving the fence.
    pub expected_process: HANDLE,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShutdownReport {
    pub acknowledged: usize,
    pub targets_exited: usize,
}

struct PendingFence {
    request: FenceRequest,
    process: HANDLE,
    event: HANDLE,
}

impl Drop for PendingFence {
    fn drop(&mut self) {
        unsafe {
            if !self.event.is_null() {
                CloseHandle(self.event);
            }
            if !self.process.is_null() {
                CloseHandle(self.process);
            }
        }
    }
}

/// Posts every marker first, then waits against one shared deadline. The duplicated
/// event handle belongs to the target once the loader accepts the fence. It is never
/// closed remotely from this process: after a timeout, a late target callback may still
/// use it, and cross-closing it could race with HANDLE reuse in that process.
pub(crate) fn drain(
    requests: &[FenceRequest],
    mut queue_fence: impl FnMut(u32, u64, u64, u64) -> Result<bool>,
) -> Result<ShutdownReport> {
    let deadline = Instant::now() + FENCE_TIMEOUT;
    let mut pending = Vec::with_capacity(requests.len());
    let mut report = ShutdownReport::default();

    for &request in requests {
        let recipient = request.recipient;
        ensure!(
            Instant::now() < deadline,
            "queue-fence deadline expired before all recipients were posted"
        );
        let process = unsafe { OpenProcess(PROCESS_ACCESS, 0, recipient.pid) };
        if process.is_null() {
            if original_exited(request.expected_process)? {
                report.targets_exited += 1;
                continue;
            }
            bail!(
                "cannot open PID {} for queue fence: {}",
                recipient.pid,
                std::io::Error::last_os_error()
            );
        }
        let mut fence = PendingFence {
            request,
            process,
            event: std::ptr::null_mut(),
        };

        if original_exited(request.expected_process)? {
            report.targets_exited += 1;
            continue;
        }
        let actual = Recipient::validate(recipient.hwnd, recipient.pid).with_context(|| {
            format!(
                "validate PID {} HWND 0x{:X} before fence",
                recipient.pid, recipient.hwnd
            )
        })?;
        ensure!(
            actual.thread_id == recipient.thread_id,
            "target HWND 0x{:X} moved to another GUI thread",
            recipient.hwnd
        );

        fence.event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        ensure!(
            !fence.event.is_null(),
            "create queue-fence event: {}",
            std::io::Error::last_os_error()
        );
        let mut remote_event: HANDLE = std::ptr::null_mut();
        let duplicated = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                fence.event,
                process,
                &mut remote_event,
                EVENT_MODIFY_STATE,
                0,
                0,
            )
        };
        ensure!(
            duplicated != 0 && !remote_event.is_null(),
            "duplicate queue-fence event into PID {}: {}",
            recipient.pid,
            std::io::Error::last_os_error()
        );
        let cookie = NEXT_COOKIE.fetch_add(1, Ordering::Relaxed);
        // Err is an uncertain transfer, so the target retains its duplicated
        // event even when this local waiter goes away. Only false permits reclaim.
        let accepted = queue_fence(
            request.instance,
            recipient.hwnd as u64,
            remote_event as usize as u64,
            cookie,
        )
        .context("queue-fence command transport is uncertain")?;
        if !accepted {
            close_remote_source(process, remote_event)?;
            bail!(
                "ProtoInput did not accept queue fence for PID {} HWND 0x{:X}",
                recipient.pid,
                recipient.hwnd
            );
        }
        pending.push(fence);
    }

    for fence in &pending {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!(
                "queue-fence deadline expired for PID {} HWND 0x{:X}",
                fence.request.recipient.pid,
                fence.request.recipient.hwnd
            );
        }
        let handles = [fence.event, fence.process];
        let millis = remaining.as_millis().clamp(1, u128::from(u32::MAX)) as u32;
        let wait = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, millis) };
        match wait {
            WAIT_OBJECT_0 => report.acknowledged += 1,
            x if x == WAIT_OBJECT_0 + 1 => report.targets_exited += 1,
            WAIT_TIMEOUT => bail!(
                "queue-fence timed out for PID {} HWND 0x{:X}",
                fence.request.recipient.pid,
                fence.request.recipient.hwnd
            ),
            other => bail!(
                "queue-fence wait failed for PID {} HWND 0x{:X} (result {other}, last error {})",
                fence.request.recipient.pid,
                fence.request.recipient.hwnd,
                std::io::Error::last_os_error()
            ),
        }
    }
    Ok(report)
}

fn original_exited(process: HANDLE) -> Result<bool> {
    let state = unsafe { windows_sys::Win32::System::Threading::WaitForSingleObject(process, 0) };
    match state {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        other => bail!(
            "check watched target process before queue fence failed (result {other}, last error {})",
            std::io::Error::last_os_error()
        ),
    }
}

fn close_remote_source(process: HANDLE, remote: HANDLE) -> Result<()> {
    let mut local: HANDLE = std::ptr::null_mut();
    let closed = unsafe {
        DuplicateHandle(
            process,
            remote,
            GetCurrentProcess(),
            &mut local,
            0,
            0,
            DUPLICATE_CLOSE_SOURCE | DUPLICATE_SAME_ACCESS,
        )
    };
    ensure!(
        closed != 0,
        "close rejected remote queue-fence event: {}",
        std::io::Error::last_os_error()
    );
    if !local.is_null() {
        unsafe { CloseHandle(local) };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipient_tracking_retargets_a_thread_without_duplication() {
        let mut recipients = Vec::new();
        remember(
            &mut recipients,
            Recipient {
                pid: 7,
                thread_id: 12,
                hwnd: 100,
            },
        )
        .unwrap();
        remember(
            &mut recipients,
            Recipient {
                pid: 7,
                thread_id: 12,
                hwnd: 200,
            },
        )
        .unwrap();
        assert_eq!(
            recipients,
            vec![Recipient {
                pid: 7,
                thread_id: 12,
                hwnd: 200,
            }]
        );
    }

    #[test]
    fn recipient_tracking_rejects_a_4097th_thread_atomically() {
        let mut recipients = (0..MAX_RECIPIENT_THREADS)
            .map(|thread_id| Recipient {
                pid: 7,
                thread_id: thread_id as u32 + 1,
                hwnd: thread_id + 1,
            })
            .collect::<Vec<_>>();
        let original = recipients.len();
        assert!(
            remember(
                &mut recipients,
                Recipient {
                    pid: 7,
                    thread_id: MAX_RECIPIENT_THREADS as u32 + 1,
                    hwnd: MAX_RECIPIENT_THREADS + 1,
                }
            )
            .is_err()
        );
        assert_eq!(recipients.len(), original);
    }
}
