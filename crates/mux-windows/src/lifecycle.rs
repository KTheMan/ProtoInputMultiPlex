//! Fail-closed, virtual-only resident backend lifecycle. Inventory never takes a lease.
use anyhow::{Context, Result, ensure};
use libloading::os::windows::Library;
use std::{
    ffi::OsString,
    os::windows::ffi::OsStringExt,
    path::PathBuf,
    ptr,
    rc::Rc,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle,
        ERROR_FILE_NOT_FOUND, FILETIME, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_ABANDONED,
        WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    System::{
        Memory::{
            CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, MapViewOfFile, OpenFileMappingW,
            PAGE_READWRITE, UnmapViewOfFile,
        },
        Threading::{
            CreateEventW, CreateMutexW, GetCurrentProcess, GetProcessTimes, ReleaseMutex,
            ResetEvent, WaitForMultipleObjects, WaitForSingleObject,
        },
    },
};

pub const WORKER_ENV: &str = "PROTOINPUTMULTIPLEX_SESSION_COMMIT";
const BUILD: u64 = 0x2026_1107_0000_0001;
const COMMIT_MAGIC: u32 = 0x4D58_434D;
const READY_MAGIC: u32 = 0x4D58_5244;
const REPLY_MAGIC: u32 = 0x4D58_5250;
const LEASE_MAGIC: u32 = 0x4D58_4C53;
pub(crate) const SETUP_ACK_CAPABILITY: u64 = 1;
const SETUP_ACK_MAGIC: u32 = 0x4D58_5341;
// Cold injector startup and DLL loading have a separate bounded budget.
// Established protocol replies retain the shorter timeout below.
const OPEN_TIMEOUT: u32 = 5000;
const TIMEOUT: u32 = 1500;
const PREPARE_TIMEOUT: u32 = 4000;

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
fn nonce() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
        ^ (u64::from(std::process::id()) << 32)
        ^ NEXT.fetch_add(1, Ordering::Relaxed)
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Record {
    magic: u32,
    abi: u32,
    bytes: u32,
    state: u32,
    session: u64,
    reserved: [u64; 5],
}

struct Mapping {
    handle: HANDLE,
    view: *mut u8,
}
impl Mapping {
    fn create_unique(name: &str, bytes: usize) -> Result<Self> {
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                ptr::null(),
                PAGE_READWRITE,
                0,
                bytes as u32,
                wide(name).as_ptr(),
            )
        };
        ensure!(
            !handle.is_null(),
            "create unique lifecycle mapping: {}",
            std::io::Error::last_os_error()
        );
        if std::io::Error::last_os_error().raw_os_error()
            == Some(windows_sys::Win32::Foundation::ERROR_ALREADY_EXISTS as i32)
        {
            unsafe {
                CloseHandle(handle);
            }
            anyhow::bail!("lifecycle mapping name collision");
        }
        Self::map(handle, FILE_MAP_READ | FILE_MAP_WRITE, bytes)
    }
    fn create(name: &str, bytes: usize) -> Result<Self> {
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                ptr::null(),
                PAGE_READWRITE,
                0,
                bytes as u32,
                wide(name).as_ptr(),
            )
        };
        ensure!(
            !handle.is_null(),
            "create lifecycle mapping: {}",
            std::io::Error::last_os_error()
        );
        Self::map(handle, FILE_MAP_READ | FILE_MAP_WRITE, bytes)
    }
    fn open(name: &str, write: bool, bytes: usize) -> Result<Self> {
        let access = if write {
            FILE_MAP_READ | FILE_MAP_WRITE
        } else {
            FILE_MAP_READ
        };
        let handle = unsafe { OpenFileMappingW(access, 0, wide(name).as_ptr()) };
        ensure!(
            !handle.is_null(),
            "open lifecycle mapping: {}",
            std::io::Error::last_os_error()
        );
        Self::map(handle, access, bytes)
    }
    fn map(handle: HANDLE, access: u32, bytes: usize) -> Result<Self> {
        let view = unsafe { MapViewOfFile(handle, access, 0, 0, bytes) }.Value as *mut u8;
        if view.is_null() {
            unsafe {
                CloseHandle(handle);
            }
            anyhow::bail!("map lifecycle record: {}", std::io::Error::last_os_error());
        }
        Ok(Self { handle, view })
    }
    fn record(&self) -> Record {
        unsafe { ptr::read_volatile(self.view.cast()) }
    }
    fn state(&self) -> &AtomicU32 {
        unsafe { &*self.view.add(12).cast() }
    }
}
impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe {
            UnmapViewOfFile(
                windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: self.view.cast(),
                },
            );
            CloseHandle(self.handle);
        }
    }
}

