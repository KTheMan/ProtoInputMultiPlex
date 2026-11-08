//! Host-side command/reply owner for the optional target-thread keyboard-state bridge.
//!
//! A caller must retain `Bridge` in its target record before calling `start`;
//! shutdown must verify `stop`
//! before allowing target lifecycle retirement. Dropping this value never sends a
//! target command or asserts that an attempted start was cleaned up.
use anyhow::{Context, Result, ensure};
use std::ptr;
use std::sync::atomic::Ordering;
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE,
        INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    System::{
        Memory::{
            CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
            PAGE_READWRITE, UnmapViewOfFile,
        },
        Threading::{
            CreateEventW, GetCurrentProcess, GetProcessId, OpenProcess, PROCESS_DUP_HANDLE,
            PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject,
        },
    },
};

pub(crate) type Command = unsafe extern "C" fn(u32, u64, u32, u64, u64) -> bool;
pub(crate) type Version = unsafe extern "C" fn() -> u32;

pub(crate) const ABI_VERSION: u32 = 1;
pub(crate) const ACTION_START: u32 = 1;
pub(crate) const ACTION_STOP: u32 = 2;
pub(crate) const STATUS_OK: u32 = 0;
pub(crate) const STATUS_DISABLED: u32 = 1;
pub(crate) const STATUS_BUSY: u32 = 2;
pub(crate) const STATUS_INVALID: u32 = 3;
pub(crate) const STATUS_PUBLISH_FAILED: u32 = 4;
pub(crate) const STATUS_HELD: u32 = 5;
const STATUS_INITIAL: u32 = u32::MAX;
const START_TIMEOUT_MS: u32 = 5000;
const STOP_TIMEOUT_MS: u32 = 2000;
const PROCESS_ACCESS: u32 =
    PROCESS_DUP_HANDLE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;

/// Fixed 32-byte loader-to-target pipe body (the instance is the outer API argument).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CommandPacket {
    pub epoch: u64,
    pub reply_mapping: u64,
    pub reply_event: u64,
    pub action: u32,
    pub reserved: u32,
}
const _: () = {
    assert!(std::mem::size_of::<CommandPacket>() == 32);
    assert!(std::mem::align_of::<CommandPacket>() == 8);
};

type Phase = BridgePhase;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BridgePhase {
    Inactive,
    StartAttempted,
    Armed,
    StopAttempted,
}

struct Pending {
    action: u32,
    exchange: Exchange,
}

/// Per-target owner; keep it in the retained target record across attach and shutdown.
pub(crate) struct Bridge {
    pid: u32,
    instance: u32,
    epoch: u64,
    phase: Phase,
    armed: bool,
    pending: Option<Pending>,
}

impl Bridge {
    /// Binds to the established target lifecycle generation without creating a new identity.
    pub(crate) fn new(pid: u32, instance: u32, epoch: u64) -> Result<Self> {
        ensure!(pid != 0 && instance != 0, "invalid keyboard bridge target");
        ensure!(
            epoch != 0,
            "invalid zero keyboard bridge lifecycle generation"
        );
        debug_assert_eq!(std::mem::size_of::<CommandPacket>(), 32);
        Ok(Self {
            pid,
            instance,
            epoch,
            phase: Phase::Inactive,
            armed: false,
            pending: None,
        })
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }

    pub(crate) fn armed(&self) -> bool {
        self.armed
    }

    pub(crate) fn attempted(&self) -> bool {
        self.phase != Phase::Inactive
    }

    /// Starts the target provider and requires an acknowledged STATUS_OK before input.
    pub(crate) fn start(
        &mut self,
        retained_process: HANDLE,
        command: impl FnMut(u32, u64, u32, u64, u64) -> Result<bool>,
    ) -> Result<()> {
        ensure!(
            self.phase == Phase::Inactive,
            "keyboard bridge start was already attempted"
        );
        self.phase = Phase::StartAttempted;
        let status = match self.transact(retained_process, command, ACTION_START, START_TIMEOUT_MS)
        {
            Ok(status) => status,
            Err(error) => {
                if self.pending.is_none() {
                    // Ok(false) proves that no command was transferred and its exact
                    // remote reply handles were reclaimed. A true/timeout keeps state.
                    self.phase = Phase::Inactive;
                }
                return Err(error).context("start target keyboard-state provider");
            }
        };
        if status == STATUS_OK {
            self.phase = Phase::Armed;
            self.armed = true;
            Ok(())
        } else {
            // Keep the attempted state: Stop must obtain a verified inactive reply
            // before the caller may regard cleanup as complete.
            Err(status_error(ACTION_START, status))
        }
    }

