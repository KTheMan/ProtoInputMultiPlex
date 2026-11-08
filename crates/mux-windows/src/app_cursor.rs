//! Optional, output-only cursor snapshots from an explicitly attached process.
//! No foreign cursor handle or interactive-desktop cursor is used by the host.
use anyhow::{Context, Result, ensure};
use cursor_snapshot_protocol::{
    HEADER_BYTES, MAPPING_BYTES, STATUS_CAPTURE_FAILED, STATUS_EMPTY, STATUS_HIDDEN,
    STATUS_STOPPED, STATUS_UNSUPPORTED, STATUS_VALID, SnapshotHeader,
};
use std::{
    ptr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE,
        INVALID_HANDLE_VALUE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    System::{
        Memory::{
            CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
            PAGE_READWRITE, UnmapViewOfFile,
        },
        Threading::{
            CreateMutexW, GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE,
            PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, ReleaseMutex,
            WaitForSingleObject,
        },
    },
};

pub(crate) type Start = unsafe extern "C" fn(u32, u64, u64, u64) -> bool;
pub(crate) type Associate = unsafe extern "C" fn(u32, u64, u64) -> bool;
pub(crate) type Stop = unsafe extern "C" fn(u32, u64) -> bool;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AppCursorImage {
    pub width: u32,
    pub height: u32,
    pub hotspot: (i32, i32),
    #[serde(skip)]
    pub pixels: Arc<[u8]>,
    pub identity: u64,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct AppCursorObservation {
    pub state: String,
    pub capability: bool,
    pub pid: u32,
    pub hwnd: usize,
    pub attachment_epoch: u64,
    pub association_epoch: u64,
    pub sequence: u64,
    /// Last coherent provider status; absent while unselected, disabled or stale.
    #[serde(default)]
    pub provider_status: Option<u32>,
    pub kind: u32,
    pub visible: Option<bool>,
    pub image: Option<AppCursorImage>,
}

impl AppCursorObservation {
    pub(crate) fn fallback(state: impl Into<String>, pid: u32, hwnd: usize) -> Self {
        Self {
            state: state.into(),
            pid,
            hwnd,
            ..Self::default()
        }
    }
}

static NEXT_ATTACHMENT: AtomicU64 = AtomicU64::new(1);

/// One mapping per attached target, retained across disable until a target Stop ack.
pub(crate) struct Observer {
    mapping: HANDLE,
    mutex: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    pid: u32,
    instance: u32,
    attachment: u64,
    association: u64,
    hwnd: usize,
    sequence: u64,
    latest: AppCursorObservation,
    stopping: bool,
    stop_failed: bool,
    waiting_since: Instant,
}

impl Observer {
    pub(crate) fn start(
        pid: u32,
        instance: u32,
        expected_process: HANDLE,
        mut start: impl FnMut(u32, u64, u64, u64) -> Result<bool>,
    ) -> Result<Self> {
        // An original live process object plus PID lookup prevents reuse during duplication.
        ensure!(
            unsafe { WaitForSingleObject(expected_process, 0) } == WAIT_TIMEOUT,
            "original cursor target exited"
        );
        let process = OwnedHandle(unsafe {
            OpenProcess(
                PROCESS_DUP_HANDLE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        });
        ensure!(
            !process.0.is_null(),
            "open cursor target: {}",
            std::io::Error::last_os_error()
        );
        ensure!(
            unsafe { WaitForSingleObject(expected_process, 0) } == WAIT_TIMEOUT,
            "original cursor target exited"
        );
        let attachment = NEXT_ATTACHMENT.fetch_add(1, Ordering::Relaxed);
        ensure!(attachment != 0, "cursor attachment generation exhausted");
        let mapping = OwnedHandle(unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                ptr::null(),
                PAGE_READWRITE,
                0,
                MAPPING_BYTES as u32,
                ptr::null(),
            )
        });
        ensure!(
            !mapping.0.is_null(),
            "create cursor mapping: {}",
            std::io::Error::last_os_error()
        );
        let mutex = OwnedHandle(unsafe { CreateMutexW(ptr::null(), 0, ptr::null()) });
        ensure!(
            !mutex.0.is_null(),
            "create cursor mutex: {}",
            std::io::Error::last_os_error()
        );
        let view = unsafe { MapViewOfFile(mapping.0, FILE_MAP_ALL_ACCESS, 0, 0, MAPPING_BYTES) };
        ensure!(
            !view.Value.is_null(),
            "map cursor snapshot: {}",
            std::io::Error::last_os_error()
        );
        let mut observer = Self {
            mapping: mapping.0,
            mutex: mutex.0,
            view,
            pid,
            instance,
            attachment,
            association: 0,
            hwnd: 0,
            sequence: 0,
            latest: AppCursorObservation::fallback("Waiting for application cursor", pid, 0),
            stopping: false,
            stop_failed: false,
            waiting_since: Instant::now(),
        };
        std::mem::forget(mapping);
        std::mem::forget(mutex);
        unsafe {
            ptr::write(
                observer.view.Value.cast::<SnapshotHeader>(),
                SnapshotHeader::empty(pid, attachment),
            );
        }
        let remote_mapping = duplicate(process.0, observer.mapping)?;
        let remote_mutex = match duplicate(process.0, observer.mutex) {
            Ok(handle) => handle,
            Err(error) => {
                close_rejected(process.0, remote_mapping);
                return Err(error);
            }
        };
        // Only Ok(false) proves no pipe write began. Err/true leave target copies
        // alive: local mapping closure cannot invalidate a remote mapped view.
        if !start(
            instance,
            remote_mapping as usize as u64,
            remote_mutex as usize as u64,
            attachment,
        )
        .context("cursor observer setup transport is uncertain")?
        {
            close_rejected(process.0, remote_mapping);
            close_rejected(process.0, remote_mutex);
            anyhow::bail!("cursor observer setup was not accepted");
        }
        observer.latest.attachment_epoch = attachment;
        Ok(observer)
    }

    pub(crate) fn associate(
        &mut self,
        hwnd: usize,
        mut associate: impl FnMut(u32, u64, u64) -> Result<bool>,
    ) -> Result<()> {
        ensure!(!self.stopping, "cursor observer is stopping");
        if self.hwnd == hwnd {
            return Ok(());
        }
        crate::queue_fence::Recipient::validate(hwnd, self.pid)?;
        self.association = self
            .association
            .checked_add(1)
            .context("cursor association generation exhausted")?;
        self.hwnd = hwnd;
        self.sequence = 0;
        self.waiting_since = Instant::now();
        self.latest = self.fallback("Waiting for fresh application cursor");
        match associate(self.instance, hwnd as u64, self.association) {
            Ok(true) => {}
            Ok(false) => {
                self.hwnd = 0;
                anyhow::bail!("cursor association was not accepted");
            }
            Err(error) => {
                self.hwnd = 0;
                return Err(error).context("cursor association transport is uncertain");
            }
        }
        Ok(())
    }

    fn fallback(&self, state: &str) -> AppCursorObservation {
        AppCursorObservation {
            state: state.into(),
            capability: true,
            pid: self.pid,
            hwnd: self.hwnd,
            attachment_epoch: self.attachment,
            association_epoch: self.association,
            ..AppCursorObservation::default()
        }
    }

    fn copy_snapshot(&self) -> Result<Option<(SnapshotHeader, Vec<u8>)>> {
        let wait = unsafe { WaitForSingleObject(self.mutex, 0) };
        if wait == WAIT_TIMEOUT {
            return Ok(None);
        }
        ensure!(
            wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED,
            "cursor mutex wait failed"
        );
        // An abandoned writer cannot establish a coherent record. Release ownership
        // and reject it; a subsequent fresh write may establish a valid snapshot.
        if wait == WAIT_ABANDONED {
            unsafe {
                ReleaseMutex(self.mutex);
            }
            anyhow::bail!("application cursor writer was interrupted");
        }
        let header = unsafe { ptr::read(self.view.Value.cast::<SnapshotHeader>()) };
        let valid = header.validate(MAPPING_BYTES, self.pid, self.attachment);
        let needs_pixels = !self.stopping
            && header.association_epoch == self.association
            && header.selected_hwnd == self.hwnd as u64
            && (header.sequence > self.sequence || self.latest.sequence != header.sequence);
        let pixels = if valid && needs_pixels {
            unsafe {
                std::slice::from_raw_parts(
                    self.view.Value.cast::<u8>().add(HEADER_BYTES),
                    header.payload_len as usize,
                )
                .to_vec()
            }
        } else {
            Vec::new()
        };
        unsafe {
            ReleaseMutex(self.mutex);
        }
        ensure!(valid, "malformed application cursor snapshot");
        Ok(Some((header, pixels)))
    }

    pub(crate) fn read(&mut self) -> AppCursorObservation {
        if self.stopping {
            return self.fallback("Application cursor observer is stopping");
        }
        match self.copy_snapshot() {
            Ok(None) => return self.latest.clone(),
            Err(error) => {
                self.latest = self.fallback(&error.to_string());
                return self.latest.clone();
            }
            Ok(Some((header, pixels))) => {
                if header.association_epoch != self.association
                    || header.selected_hwnd != self.hwnd as u64
                    || header.sequence == 0
                {
                    self.latest =
                        self.fallback(if self.waiting_since.elapsed() >= Duration::from_secs(2) {
                            "Application cursor has not reported a usable snapshot"
                        } else {
                            "Waiting for fresh application cursor"
                        });
                } else if header.sequence < self.sequence {
                    self.latest = self.fallback("Stale application cursor snapshot");
                } else if header.sequence > self.sequence || self.latest.sequence != header.sequence
                {
                    self.sequence = header.sequence;
                    let supported = header.status == STATUS_VALID || header.status == STATUS_HIDDEN;
                    self.latest = self.fallback(provider_state(header.status));
                    self.latest.sequence = header.sequence;
                    self.latest.provider_status = Some(header.status);
                    if supported {
                        self.latest.kind = header.kind;
                        self.latest.visible =
                            Some(header.flags & 1 != 0 && header.status != STATUS_HIDDEN);
                        if !pixels.is_empty() {
                            if !header.validate_bgra_payload(&pixels) {
                                self.latest = self.fallback("Invalid application cursor alpha");
                            } else {
                                self.latest.image = Some(AppCursorImage {
                                    width: header.width,
                                    height: header.height,
                                    hotspot: (header.hotspot_x, header.hotspot_y),
                                    identity: image_identity(
                                        header.width,
                                        header.height,
                                        header.hotspot_x,
                                        header.hotspot_y,
                                        &pixels,
                                    ),
                                    pixels: pixels.into(),
                                });
                            }
                        }
                    }
                }
            }
        }
        self.latest.clone()
    }

    pub(crate) fn stop(&mut self, mut stop: impl FnMut(u32, u64) -> Result<bool>) -> Result<()> {
        ensure!(
            !self.stop_failed,
            "cursor observer Stop remains unconfirmed after a failed transfer"
        );
        if !self.stopping {
            // Latch before the callback. Any error remains a failed Stop rather
            // than turning a later call into an implicit retry or clean result.
            self.stopping = true;
            match stop(self.instance, self.attachment) {
                Ok(true) => {
                    self.latest = self.fallback("Application cursor following disabled");
                }
                Ok(false) => {
                    self.stop_failed = true;
                    self.latest = self.fallback("Application cursor Stop was not accepted");
                    anyhow::bail!("cursor observer Stop was not accepted");
                }
                Err(error) => {
                    self.stop_failed = true;
                    self.latest = self.fallback("Application cursor Stop transfer is uncertain");
                    return Err(error).context("cursor observer Stop transport is uncertain");
                }
            }
        }
        Ok(())
    }

    pub(crate) fn stopped(&self) -> bool {
        self.stopping
            && !self.stop_failed
            && self
                .copy_snapshot()
                .ok()
                .flatten()
                .is_some_and(|(header, _)| header.status == STATUS_STOPPED)
    }
    pub(crate) fn stopping(&self) -> bool {
        self.stopping
    }
}

