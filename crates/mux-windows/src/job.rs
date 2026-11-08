//! Terminate the owned worker and its injector children when the controller exits.
use anyhow::{Result, ensure};
use std::{os::windows::io::AsRawHandle, process::Child};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    },
};

pub struct Job(HANDLE);
impl Job {
    pub fn own(child: &Child) -> Result<Self> {
        unsafe {
            let job = Self(CreateJobObjectW(std::ptr::null(), std::ptr::null()));
            ensure!(
                !job.0.is_null(),
                "create worker job: {}",
                std::io::Error::last_os_error()
            );
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            ensure!(
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as _,
                    std::mem::size_of_val(&limits) as u32
                ) != 0,
                "configure worker job: {}",
                std::io::Error::last_os_error()
            );
            ensure!(
                AssignProcessToJobObject(job.0, child.as_raw_handle()) != 0,
                "assign worker job: {}",
                std::io::Error::last_os_error()
            );
            Ok(job)
        }
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}