    /// Stops only after the caller has released keys and drained all saved recipients.
    /// An inactive bridge has no target state to retire and needs no command.
    pub(crate) fn stop(
        &mut self,
        retained_process: HANDLE,
        command: impl FnMut(u32, u64, u32, u64, u64) -> Result<bool>,
    ) -> Result<()> {
        if self.phase == Phase::Inactive {
            return Ok(());
        }
        ensure!(
            self.pending.is_none(),
            "keyboard bridge has an unresolved command"
        );
        let previous = self.phase;
        self.phase = Phase::StopAttempted;
        let status = match self.transact(retained_process, command, ACTION_STOP, STOP_TIMEOUT_MS) {
            Ok(status) => status,
            Err(error) => {
                if self.pending.is_none() {
                    // No transfer: preserve the last verified phase. If transfer status
                    // is uncertain, transact retains Pending and StopAttempted remains.
                    self.phase = previous;
                }
                return Err(error).context("stop target keyboard-state provider");
            }
        };
        if status == STATUS_OK || status == STATUS_DISABLED {
            self.phase = Phase::Inactive;
            self.armed = false;
            Ok(())
        } else {
            self.phase = Phase::StopAttempted;
            Err(status_error(ACTION_STOP, status))
        }
    }

    fn transact(
        &mut self,
        retained_process: HANDLE,
        mut command: impl FnMut(u32, u64, u32, u64, u64) -> Result<bool>,
        action: u32,
        timeout_ms: u32,
    ) -> Result<u32> {
        ensure!(
            self.pending.is_none(),
            "keyboard bridge already has an unresolved command"
        );
        let exchange = Exchange::create_local(self.pid, retained_process)?;
        self.pending = Some(Pending { action, exchange });
        if let Err(error) = self
            .pending
            .as_mut()
            .expect("pending exchange was just installed")
            .exchange
            .duplicate_reply_handles()
        {
            let reclaim = self
                .pending
                .as_mut()
                .expect("pending exchange was just installed")
                .exchange
                .reclaim_remote();
            match reclaim {
                Ok(()) => {
                    self.pending.take();
                    return Err(error).context("prepare keyboard-state reply handles");
                }
                Err(reclaim_error) => {
                    return Err(error).context(format!(
                        "prepare keyboard-state reply handles; remote ownership remains pending because reclamation failed: {reclaim_error:#}"
                    ));
                }
            }
        }
        ensure_original_alive(retained_process, self.pid)
            .context("recheck keyboard-state target before command transfer")?;
        let pending = self.pending.as_ref().unwrap();
        let packet = CommandPacket {
            epoch: self.epoch,
            reply_mapping: pending.exchange.remote_mapping,
            reply_event: pending.exchange.remote_event,
            action,
            reserved: 0,
        };
        debug_assert_eq!(packet.reserved, 0);

        // Only Ok(false) proves no write/transfer. An error is uncertain: retain
        // Pending and its local reply resources; never cross-close target handles.
        let transferred = command(
            self.instance,
            packet.epoch,
            packet.action,
            packet.reply_mapping,
            packet.reply_event,
        )
        .context("keyboard-state command transport is uncertain")?;
        if !transferred {
            let reclaim = self
                .pending
                .as_mut()
                .expect("pending exchange was just installed")
                .exchange
                .reclaim_remote();
            if reclaim.is_ok() {
                self.pending.take();
            }
            reclaim?;
            anyhow::bail!("keyboard-state loader did not transfer the command");
        }

        let pending = self.pending.as_ref().unwrap();
        let status = pending.exchange.wait(timeout_ms).with_context(|| {
            format!(
                "keyboard-state command {} acknowledgement timed out or was invalid",
                pending.action
            )
        })?;
        // The target handler owns and closes both duplicated handles after writing
        // the status and signaling. Releasing local channel handles is safe after ack.
        self.pending.take();
        Ok(status)
    }
}