fn provider_state(status: u32) -> &'static str {
    match status {
        STATUS_VALID | STATUS_HIDDEN => "Following application cursor",
        STATUS_EMPTY => "Waiting for application cursor provider",
        STATUS_UNSUPPORTED => "Application cursor request or image unsupported",
        STATUS_CAPTURE_FAILED => "Application cursor provider failed",
        STATUS_STOPPED => "Application cursor provider stopped",
        _ => "Application cursor image unavailable",
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        unsafe {
            UnmapViewOfFile(self.view);
            CloseHandle(self.mapping);
            CloseHandle(self.mutex);
        }
    }
}
struct OwnedHandle(HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}
fn duplicate(process: HANDLE, local: HANDLE) -> Result<HANDLE> {
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
        "duplicate cursor channel: {}",
        std::io::Error::last_os_error()
    );
    Ok(remote)
}
fn close_rejected(process: HANDLE, remote: HANDLE) {
    let mut local = ptr::null_mut();
    if unsafe {
        DuplicateHandle(
            process,
            remote,
            GetCurrentProcess(),
            &mut local,
            0,
            0,
            DUPLICATE_CLOSE_SOURCE | DUPLICATE_SAME_ACCESS,
        )
    } != 0
        && !local.is_null()
    {
        unsafe {
            CloseHandle(local);
        }
    }
}