/// One supervisor-owned transaction for every target touched by its child worker.
pub struct CommitOwner {
    mapping: Mapping,
    name: String,
    session: u64,
}
// Kernel mappings may be moved between supervisor threads; this owner is never cloned.
unsafe impl Send for CommitOwner {}
impl CommitOwner {
    pub fn create() -> Result<Self> {
        let session = nonce();
        let name = format!(
            "Local\\ProtoInputMultiPlex.Commit.{}.{session}",
            std::process::id()
        );
        let mapping = Mapping::create_unique(&name, 64)?;
        unsafe {
            ptr::write(
                mapping.view.cast(),
                Record {
                    magic: COMMIT_MAGIC,
                    abi: 1,
                    bytes: 64,
                    session,
                    ..Record::default()
                },
            );
        }
        Ok(Self {
            mapping,
            name,
            session,
        })
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Only call after the worker emitted Stopped and exited successfully.
    pub fn commit(&self) -> Result<()> {
        let record = self.mapping.record();
        ensure!(
            valid_commit(record) && record.session == self.session,
            "lifecycle commit identity changed"
        );
        self.mapping
            .state()
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| anyhow::anyhow!("lifecycle transaction is not pending"))?;
        Ok(())
    }
}
fn valid_commit(record: Record) -> bool {
    record.magic == COMMIT_MAGIC && record.abi == 1 && record.bytes == 64 && record.session != 0
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct Readiness {
    pub magic: u32,
    pub abi: u32,
    pub bytes: u32,
    pub sequence: u32,
    pub state: u32,
    pub owner_pid: u32,
    pub bits: u32,
    pub error: u32,
    pub creation: u64,
    pub generation: u64,
    pub session: u64,
    pub build: u64,
    pub reserved: [u64; 8],
}

impl Readiness {
    fn header_matches(&self) -> bool {
        self.magic == READY_MAGIC && self.abi == 1 && self.bytes == 128
    }

    fn identity_matches(&self, creation: u64, bits: u32) -> bool {
        self.creation == creation && self.bits == bits
    }

    fn build_matches(&self) -> bool {
        self.build == BUILD
    }
}

fn readiness_sample(mapping: &Mapping) -> (Readiness, u32) {
    let first: Readiness = unsafe { ptr::read_volatile(mapping.view.cast()) };
    std::sync::atomic::fence(Ordering::Acquire);
    let sequence = unsafe { ptr::read_volatile(mapping.view.add(12).cast::<u32>()) };
    (first, sequence)
}

pub(crate) fn readiness(pid: u32, creation: u64, bits: u32) -> Option<Readiness> {
    let mapping = Mapping::open(
        &format!("Local\\ProtoInputMultiPlex.Lifecycle.{pid}.{creation}"),
        false,
        128,
    )
    .ok()?;
    for _ in 0..4 {
        let (first, sequence) = readiness_sample(&mapping);
        if first.sequence == sequence && sequence & 1 == 0 {
            return (first.header_matches()
                && first.identity_matches(creation, bits)
                && first.build_matches())
            .then_some(first);
        }
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnReadinessStatus {
    IdentityUnavailable,
    Absent,
    ReadFailed,
    Unstable,
    InvalidHeader,
    InvalidIdentity,
    InvalidBuild,
    InvalidState,
    Observed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeReadinessState {
    Initializing,
    Unclaimed,
    Active,
    Prepared,
    Poisoned,
    Reusable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeReadinessError {
    None,
    AccessDenied,
    InvalidHandle,
    InvalidData,
    InvalidParameter,
    BrokenPipe,
    Busy,
    WaitTimeout,
    Timeout,
    Unknown,
}

/// Current native lifecycle facts for this process only, never an input receipt.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct OwnReadinessObservation {
    pub status: OwnReadinessStatus,
    pub present: Option<bool>,
    pub coherent: Option<bool>,
    pub header_matches: Option<bool>,
    pub identity_matches: Option<bool>,
    pub build_matches: Option<bool>,
    pub binding_present: Option<bool>,
    pub state: Option<NativeReadinessState>,
    pub error: Option<NativeReadinessError>,
}

impl OwnReadinessObservation {
    fn unavailable(status: OwnReadinessStatus, present: Option<bool>) -> Self {
        Self {
            status,
            present,
            coherent: None,
            header_matches: None,
            identity_matches: None,
            build_matches: None,
            binding_present: None,
            state: None,
            error: None,
        }
    }
}

fn own_readiness_sample(
    record: Readiness,
    sequence: u32,
    creation: u64,
    bits: u32,
) -> OwnReadinessObservation {
    let mut observed =
        OwnReadinessObservation::unavailable(OwnReadinessStatus::Unstable, Some(true));
    let coherent = record.sequence == sequence && sequence & 1 == 0;
    observed.coherent = Some(coherent);
    if !coherent {
        return observed;
    }
    let header_matches = record.header_matches();
    let identity_matches = record.identity_matches(creation, bits);
    let build_matches = record.build_matches();
    observed.header_matches = Some(header_matches);
    observed.identity_matches = Some(identity_matches);
    observed.build_matches = Some(build_matches);
    observed.status = if !header_matches {
        OwnReadinessStatus::InvalidHeader
    } else if !identity_matches {
        OwnReadinessStatus::InvalidIdentity
    } else if !build_matches {
        OwnReadinessStatus::InvalidBuild
    } else {
        OwnReadinessStatus::InvalidState
    };
    if !(header_matches && identity_matches && build_matches) {
        return observed;
    }
    observed.state = match record.state {
        0 => Some(NativeReadinessState::Initializing),
        1 => Some(NativeReadinessState::Unclaimed),
        2 => Some(NativeReadinessState::Active),
        3 => Some(NativeReadinessState::Prepared),
        4 => Some(NativeReadinessState::Poisoned),
        5 => Some(NativeReadinessState::Reusable),
        _ => None,
    };
    if observed.state.is_none() {
        return observed;
    }
    observed.binding_present =
        Some(record.owner_pid != 0 && record.generation != 0 && record.session != 0);
    observed.error = Some(match record.error {
        0 => NativeReadinessError::None,
        5 => NativeReadinessError::AccessDenied,
        6 => NativeReadinessError::InvalidHandle,
        13 => NativeReadinessError::InvalidData,
        87 => NativeReadinessError::InvalidParameter,
        109 => NativeReadinessError::BrokenPipe,
        170 => NativeReadinessError::Busy,
        258 => NativeReadinessError::WaitTimeout,
        1460 => NativeReadinessError::Timeout,
        _ => NativeReadinessError::Unknown,
    });
    observed.status = OwnReadinessStatus::Observed;
    observed
}

/// Attempt one native Ready sample for the calling process's own creation.
/// Missing, unstable and rejected records remain distinct; this never takes a lease.
pub fn own_readiness_observation() -> OwnReadinessObservation {
    let mut created = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut exited = created;
    let mut kernel = created;
    let mut user = created;
    if unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    } == 0
    {
        return OwnReadinessObservation::unavailable(OwnReadinessStatus::IdentityUnavailable, None);
    }
    let creation = u64::from(created.dwHighDateTime) << 32 | u64::from(created.dwLowDateTime);
    let name = format!(
        "Local\\ProtoInputMultiPlex.Lifecycle.{}.{creation}",
        std::process::id()
    );
    let wide_name = wide(&name);
    let (handle, open_error) = unsafe {
        let handle = OpenFileMappingW(FILE_MAP_READ, 0, wide_name.as_ptr());
        let error = if handle.is_null() { GetLastError() } else { 0 };
        (handle, error)
    };
    if handle.is_null() {
        return if open_error == ERROR_FILE_NOT_FOUND {
            OwnReadinessObservation::unavailable(OwnReadinessStatus::Absent, Some(false))
        } else {
            OwnReadinessObservation::unavailable(OwnReadinessStatus::ReadFailed, None)
        };
    }
    let mapping = match Mapping::map(handle, FILE_MAP_READ, size_of::<Readiness>()) {
        Ok(mapping) => mapping,
        Err(_) => {
            return OwnReadinessObservation::unavailable(
                OwnReadinessStatus::ReadFailed,
                Some(true),
            );
        }
    };
    let (record, sequence) = readiness_sample(&mapping);
    own_readiness_sample(record, sequence, creation, usize::BITS)
}
fn lease_name(pid: u32, creation: u64) -> String {
    format!("Local\\ProtoInputMultiPlex.Lease.{pid}.{creation}")
}
pub(crate) fn lease_state(pid: u32, creation: u64) -> Option<u32> {
    let mapping = Mapping::open(&lease_name(pid, creation), false, 64).ok()?;
    let record = mapping.record();
    if record.magic != LEASE_MAGIC
        || record.abi != 1
        || record.bytes != 64
        || record.session != creation
    {
        return None;
    }
    let state = mapping.state().load(Ordering::Acquire);
    let owner_alive = crate::targets::ProcessIdentity::open(record.reserved[1] as u32)
        .is_ok_and(|owner| owner.creation == record.reserved[0]);
    Some(if state == 2 && !owner_alive { 1 } else { state })
}

pub(crate) struct TargetLease {
    mutex: HANDLE,
    marker: Mapping,
    prepared: bool,
}
impl TargetLease {
    pub fn acquire(identity: &crate::targets::ProcessIdentity) -> Result<Self> {
        let owner = crate::targets::ProcessIdentity::open(std::process::id())?;
        let mutex = unsafe {
            CreateMutexW(
                ptr::null(),
                0,
                wide(&format!(
                    "Local\\ProtoInputMultiPlex.Controller.{}.{}",
                    identity.pid, identity.creation
                ))
                .as_ptr(),
            )
        };
        ensure!(!mutex.is_null(), "create target controller lease");
        let result = unsafe { WaitForSingleObject(mutex, 0) };
        if result != WAIT_OBJECT_0 && result != WAIT_ABANDONED {
            unsafe {
                CloseHandle(mutex);
            }
            anyhow::bail!("PID {} already has a controller", identity.pid);
        }
        let marker = match Mapping::create(&lease_name(identity.pid, identity.creation), 64) {
            Ok(marker) => marker,
            Err(error) => {
                unsafe {
                    ReleaseMutex(mutex);
                    CloseHandle(mutex);
                }
                return Err(error);
            }
        };
        let mut lease = Self {
            mutex,
            marker,
            prepared: false,
        };
        let previous = lease.marker.record();
        if result == WAIT_ABANDONED || (previous.magic == LEASE_MAGIC && previous.state != 3) {
            lease.marker.state().store(1, Ordering::Release);
            anyhow::bail!(
                "PID {} has an abandoned or dirty controller; restart the application",
                identity.pid
            );
        }
        lease.marker.state().store(0, Ordering::Release);
        unsafe {
            ptr::write(
                lease.marker.view.cast(),
                Record {
                    magic: LEASE_MAGIC,
                    abi: 1,
                    bytes: 64,
                    state: 0,
                    session: identity.creation,
                    reserved: [owner.creation, u64::from(owner.pid), 0, 0, 0],
                },
            );
        }
        lease.marker.state().store(2, Ordering::Release);
        // The target holds this marker until exit, including a worker crash before injection.
        if previous.magic != LEASE_MAGIC {
            let _target_marker = duplicate(lease.marker.handle, identity.handle())?;
        }
        lease.prepared = false;
        Ok(lease)
    }
    fn prepared(&mut self) {
        self.marker.state().store(3, Ordering::Release);
        self.prepared = true;
    }
}
impl Drop for TargetLease {
    fn drop(&mut self) {
        if !self.prepared {
            self.marker.state().store(1, Ordering::Release);
        }
        unsafe {
            ReleaseMutex(self.mutex);
            CloseHandle(self.mutex);
        }
    }
}

#[repr(C)]
#[derive(Default)]
struct Reply {
    magic: u32,
    abi: u32,
    bytes: u32,
    result: u32,
    creation: u64,
    generation: u64,
    session: u64,
    native_state: u32,
    error: u32,
    reserved: [u64; 2],
}
#[repr(C)]
struct Begin {
    creation: u64,
    generation: u64,
    session: u64,
    commit: u64,
    reply: u64,
    event: u64,
}
#[repr(C)]
struct Retire {
    generation: u64,
    session: u64,
    reply: u64,
    event: u64,
}
// The native protocol fixes these layouts for both MSVC pointer widths.
const _: () = {
    assert!(std::mem::size_of::<Record>() == 64);
    assert!(std::mem::size_of::<Readiness>() == 128);
    assert!(std::mem::size_of::<Reply>() == 64);
    assert!(std::mem::size_of::<Begin>() == 48);
    assert!(std::mem::size_of::<Retire>() == 32);
    assert!(std::mem::align_of::<Record>() == 8);
    assert!(std::mem::align_of::<Readiness>() == 8);
    assert!(std::mem::align_of::<Reply>() == 8);
    assert!(std::mem::align_of::<Begin>() == 8);
    assert!(std::mem::align_of::<Retire>() == 8);
};
type Open = unsafe extern "C" fn(u64, u32, u64, *const u16, bool, u32) -> u32;
type BeginFn = unsafe extern "C" fn(u32, *const Begin) -> bool;
type RetireFn = unsafe extern "C" fn(u32, *const Retire) -> bool;
type Close = unsafe extern "C" fn(u32) -> bool;
#[derive(Clone)]
pub(crate) struct NativeLifecycleApi {
    open: Open,
    begin: BeginFn,
    retire: RetireFn,
    close: Close,
}

#[derive(Clone)]
pub(crate) enum Api {
    Native(NativeLifecycleApi),
    Rust(Rc<crate::protoinput::control::Registry>),
}
impl Api {
    pub(crate) fn rust(registry: Rc<crate::protoinput::control::Registry>) -> Self {
        Self::Rust(registry)
    }

    pub unsafe fn load(library: &Library) -> Option<Self> {
        unsafe {
            let version = library
                .get::<unsafe extern "C" fn() -> u32>(b"MuxLifecycleVersion\0")
                .ok()?;
            if version() != 1 {
                return None;
            }
            Some(Self::Native(NativeLifecycleApi {
                open: *library.get(b"MuxLifecycleOpen\0").ok()?,
                begin: *library.get(b"MuxLifecycleBegin\0").ok()?,
                retire: *library.get(b"MuxLifecyclePrepareRetire\0").ok()?,
                close: *library.get(b"MuxLifecycleClose\0").ok()?,
            }))
        }
    }

    fn open_connection(
        &self,
        identity: &crate::targets::ProcessIdentity,
        folder: &[u16],
        fresh: bool,
    ) -> Result<u32> {
        match self {
            Self::Native(NativeLifecycleApi { open, .. }) => {
                identity.ensure_alive()?;
                let instance = unsafe {
                    open(
                        identity.handle() as u64,
                        identity.pid,
                        identity.creation,
                        folder.as_ptr(),
                        fresh,
                        OPEN_TIMEOUT,
                    )
                };
                ensure!(
                    instance != 0,
                    "bounded lifecycle open failed for PID {}",
                    identity.pid
                );
                retain_verified_native_instance(
                    instance,
                    || {
                        identity.ensure_alive()?;
                        // A paired older loader/hooks build can agree with itself.
                        // Rust must independently enforce its resident contract.
                        let ready = readiness(identity.pid, identity.creation, identity.bits)
                            .context("opened native target has incompatible resident identity, architecture or exact build")?;
                        ensure!(
                            matches!(ready.state, 1 | 5),
                            "opened native target is not available for lifecycle Begin"
                        );
                        identity.ensure_alive()
                    },
                    |instance| self.close(instance),
                )
            }
            Self::Rust(registry) => {
                let path = folder
                    .strip_suffix(&[0])
                    .context("runtime path is not terminated")?;
                ensure!(!path.contains(&0), "runtime path contains embedded NUL");
                registry.open(
                    identity,
                    &PathBuf::from(OsString::from_wide(path)),
                    fresh,
                    OPEN_TIMEOUT,
                )
            }
        }
    }

    fn begin(&self, instance: u32, begin: &Begin) -> Result<bool> {
        match self {
            Self::Native(NativeLifecycleApi { begin: send, .. }) => {
                Ok(unsafe { send(instance, begin) })
            }
            Self::Rust(registry) => registry.begin_checked(
                instance,
                crate::protoinput::wire::Command::Begin {
                    creation: begin.creation,
                    generation: begin.generation,
                    session: begin.session,
                    commit: begin.commit,
                    reply: begin.reply,
                    event: begin.event,
                },
            ),
        }
    }

    fn retire(&self, instance: u32, retire: &Retire) -> Result<bool> {
        match self {
            Self::Native(NativeLifecycleApi { retire: send, .. }) => {
                Ok(unsafe { send(instance, retire) })
            }
            Self::Rust(registry) => registry.transfer(
                instance,
                crate::protoinput::wire::Command::Retire {
                    generation: retire.generation,
                    session: retire.session,
                    reply: retire.reply,
                    event: retire.event,
                },
            ),
        }
    }

    fn close(&self, instance: u32) -> Result<()> {
        match self {
            Self::Native(NativeLifecycleApi { close, .. }) => {
                ensure!(unsafe { close(instance) }, "native lifecycle Close failed");
                Ok(())
            }
            Self::Rust(registry) => registry.close(instance),
        }
    }
    pub fn attach(
        self,
        identity: crate::targets::ProcessIdentity,
        folder: &[u16],
    ) -> Result<Attachment> {
        let eligibility = crate::targets::eligibility(identity.pid);
        ensure!(eligibility.can_start(true), "{}", eligibility.reason);
        let lease = TargetLease::acquire(&identity)?;
        identity.ensure_alive()?;
        // Recheck readiness inside the exclusive lease. A concurrent clean owner may have changed it.
        let ready = readiness(identity.pid, identity.creation, identity.bits);
        let fresh = crate::targets::hooks_loaded(identity.pid).map(|loaded| !loaded)?;
        ensure!(
            fresh || ready.is_some_and(|record| record.state == 5),
            "target lifecycle changed before attachment"
        );
        let commit_name = std::env::var(WORKER_ENV)
            .context("virtual lifecycle requires a supervisor transaction")?;
        let commit = Mapping::open(&commit_name, false, 64)?;
        let record = commit.record();
        ensure!(
            valid_commit(record) && record.state == 0,
            "lifecycle transaction is not pending"
        );
        let generation = nonce();
        let instance = self.open_connection(&identity, folder, fresh)?;
        let mut attachment = Attachment {
            api: self.clone(),
            identity,
            lease,
            instance,
            generation,
            session: record.session,
            prepared: false,
        };
        // Provision the persistent checked-setup channel before duplicating the
        // one-shot Begin handles. Creation failure has transferred nothing.
        if let Self::Rust(registry) = &self {
            registry.prepare_setup(instance, generation, record.session)?;
        }
        let exchange = Exchange::create(&attachment.identity)?;
        let remote_commit = match duplicate(commit.handle, attachment.identity.handle()) {
            Ok(handle) => handle,
            Err(error) => {
                exchange.reclaim();
                return Err(error);
            }
        };
        let begin = Begin {
            creation: attachment.identity.creation,
            generation,
            session: record.session,
            commit: remote_commit,
            reply: exchange.remote_mapping,
            event: exchange.remote_event,
        };
        let sent = self.begin(instance, &begin)?;
        if !sent {
            exchange.reclaim();
            reclaim(remote_commit, attachment.identity.handle());
        }
        ensure!(sent, "native lifecycle Begin transport failed");
        exchange.wait(
            attachment.identity.creation,
            generation,
            record.session,
            2,
            TIMEOUT,
        )?;
        attachment.prepared = false;
        Ok(attachment)
    }
}

fn retain_verified_native_instance(
    instance: u32,
    verify: impl FnOnce() -> Result<()>,
    close: impl FnOnce(u32) -> Result<()>,
) -> Result<u32> {
    if let Err(error) = verify() {
        return match close(instance) {
            Ok(()) => Err(error),
            Err(close_error) => Err(error.context(format!(
                "also failed to close rejected native instance: {close_error:#}"
            ))),
        };
    }
    Ok(instance)
}
pub(crate) struct Attachment {
    api: Api,
    identity: crate::targets::ProcessIdentity,
    lease: TargetLease,
    pub instance: u32,
    generation: u64,
    session: u64,
    prepared: bool,
}
impl Attachment {
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub fn prepare(&mut self) -> Result<()> {
        if self.identity.ensure_alive().is_err() {
            let _ = self.api.close(self.instance);
            self.prepared = true;
            self.lease.prepared();
            return Ok(());
        }
        let exchange = Exchange::create(&self.identity)?;
        let retire = Retire {
            generation: self.generation,
            session: self.session,
            reply: exchange.remote_mapping,
            event: exchange.remote_event,
        };
        let sent = self.api.retire(self.instance, &retire)?;
        if !sent {
            exchange.reclaim();
        }
        ensure!(sent, "native lifecycle Prepare transport failed");
        exchange.wait(
            self.identity.creation,
            self.generation,
            self.session,
            3,
            PREPARE_TIMEOUT,
        )?;
        self.api.close(self.instance)?;
        self.prepared = true;
        self.lease.prepared();
        Ok(())
    }
}
impl Drop for Attachment {
    fn drop(&mut self) {
        if !self.prepared {
            let _ = self.api.close(self.instance);
        }
    }
}

fn duplicate(handle: HANDLE, target: HANDLE) -> Result<u64> {
    let mut remote = ptr::null_mut();
    ensure!(
        unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                handle,
                target,
                &mut remote,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        } != 0,
        "duplicate lifecycle handle: {}",
        std::io::Error::last_os_error()
    );
    Ok(remote as u64)
}
fn reclaim(remote: u64, target: HANDLE) {
    unsafe {
        let mut local = ptr::null_mut();
        if DuplicateHandle(
            target,
            remote as HANDLE,
            GetCurrentProcess(),
            &mut local,
            0,
            0,
            DUPLICATE_SAME_ACCESS | DUPLICATE_CLOSE_SOURCE,
        ) != 0
        {
            CloseHandle(local);
        }
    }
}
struct Exchange {
    mapping: Mapping,
    event: HANDLE,
    remote_mapping: u64,
    remote_event: u64,
    target: HANDLE,
}
impl Exchange {
    fn create(identity: &crate::targets::ProcessIdentity) -> Result<Self> {
        let mapping = Mapping::create_unique(
            &format!(
                "Local\\ProtoInputMultiPlex.Reply.{}.{}",
                std::process::id(),
                nonce()
            ),
            64,
        )?;
        let event = unsafe { CreateEventW(ptr::null(), 0, 0, ptr::null()) };
        ensure!(!event.is_null(), "create lifecycle acknowledgement event");
        let mut exchange = Self {
            mapping,
            event,
            remote_mapping: 0,
            remote_event: 0,
            target: identity.handle(),
        };
        exchange.remote_mapping = duplicate(exchange.mapping.handle, exchange.target)?;
        exchange.remote_event = match duplicate(event, exchange.target) {
            Ok(handle) => handle,
            Err(error) => {
                reclaim(exchange.remote_mapping, exchange.target);
                return Err(error);
            }
        };
        Ok(exchange)
    }
    fn reclaim(&self) {
        reclaim(self.remote_mapping, self.target);
        reclaim(self.remote_event, self.target);
    }
    fn wait(
        &self,
        creation: u64,
        generation: u64,
        session: u64,
        expected_state: u32,
        timeout: u32,
    ) -> Result<()> {
        ensure!(
            unsafe { WaitForSingleObject(self.event, timeout) } == WAIT_OBJECT_0,
            "native lifecycle acknowledgement timed out; restart the application"
        );
        let reply: Reply = unsafe { ptr::read_volatile(self.mapping.view.cast()) };
        ensure!(
            reply_valid(&reply, creation, generation, session, expected_state),
            "native lifecycle acknowledgement failed (result {}, error {}); restart the application",
            reply.result,
            reply.error
        );
        Ok(())
    }
}
impl Drop for Exchange {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.event);
        }
    }
}
fn reply_valid(
    reply: &Reply,
    creation: u64,
    generation: u64,
    session: u64,
    expected_state: u32,
) -> bool {
    reply.magic == REPLY_MAGIC
        && reply.abi == 1
        && reply.bytes == 64
        && reply.result == 1
        && reply.creation == creation
        && reply.generation == generation
        && reply.session == session
        && reply.native_state == expected_state
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SetupReply {
    magic: u32,
    abi: u32,
    bytes: u32,
    sequence: u32,
    creation: u64,
    generation: u64,
    session: u64,
    request: u64,
    result: u32,
    native_status: u32,
    hook: u32,
    reserved: u32,
}
const _: () = {
    assert!(size_of::<SetupReply>() == 64);
    assert!(std::mem::offset_of!(SetupReply, creation) == 16);
    assert!(std::mem::offset_of!(SetupReply, result) == 48);
};

impl SetupReply {
    fn verify(&self, creation: u64, generation: u64, session: u64, request: u64) -> Result<()> {
        ensure!(
            self.magic == SETUP_ACK_MAGIC
                && self.abi == 1
                && self.bytes == 64
                && self.sequence != 0
                && self.sequence & 1 == 0
                && self.reserved == 0
                && self.creation == creation
                && self.generation == generation
                && self.session == session
                && self.request == request
                && self.hook == 2,
            "checked MessageFilter installation acknowledgement has invalid identity or layout"
        );
        ensure!(
            matches!(self.result, 1 | 2) && self.native_status == 0,
            "checked MessageFilter installation failed (result {}, native NTSTATUS 0x{:08X}); restart the application",
            self.result,
            self.native_status
        );
        Ok(())
    }
}

/// One reply slot per generation, provisioned by BeginV2. Setup frames contain
/// no handles, so duplicate request handling can never close transferred handles twice.
pub(crate) struct SetupChannel {
    mapping: Mapping,
    event: HANDLE,
    identity: crate::targets::ProcessIdentity,
    pub(crate) remote_mapping: u64,
    pub(crate) remote_event: u64,
    transferred: bool,
    generation: u64,
    session: u64,
    request: u64,
    failed: bool,
    abandoned: bool,
}

impl SetupChannel {
    pub(crate) fn create(
        identity: &crate::targets::ProcessIdentity,
        generation: u64,
        session: u64,
    ) -> Result<Self> {
        ensure!(
            generation != 0 && session != 0,
            "zero setup channel identity"
        );
        let mapping = Mapping::create_unique(
            &format!(
                "Local\\ProtoInputMultiPlex.SetupReply.{}.{}",
                std::process::id(),
                nonce()
            ),
            64,
        )?;
        let event = unsafe { CreateEventW(ptr::null(), 0, 0, ptr::null()) };
        ensure!(!event.is_null(), "create setup acknowledgement event");
        let retained = match identity.try_clone() {
            Ok(identity) => identity,
            Err(error) => {
                unsafe {
                    CloseHandle(event);
                }
                return Err(error);
            }
        };
        let mut channel = Self {
            mapping,
            event,
            identity: retained,
            remote_mapping: 0,
            remote_event: 0,
            transferred: false,
            generation,
            session,
            request: 0,
            failed: false,
            abandoned: false,
        };
        unsafe {
            ptr::write(
                channel.mapping.view.cast(),
                SetupReply {
                    magic: SETUP_ACK_MAGIC,
                    abi: 1,
                    bytes: 64,
                    creation: identity.creation,
                    generation,
                    session,
                    ..SetupReply::default()
                },
            );
        }
        channel.remote_mapping = duplicate(channel.mapping.handle, channel.identity.handle())?;
        channel.remote_event = duplicate(event, channel.identity.handle())?;
        Ok(channel)
    }

    pub(crate) fn transferred(&mut self, transferred: bool) {
        self.transferred = transferred;
    }

    pub(crate) fn failed(&self) -> bool {
        self.failed
    }

    pub(crate) fn allows_normal_commands(&self) -> bool {
        !self.failed && !self.abandoned
    }

    /// A submitted request or provisioned channel alone does not establish detours.
    pub(crate) fn confirmed(&self) -> bool {
        self.request != 0 && !self.failed && !self.abandoned
    }

    pub(crate) fn pending_command_matches(
        &self,
        command: &crate::protoinput::wire::Command,
    ) -> bool {
        self.failed
            && !self.abandoned
            && matches!(command,
            crate::protoinput::wire::Command::InstallMessageFilter { creation, generation, session, request }
                if *creation == self.identity.creation && *generation == self.generation
                    && *session == self.session && *request == self.request)
    }

    /// Only the pipe's proven NotStarted outcome permits normal Stop cleanup.
    /// The request number stays consumed and another setup attempt is forbidden.
    pub(crate) fn abandon_unsubmitted(&mut self) {
        self.failed = false;
        self.abandoned = true;
    }

    pub(crate) fn issue(&mut self) -> Result<crate::protoinput::wire::Command> {
        ensure!(
            !self.failed && !self.abandoned,
            "checked setup channel remains unconfirmed; restart the application"
        );
        self.identity.ensure_alive()?;
        ensure!(
            unsafe { ResetEvent(self.event) } != 0,
            "reset setup acknowledgement event"
        );
        self.request = self
            .request
            .checked_add(1)
            .context("setup request IDs exhausted")?;
        // Latch before any write. Only a correlated Applied reply clears this.
        self.failed = true;
        Ok(crate::protoinput::wire::Command::InstallMessageFilter {
            creation: self.identity.creation,
            generation: self.generation,
            session: self.session,
            request: self.request,
        })
    }

    fn snapshot(&self) -> Result<SetupReply> {
        let words = self.mapping.view.cast::<AtomicU32>();
        let first = unsafe { (*words.add(3)).load(Ordering::Acquire) };
        ensure!(
            first != 0 && first & 1 == 0,
            "setup acknowledgement publication is incomplete"
        );
        let mut data = [0u32; 16];
        for (index, word) in data.iter_mut().enumerate() {
            *word = unsafe { (*words.add(index)).load(Ordering::Relaxed) };
        }
        std::sync::atomic::fence(Ordering::Acquire);
        let last = unsafe { (*words.add(3)).load(Ordering::Acquire) };
        ensure!(
            first == last && data[3] == first,
            "setup acknowledgement publication changed"
        );
        let wide = |index| u64::from(data[index]) | (u64::from(data[index + 1]) << 32);
        Ok(SetupReply {
            magic: data[0],
            abi: data[1],
            bytes: data[2],
            sequence: data[3],
            creation: wide(4),
            generation: wide(6),
            session: wide(8),
            request: wide(10),
            result: data[12],
            native_status: data[13],
            hook: data[14],
            reserved: data[15],
        })
    }

    pub(crate) fn wait(&mut self, deadline: Instant, cancel: &AtomicBool) -> Result<()> {
        loop {
            ensure!(
                !cancel.load(Ordering::Acquire),
                "checked setup acknowledgement cancelled after submission; restart the application"
            );
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(
                !remaining.is_zero(),
                "checked setup acknowledgement timed out; restart the application"
            );
            let handles = [self.event, self.identity.handle()];
            let millis = remaining.min(Duration::from_millis(10)).as_millis().max(1) as u32;
            match unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, millis) } {
                WAIT_OBJECT_0 => {
                    self.snapshot()?.verify(
                        self.identity.creation,
                        self.generation,
                        self.session,
                        self.request,
                    )?;
                    self.failed = false;
                    return Ok(());
                }
                value if value == WAIT_OBJECT_0 + 1 => {
                    anyhow::bail!("original target exited before setup acknowledgement")
                }
                WAIT_TIMEOUT => {}
                _ => anyhow::bail!(
                    "wait for setup acknowledgement failed: {}",
                    std::io::Error::last_os_error()
                ),
            }
        }
    }
}

