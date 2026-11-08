//! Scoped Windows power requests for explicit macro playback sessions.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::io;
use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE},
    System::{
        Power::{
            PowerClearRequest, PowerCreateRequest, PowerRequestDisplayRequired,
            PowerRequestSystemRequired, PowerSetRequest,
        },
        Threading::{GetCurrentProcessId, REASON_CONTEXT, REASON_CONTEXT_0},
    },
};

const POWER_REQUEST_CONTEXT_VERSION: u32 = 0;
const POWER_REQUEST_CONTEXT_SIMPLE_STRING: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub process_id: u32,
    pub active: bool,
    pub display_required: bool,
    pub system_required: bool,
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// Holds display and system power requirements for its lifetime.
///
/// This asks Windows to keep the display and system awake; Windows policy and
/// user initiated sleep can still override the request.
pub struct KeepAwake {
    handle: OwnedHandle,
    status: Status,
}

impl KeepAwake {
    pub fn new() -> Result<Self> {
        let mut reason = "ProtoInputMultiPlex macro playback"
            .encode_utf16()
            .collect::<Vec<_>>();
        reason.push(0);
        let context = REASON_CONTEXT {
            Version: POWER_REQUEST_CONTEXT_VERSION,
            Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            Reason: REASON_CONTEXT_0 {
                SimpleReasonString: reason.as_ptr() as *mut u16,
            },
        };
        let handle = unsafe { PowerCreateRequest(&context) };
        ensure!(
            !handle.is_null() && handle != INVALID_HANDLE_VALUE,
            "create Windows power request: {}",
            io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
        );
        let handle = OwnedHandle(handle);

        ensure!(
            unsafe { PowerSetRequest(handle.0, PowerRequestDisplayRequired) } != 0,
            "set display power request: {}",
            io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
        );
        if unsafe { PowerSetRequest(handle.0, PowerRequestSystemRequired) } == 0 {
            let error = io::Error::from_raw_os_error(unsafe { GetLastError() } as i32);
            unsafe { PowerClearRequest(handle.0, PowerRequestDisplayRequired) };
            return Err(error).context("set system power request");
        }

        Ok(Self {
            handle,
            status: Status {
                process_id: unsafe { GetCurrentProcessId() },
                active: true,
                display_required: true,
                system_required: true,
            },
        })
    }

    pub fn status(&self) -> Status {
        self.status.clone()
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        if self.status.display_required {
            unsafe { PowerClearRequest(self.handle.0, PowerRequestDisplayRequired) };
        }
        if self.status.system_required {
            unsafe { PowerClearRequest(self.handle.0, PowerRequestSystemRequired) };
        }
        self.status.active = false;
        self.status.display_required = false;
        self.status.system_required = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_and_releases_a_scoped_power_request() {
        let request = KeepAwake::new().expect("create a Windows power request");
        let status = request.status();
        assert!(status.active);
        assert!(status.display_required);
        assert!(status.system_required);
        assert_ne!(status.process_id, 0);
        drop(request);
    }
}