fn image_identity(width: u32, height: u32, x: i32, y: i32, pixels: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    (width, height, x, y, pixels).hash(&mut hash);
    hash.finish()
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
    fn uncertain_start_does_not_reclaim_remote_channel_handles() {
        let mut remote = None;
        let result = Observer::start(
            unsafe { GetCurrentProcessId() },
            1,
            unsafe { GetCurrentProcess() },
            |instance, mapping, mutex, attachment| {
                assert_eq!(instance, 1);
                assert_ne!(attachment, 0);
                remote = Some((
                    OwnedHandle(mapping as usize as HANDLE),
                    OwnedHandle(mutex as usize as HANDLE),
                ));
                anyhow::bail!("partial pipe write");
            },
        );
        assert!(result.is_err());
        let (mapping, mutex) = remote.unwrap();
        assert_open(mapping.0);
        assert_open(mutex.0);
    }

    #[test]
    fn uncertain_stop_stays_unconfirmed_and_never_retries() {
        let mut remote = None;
        let mut observer = Observer::start(
            unsafe { GetCurrentProcessId() },
            1,
            unsafe { GetCurrentProcess() },
            |_, mapping, mutex, _| {
                remote = Some((
                    OwnedHandle(mapping as usize as HANDLE),
                    OwnedHandle(mutex as usize as HANDLE),
                ));
                Ok(true)
            },
        )
        .unwrap();
        assert!(
            observer
                .stop(|_, _| anyhow::bail!("partial pipe write"))
                .is_err()
        );
        assert!(observer.stopping());
        assert!(!observer.stopped());
        let mut retried = false;
        assert!(
            observer
                .stop(|_, _| {
                    retried = true;
                    Ok(true)
                })
                .is_err()
        );
        assert!(!retried);
        assert!(!observer.stopped());
        drop(observer);
        let (mapping, mutex) = remote.unwrap();
        assert_open(mapping.0);
        assert_open(mutex.0);
    }
}