impl Drop for SetupChannel {
    fn drop(&mut self) {
        // Once Begin writing starts, these belong to the target even after a
        // partial write, lost reply or host cancellation. Never cross-close them.
        if !self.transferred {
            if self.remote_mapping != 0 {
                reclaim(self.remote_mapping, self.identity.handle());
            }
            if self.remote_event != 0 {
                reclaim(self.remote_event, self.identity.handle());
            }
        }
        unsafe {
            CloseHandle(self.event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn own_ready_record() -> Readiness {
        Readiness {
            magic: READY_MAGIC,
            abi: 1,
            bytes: 128,
            sequence: 2,
            state: 2,
            owner_pid: 0x1234_abcd,
            bits: 64,
            creation: 7,
            generation: 0x8877_6655,
            session: 0x1122_3344_5566_7788,
            build: BUILD,
            ..Readiness::default()
        }
    }

    #[test]
    fn own_ready_rejects_scope_header_build_and_unknown_state_before_publication() {
        let valid = own_ready_record();
        let cases = [
            (
                Readiness { magic: 0, ..valid },
                OwnReadinessStatus::InvalidHeader,
            ),
            (
                Readiness { abi: 2, ..valid },
                OwnReadinessStatus::InvalidHeader,
            ),
            (
                Readiness { bytes: 64, ..valid },
                OwnReadinessStatus::InvalidHeader,
            ),
            (
                Readiness {
                    creation: 8,
                    ..valid
                },
                OwnReadinessStatus::InvalidIdentity,
            ),
            (
                Readiness { bits: 32, ..valid },
                OwnReadinessStatus::InvalidIdentity,
            ),
            (
                Readiness { build: 0, ..valid },
                OwnReadinessStatus::InvalidBuild,
            ),
            (
                Readiness {
                    state: u32::MAX,
                    ..valid
                },
                OwnReadinessStatus::InvalidState,
            ),
        ];
        for (record, status) in cases {
            let observed = own_readiness_sample(record, 2, 7, 64);
            assert_eq!(observed.status, status);
            assert_eq!(observed.present, Some(true));
            assert_eq!(observed.coherent, Some(true));
            assert_eq!(observed.state, None);
            assert_eq!(observed.error, None);
            assert_eq!(observed.binding_present, None);
        }
    }

    #[test]
    fn own_ready_unstable_sequence_never_publishes_payload_facts() {
        for (first, last) in [(2, 4), (3, 3)] {
            let observed = own_readiness_sample(
                Readiness {
                    sequence: first,
                    ..own_ready_record()
                },
                last,
                7,
                64,
            );
            assert_eq!(observed.status, OwnReadinessStatus::Unstable);
            assert_eq!(observed.coherent, Some(false));
            assert_eq!(observed.header_matches, None);
            assert_eq!(observed.identity_matches, None);
            assert_eq!(observed.build_matches, None);
            assert_eq!(observed.state, None);
            assert_eq!(observed.error, None);
        }
    }

    #[test]
    fn own_ready_observation_serializes_only_fixed_facts_and_sanitized_error() {
        let observed = own_readiness_sample(
            Readiness {
                state: 4,
                error: u32::MAX,
                ..own_ready_record()
            },
            2,
            7,
            64,
        );
        let value = serde_json::to_value(observed).unwrap();
        assert_eq!(value["state"], "poisoned");
        assert_eq!(value["error"], "unknown");
        assert_eq!(value["binding_present"], true);
        let mut keys = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "binding_present",
                "build_matches",
                "coherent",
                "error",
                "header_matches",
                "identity_matches",
                "present",
                "state",
                "status"
            ]
        );
        let broken_pipe = own_readiness_sample(
            Readiness {
                state: 4,
                error: 109,
                ..own_ready_record()
            },
            2,
            7,
            64,
        );
        assert_eq!(broken_pipe.error, Some(NativeReadinessError::BrokenPipe));
    }

    mod native_open_contract {
        use super::*;
        use std::{
            os::windows::{io::AsRawHandle, process::CommandExt},
            process::{Child, Command, Stdio},
            sync::{Mutex, atomic::AtomicUsize},
        };

        static FIXTURE: Mutex<()> = Mutex::new(());
        static OPEN_CALLS: AtomicU32 = AtomicU32::new(0);
        static BEGIN_CALLS: AtomicU32 = AtomicU32::new(0);
        static CLOSE_CALLS: AtomicU32 = AtomicU32::new(0);
        static CLOSED_INSTANCE: AtomicU32 = AtomicU32::new(0);
        static CLOSE_OK: AtomicBool = AtomicBool::new(true);
        static EXIT_ON_OPEN: AtomicUsize = AtomicUsize::new(0);
        const INSTANCE: u32 = 0xcafe;

        unsafe extern "C" fn open(_: u64, _: u32, _: u64, _: *const u16, _: bool, _: u32) -> u32 {
            OPEN_CALLS.fetch_add(1, Ordering::SeqCst);
            let owned = EXIT_ON_OPEN.swap(0, Ordering::SeqCst);
            if owned != 0 {
                // Only a retained Child handle from owned_child() is installed here.
                unsafe {
                    windows_sys::Win32::System::Threading::TerminateProcess(owned as HANDLE, 0xf1);
                    WaitForSingleObject(owned as HANDLE, 1000);
                }
            }
            INSTANCE
        }
        unsafe extern "C" fn begin(_: u32, _: *const Begin) -> bool {
            BEGIN_CALLS.fetch_add(1, Ordering::SeqCst);
            true
        }
        unsafe extern "C" fn retire(_: u32, _: *const Retire) -> bool {
            false
        }
        unsafe extern "C" fn close(instance: u32) -> bool {
            CLOSE_CALLS.fetch_add(1, Ordering::SeqCst);
            CLOSED_INSTANCE.store(instance, Ordering::SeqCst);
            CLOSE_OK.load(Ordering::SeqCst)
        }
        fn api() -> Api {
            OPEN_CALLS.store(0, Ordering::SeqCst);
            BEGIN_CALLS.store(0, Ordering::SeqCst);
            CLOSE_CALLS.store(0, Ordering::SeqCst);
            CLOSED_INSTANCE.store(0, Ordering::SeqCst);
            CLOSE_OK.store(true, Ordering::SeqCst);
            EXIT_ON_OPEN.store(0, Ordering::SeqCst);
            Api::Native(NativeLifecycleApi {
                open,
                begin,
                retire,
                close,
            })
        }
        fn ready(identity: &crate::targets::ProcessIdentity) -> Readiness {
            Readiness {
                magic: READY_MAGIC,
                abi: 1,
                bytes: 128,
                sequence: 2,
                state: 1,
                bits: identity.bits,
                creation: identity.creation,
                build: BUILD,
                ..Readiness::default()
            }
        }
        fn mapping(identity: &crate::targets::ProcessIdentity, ready: Readiness) -> Mapping {
            let mapping = Mapping::create_unique(
                &format!(
                    "Local\\ProtoInputMultiPlex.Lifecycle.{}.{}",
                    identity.pid, identity.creation
                ),
                128,
            )
            .unwrap();
            unsafe {
                ptr::write(mapping.view.cast(), ready);
            }
            mapping
        }
        fn begin_after_open(api: &Api, identity: &crate::targets::ProcessIdentity) -> Result<u32> {
            let instance = api.open_connection(identity, &[0], true)?;
            // This represents the guarded next stage of attach. The fake native
            // callback does not interpret or transfer these sentinel handles.
            api.begin(
                instance,
                &Begin {
                    creation: identity.creation,
                    generation: 1,
                    session: 2,
                    commit: 3,
                    reply: 4,
                    event: 5,
                },
            )?;
            Ok(instance)
        }

        #[test]
        fn paired_older_native_build_is_closed_before_begin() {
            let _fixture = FIXTURE.lock().unwrap();
            let identity = crate::targets::ProcessIdentity::open(std::process::id()).unwrap();
            let expected = ready(&identity);
            let mapping = mapping(&identity, expected);
            let cases = [
                Readiness {
                    build: 0x2026_1020_0000_0001,
                    ..expected
                },
                Readiness {
                    creation: identity.creation + 1,
                    ..expected
                },
                Readiness {
                    bits: if identity.bits == 64 { 32 } else { 64 },
                    ..expected
                },
                Readiness {
                    sequence: 3,
                    ..expected
                },
                Readiness {
                    state: 3,
                    ..expected
                },
            ];
            for invalid in cases {
                let api = api();
                unsafe {
                    ptr::write(mapping.view.cast(), invalid);
                }
                assert!(begin_after_open(&api, &identity).is_err());
                assert_eq!(OPEN_CALLS.load(Ordering::SeqCst), 1);
                assert_eq!(CLOSE_CALLS.load(Ordering::SeqCst), 1);
                assert_eq!(CLOSED_INSTANCE.load(Ordering::SeqCst), INSTANCE);
                assert_eq!(BEGIN_CALLS.load(Ordering::SeqCst), 0);
            }
            // The current B native reference still reaches Begin normally.
            let api = api();
            unsafe {
                ptr::write(mapping.view.cast(), expected);
            }
            assert_eq!(begin_after_open(&api, &identity).unwrap(), INSTANCE);
            assert_eq!(BEGIN_CALLS.load(Ordering::SeqCst), 1);
            assert_eq!(CLOSE_CALLS.load(Ordering::SeqCst), 0);
            api.close(INSTANCE).unwrap();
        }

        #[test]
        fn absent_readiness_and_failed_close_remain_errors_before_begin() {
            let _fixture = FIXTURE.lock().unwrap();
            let identity = crate::targets::ProcessIdentity::open(std::process::id()).unwrap();
            let api = api();
            CLOSE_OK.store(false, Ordering::SeqCst);
            let detail = format!("{:#}", begin_after_open(&api, &identity).unwrap_err());
            assert!(detail.contains("incompatible resident"));
            assert!(detail.contains("Close failed"));
            assert_eq!(CLOSE_CALLS.load(Ordering::SeqCst), 1);
            assert_eq!(BEGIN_CALLS.load(Ordering::SeqCst), 0);
        }

        struct OwnedChild(Child);
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        fn owned_child() -> OwnedChild {
            OwnedChild(
                Command::new(
                    PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/cmd.exe"),
                )
                .args(["/d", "/c", "set /p MUX_NATIVE_OPEN_EXIT="])
                .creation_flags(0x0800_0000)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
            )
        }

        #[test]
        fn rejected_fresh_native_open_preserves_dirty_original_lease() {
            let _fixture = FIXTURE.lock().unwrap();
            let mut child = owned_child();
            let identity = crate::targets::ProcessIdentity::open_for_attach(child.0.id()).unwrap();
            let _mapping = mapping(
                &identity,
                Readiness {
                    build: 0x2026_1020_0000_0001,
                    ..ready(&identity)
                },
            );
            let lease = TargetLease::acquire(&identity).unwrap();
            let commit = CommitOwner::create().unwrap();
            let api = api();
            assert!(begin_after_open(&api, &identity).is_err());
            drop(lease);
            assert_eq!(lease_state(identity.pid, identity.creation), Some(1));
            assert_eq!(commit.mapping.state().load(Ordering::Acquire), 0);
            assert_eq!(BEGIN_CALLS.load(Ordering::SeqCst), 0);
            assert_eq!(CLOSE_CALLS.load(Ordering::SeqCst), 1);
            assert!(child.0.try_wait().unwrap().is_none());
        }

        #[test]
        fn original_exit_during_native_open_closes_only_that_instance() {
            let _fixture = FIXTURE.lock().unwrap();
            let mut child = owned_child();
            let identity = crate::targets::ProcessIdentity::open_for_attach(child.0.id()).unwrap();
            let _mapping = mapping(&identity, ready(&identity));
            let api = api();
            EXIT_ON_OPEN.store(child.0.as_raw_handle() as usize, Ordering::SeqCst);
            let error = begin_after_open(&api, &identity).unwrap_err();
            assert!(error.to_string().contains("exited"));
            assert_eq!(child.0.wait().unwrap().code(), Some(0xf1));
            assert_eq!(CLOSE_CALLS.load(Ordering::SeqCst), 1);
            assert_eq!(CLOSED_INSTANCE.load(Ordering::SeqCst), INSTANCE);
            assert_eq!(BEGIN_CALLS.load(Ordering::SeqCst), 0);
        }
    }
    fn setup_reply(channel: &SetupChannel, request: u64, result: u32, status: u32) {
        let reply = SetupReply {
            magic: SETUP_ACK_MAGIC,
            abi: 1,
            bytes: 64,
            sequence: 2,
            creation: channel.identity.creation,
            generation: channel.generation,
            session: channel.session,
            request,
            result,
            native_status: status,
            hook: 2,
            reserved: 0,
        };
        // Native fixture verifies these bytes on both architectures. The event
        // is signalled only after publication, as in the resident adapter.
        unsafe {
            ptr::write(channel.mapping.view.cast(), reply);
        }
        assert_ne!(
            unsafe { windows_sys::Win32::System::Threading::SetEvent(channel.event) },
            0
        );
    }

    #[test]
    fn setup_acknowledgement_rejects_stale_reply_and_preserves_failure() {
        let identity =
            crate::targets::ProcessIdentity::open_for_attach(std::process::id()).unwrap();
        let mut channel = SetupChannel::create(&identity, 8, 9).unwrap();
        assert!(
            !channel.confirmed(),
            "provisioning alone cannot certify MessageFilter detours"
        );
        channel.issue().unwrap();
        assert!(
            !channel.confirmed(),
            "submission alone cannot certify MessageFilter detours"
        );
        setup_reply(&channel, 1, 1, 0);
        channel
            .wait(
                Instant::now() + Duration::from_secs(1),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert!(!channel.failed());
        assert!(channel.confirmed());
        channel.issue().unwrap();
        assert!(!channel.confirmed());
        setup_reply(&channel, 1, 1, 0);
        assert!(
            channel
                .wait(
                    Instant::now() + Duration::from_secs(1),
                    &AtomicBool::new(false)
                )
                .is_err()
        );
        assert!(channel.failed());
        assert!(!channel.confirmed());
        assert!(
            channel.issue().is_err(),
            "failed setup must not issue an implicit retry"
        );
    }

    #[test]
    fn already_applied_setup_reply_can_confirm_but_abandonment_cannot() {
        let identity =
            crate::targets::ProcessIdentity::open_for_attach(std::process::id()).unwrap();
        let mut channel = SetupChannel::create(&identity, 8, 9).unwrap();
        channel.issue().unwrap();
        setup_reply(&channel, 1, 2, 0);
        channel
            .wait(
                Instant::now() + Duration::from_secs(1),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert!(channel.confirmed());
        channel.issue().unwrap();
        channel.abandon_unsubmitted();
        assert!(!channel.confirmed());
    }

    #[test]
    fn setup_native_failure_is_not_a_successful_write_receipt() {
        let identity =
            crate::targets::ProcessIdentity::open_for_attach(std::process::id()).unwrap();
        let mut channel = SetupChannel::create(&identity, 8, 9).unwrap();
        channel.issue().unwrap();
        setup_reply(&channel, 1, 6, 0xc000_0022);
        let error = channel
            .wait(
                Instant::now() + Duration::from_secs(1),
                &AtomicBool::new(false),
            )
            .unwrap_err();
        assert!(error.to_string().contains("C0000022"));
        assert!(channel.failed());
    }

    #[test]
    fn setup_cancel_or_lost_reply_cannot_reclaim_transferred_handles() {
        use windows_sys::Win32::Foundation::GetHandleInformation;
        let identity =
            crate::targets::ProcessIdentity::open_for_attach(std::process::id()).unwrap();
        for cancel in [true, false] {
            let mut channel = SetupChannel::create(&identity, 8, 9).unwrap();
            let remote = [channel.remote_mapping, channel.remote_event];
            channel.transferred(true);
            channel.issue().unwrap();
            let error = channel
                .wait(
                    Instant::now() + Duration::from_millis(10),
                    &AtomicBool::new(cancel),
                )
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(if cancel { "cancelled" } else { "timed out" })
            );
            assert!(channel.failed());
            drop(channel);
            for handle in remote {
                let handle = handle as usize as HANDLE;
                let mut flags = 0;
                assert_ne!(unsafe { GetHandleInformation(handle, &mut flags) }, 0);
                unsafe {
                    CloseHandle(handle);
                }
            }
        }
    }

    #[test]
    fn unstarted_setup_channel_reclaims_only_its_remote_duplicates() {
        use windows_sys::Win32::Foundation::GetHandleInformation;
        let identity =
            crate::targets::ProcessIdentity::open_for_attach(std::process::id()).unwrap();
        let channel = SetupChannel::create(&identity, 8, 9).unwrap();
        let handles = [channel.remote_mapping, channel.remote_event];
        drop(channel);
        for handle in handles {
            let mut flags = 0;
            assert_eq!(
                unsafe { GetHandleInformation(handle as usize as HANDLE, &mut flags) },
                0
            );
        }
    }

    #[test]
    fn original_target_exit_cannot_acknowledge_setup() {
        use std::{
            os::windows::process::CommandExt,
            process::{Command, Stdio},
        };
        let mut child = Command::new(
            PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/cmd.exe"),
        )
        .args(["/d", "/c", "set /p MUX_SETUP_ACK_EXIT="])
        .creation_flags(0x0800_0000)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
        let identity = crate::targets::ProcessIdentity::open_for_attach(child.id()).unwrap();
        let mut channel = SetupChannel::create(&identity, 8, 9).unwrap();
        channel.transferred(true);
        channel.issue().unwrap();
        drop(child.stdin.take());
        child.wait().unwrap();
        let error = channel
            .wait(
                Instant::now() + Duration::from_secs(1),
                &AtomicBool::new(false),
            )
            .unwrap_err();
        assert!(error.to_string().contains("original target exited"));
        assert!(channel.failed());
    }

    #[test]
    fn native_setup_reply_fixture_has_the_exact_rust_layout() {
        let golden = include_str!("../../../scripts/native/setup-ack-golden.txt")
            .lines()
            .nth(2)
            .unwrap()
            .strip_prefix("reply=")
            .unwrap();
        let bytes = (0..golden.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&golden[index..index + 2], 16).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(bytes.len(), size_of::<SetupReply>());
        let reply = unsafe { ptr::read_unaligned(bytes.as_ptr().cast::<SetupReply>()) };
        reply.verify(7, 8, 9, 1).unwrap();
        assert!(reply.verify(7, 8, 9, 2).is_err());
        assert!(reply.verify(7, 8, 10, 1).is_err());
        assert!(
            SetupReply {
                sequence: 3,
                ..reply
            }
            .verify(7, 8, 9, 1)
            .is_err()
        );
    }
    #[test]
    fn abi_sizes_match_native_contract() {
        assert_eq!(size_of::<Record>(), 64);
        assert_eq!(size_of::<Readiness>(), 128);
        assert_eq!(size_of::<Reply>(), 64);
        assert_eq!(size_of::<Begin>(), 48);
        assert_eq!(size_of::<Retire>(), 32);
    }
    #[test]
    fn acknowledgement_rejects_stale_generation_and_session() {
        let reply = Reply {
            magic: REPLY_MAGIC,
            abi: 1,
            bytes: 64,
            result: 1,
            creation: 7,
            generation: 8,
            session: 9,
            native_state: 2,
            ..Reply::default()
        };
        assert!(reply_valid(&reply, 7, 8, 9, 2));
        assert!(!reply_valid(&reply, 7, 10, 9, 2));
        assert!(!reply_valid(&reply, 7, 8, 10, 2));
        assert!(!reply_valid(&reply, 10, 8, 9, 2));
        assert!(!reply_valid(&reply, 7, 8, 9, 3));
    }
    #[test]
    fn commit_is_explicit_and_exactly_once() {
        let owner = CommitOwner::create().unwrap();
        assert_eq!(owner.mapping.state().load(Ordering::Acquire), 0);
        owner.commit().unwrap();
        assert_eq!(owner.mapping.state().load(Ordering::Acquire), 1);
        assert!(owner.commit().is_err());
    }
    #[test]
    fn dropping_supervisor_cannot_commit_a_transaction() {
        let owner = CommitOwner::create().unwrap();
        let observer = Mapping::open(owner.name(), false, 64).unwrap();
        drop(owner);
        assert_eq!(observer.state().load(Ordering::Acquire), 0);
    }
    #[test]
    fn commit_rejects_a_changed_session_identity() {
        let owner = CommitOwner::create().unwrap();
        unsafe {
            (*owner.mapping.view.cast::<Record>()).session ^= 1;
        }
        assert!(owner.commit().is_err());
        assert_eq!(owner.mapping.state().load(Ordering::Acquire), 0);
    }
}