fn status_error(action: u32, status: u32) -> anyhow::Error {
    let name = match status {
        STATUS_OK => "OK",
        STATUS_DISABLED => "disabled",
        STATUS_BUSY => "busy",
        STATUS_INVALID => "invalid",
        STATUS_PUBLISH_FAILED => "publication failed",
        STATUS_HELD => "held keys remain",
        _ => "unknown status",
    };
    anyhow::anyhow!(
        "keyboard-state action {action} was not acknowledged as ready ({name}: {status})"
    )
}

struct Exchange {
    mapping: Mapping,
    event: OwnedHandle,
    process: OwnedHandle,
    remote_mapping: u64,
    remote_event: u64,
}

impl Exchange {
    fn create_local(pid: u32, retained_process: HANDLE) -> Result<Self> {
        ensure_original_alive(retained_process, pid)?;
        let process = OwnedHandle(unsafe { OpenProcess(PROCESS_ACCESS, 0, pid) });
        ensure!(
            !process.0.is_null(),
            "open keyboard-state target PID {pid}: {}",
            std::io::Error::last_os_error()
        );
        ensure_original_alive(retained_process, pid)?;
        let mapping = Mapping::create()?;
        unsafe { ptr::write_volatile(mapping.view.cast::<u32>(), STATUS_INITIAL) };
        let event = OwnedHandle(unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) });
        ensure!(
            !event.0.is_null(),
            "create keyboard-state acknowledgement event: {}",
            std::io::Error::last_os_error()
        );
        Ok(Self {
            mapping,
            event,
            process,
            remote_mapping: 0,
            remote_event: 0,
        })
    }

    fn duplicate_reply_handles(&mut self) -> Result<()> {
        self.remote_mapping = duplicate(self.process.0, self.mapping.handle)?;
        self.remote_event = duplicate(self.process.0, self.event.0)?;
        Ok(())
    }

    fn wait(&self, timeout_ms: u32) -> Result<u32> {
        let wait = unsafe { WaitForSingleObject(self.event.0, timeout_ms) };
        ensure!(
            wait == WAIT_OBJECT_0,
            "keyboard-state acknowledgement wait failed (result {wait}, last error {})",
            std::io::Error::last_os_error()
        );
        std::sync::atomic::fence(Ordering::Acquire);
        let status = unsafe { ptr::read_volatile(self.mapping.view.cast::<u32>()) };
        ensure!(
            status != STATUS_INITIAL,
            "keyboard-state event signaled without status"
        );
        Ok(status)
    }

    /// Used only before command transfer, when setup failed or the loader guarantees
    /// that Ok(false) means no transfer began. Each ID is cleared only after confirmed close.
    fn reclaim_remote(&mut self) -> Result<()> {
        let mapping = close_remote_source(self.process.0, &mut self.remote_mapping);
        let event = close_remote_source(self.process.0, &mut self.remote_event);
        mapping.and(event)
    }
}

struct Mapping {
    handle: HANDLE,
    view: *mut u8,
}

impl Mapping {
    fn create() -> Result<Self> {
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                ptr::null(),
                PAGE_READWRITE,
                0,
                std::mem::size_of::<u32>() as u32,
                ptr::null(),
            )
        };
        ensure!(
            !handle.is_null(),
            "create keyboard-state reply mapping: {}",
            std::io::Error::last_os_error()
        );
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 4) }.Value as *mut u8;
        if view.is_null() {
            unsafe { CloseHandle(handle) };
            anyhow::bail!(
                "map keyboard-state reply: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(Self { handle, view })
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.view.cast(),
            });
            CloseHandle(self.handle);
        }
    }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CloseHandle(self.0) };
        }
    }
}

fn ensure_original_alive(retained_process: HANDLE, expected_pid: u32) -> Result<()> {
    ensure!(
        !retained_process.is_null(),
        "missing retained target process handle"
    );
    let wait = unsafe { WaitForSingleObject(retained_process, 0) };
    ensure!(
        wait == WAIT_TIMEOUT,
        "original keyboard-state target is no longer live (wait result {wait})"
    );
    ensure!(
        unsafe { GetProcessId(retained_process) } == expected_pid,
        "retained keyboard-state process handle does not match PID {expected_pid}"
    );
    Ok(())
}

fn duplicate(process: HANDLE, local: HANDLE) -> Result<u64> {
    let mut remote = ptr::null_mut();
    ensure!(
        unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                local,
                process,
                &mut remote,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        } != 0
            && !remote.is_null(),
        "duplicate keyboard-state reply handle: {}",
        std::io::Error::last_os_error()
    );
    Ok(remote as usize as u64)
}

fn close_remote_source(process: HANDLE, remote: &mut u64) -> Result<()> {
    if *remote == 0 {
        return Ok(());
    }
    let mut local = ptr::null_mut();
    ensure!(
        unsafe {
            DuplicateHandle(
                process,
                *remote as usize as HANDLE,
                GetCurrentProcess(),
                &mut local,
                0,
                0,
                DUPLICATE_CLOSE_SOURCE | DUPLICATE_SAME_ACCESS,
            )
        } != 0,
        "reclaim rejected keyboard-state reply handle: {}",
        std::io::Error::last_os_error()
    );
    if !local.is_null() {
        unsafe { CloseHandle(local) };
    }
    *remote = 0;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::{
        Foundation::GetHandleInformation, System::Threading::GetCurrentProcessId,
    };

    fn assert_open(handle: HANDLE) {
        let mut flags = 0;
        assert_ne!(unsafe { GetHandleInformation(handle, &mut flags) }, 0);
    }

    #[test]
    fn uncertain_start_retains_pending_and_remote_reply_ownership() {
        let process = unsafe { GetCurrentProcess() };
        let mut bridge = Bridge::new(unsafe { GetCurrentProcessId() }, 1, 7).unwrap();
        let mut remote = None;
        let error = bridge
            .start(process, |instance, epoch, action, mapping, event| {
                assert_eq!((instance, epoch, action), (1, 7, ACTION_START));
                remote = Some((
                    OwnedHandle(mapping as usize as HANDLE),
                    OwnedHandle(event as usize as HANDLE),
                ));
                anyhow::bail!("partial pipe write");
            })
            .unwrap_err();

        assert!(format!("{error:#}").contains("transport is uncertain"));
        assert!(bridge.attempted());
        assert!(!bridge.armed());
        assert_eq!(bridge.phase, Phase::StartAttempted);
        assert!(bridge.pending.is_some());
        let mut retried = false;
        assert!(
            bridge
                .stop(process, |_, _, _, _, _| {
                    retried = true;
                    Ok(false)
                })
                .is_err()
        );
        assert!(!retried);
        drop(bridge);
        let (mapping, event) = remote.unwrap();
        assert_open(mapping.0);
        assert_open(event.0);
    }

    #[test]
    fn rejected_start_reclaims_pending_and_allows_fresh_attempt() {
        let process = unsafe { GetCurrentProcess() };
        let mut bridge = Bridge::new(unsafe { GetCurrentProcessId() }, 1, 7).unwrap();
        assert!(bridge.start(process, |_, _, _, _, _| Ok(false)).is_err());
        assert!(!bridge.attempted());
        assert!(!bridge.armed());
        assert!(bridge.pending.is_none());

        let mut retried = false;
        assert!(
            bridge
                .start(process, |_, _, _, _, _| {
                    retried = true;
                    Ok(false)
                })
                .is_err()
        );
        assert!(retried);
        assert!(bridge.pending.is_none());
    }

    #[test]
    fn uncertain_stop_preserves_armed_state_and_pending_reply() {
        let process = unsafe { GetCurrentProcess() };
        let mut bridge = Bridge::new(unsafe { GetCurrentProcessId() }, 1, 7).unwrap();
        bridge.phase = Phase::Armed;
        bridge.armed = true;
        let mut remote = None;
        assert!(
            bridge
                .stop(process, |_, _, action, mapping, event| {
                    assert_eq!(action, ACTION_STOP);
                    remote = Some((
                        OwnedHandle(mapping as usize as HANDLE),
                        OwnedHandle(event as usize as HANDLE),
                    ));
                    anyhow::bail!("partial pipe write");
                })
                .is_err()
        );
        assert!(bridge.attempted());
        assert!(bridge.armed());
        assert_eq!(bridge.phase, Phase::StopAttempted);
        assert!(bridge.pending.is_some());
        drop(bridge);
        let (mapping, event) = remote.unwrap();
        assert_open(mapping.0);
        assert_open(event.0);
    }
}
