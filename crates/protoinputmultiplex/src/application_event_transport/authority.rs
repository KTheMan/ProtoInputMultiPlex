use anyhow::{Result, ensure};
use mux_application_sdk::{ControlGrant, EventReady, Grant, RecipientId};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Notify;

pub(super) const MAX_CLIENTS: usize = 8;
pub(super) const UI_CAPACITY: usize = 32;
pub(super) const MAX_PENDING: usize = 16;
pub(super) const OUT_CAPACITY: usize = MAX_PENDING + 2;
pub(super) const MAX_TTL: Duration = Duration::from_secs(300);
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
pub(super) const HOST_ACTION_TIMEOUT: Duration = Duration::from_secs(55);
pub(super) const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const NETWORK_POLL: Duration = Duration::from_millis(20);

#[derive(Clone)]
pub(crate) struct Config {
    pub allowed_origins: Vec<String>,
}

/// Host-issued setup token. It contains no credential and creates no users/grants.
/// One permit binds one disposable receiver; replace it with a freshly registered
/// receiver after revoke/disconnect rather than reconnecting a retired identity.
#[derive(Clone)]
pub(crate) struct ClientPermit {
    pub(super) shared: Arc<Shared>,
    pub(super) lease: Arc<Lease>,
}
impl ClientPermit {
    pub fn id(&self) -> u64 {
        self.lease.id
    }
    /// Credential-free authority state; a stale actor snapshot cannot revive it.
    pub fn live(&self) -> bool {
        self.lease.live(&self.shared, Instant::now())
    }
    pub fn issued(&self) -> bool {
        self.live()
            && self.lease.issued.load(Ordering::Acquire)
            && !self.lease.issuing.load(Ordering::Acquire)
            && !self.lease.connected.load(Ordering::Acquire)
            && self
                .lease
                .credential
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct IssuedClient {
    pub descriptor_path: PathBuf,
    pub expires_in: Duration,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ServiceSnapshot {
    pub running: bool,
    pub stopping: bool,
    pub retired: bool,
    pub port: u16,
    pub approved_clients: usize,
    pub connected_clients: usize,
    pub cleanup_pending: usize,
    pub uncertain_admissions: u64,
    pub queue_overflows: u64,
}

pub(super) struct Shared {
    pub stop: AtomicBool,
    pub running: AtomicBool,
    pub retired: AtomicBool,
    pub next: AtomicU64,
    pub uncertain: AtomicU64,
    pub overflows: AtomicU64,
    pub leases: Mutex<BTreeMap<u64, Arc<Lease>>>,
    pub changed: Notify,
}
impl Shared {
    pub fn new() -> Self {
        Self {
            stop: AtomicBool::new(false),
            running: AtomicBool::new(false),
            retired: AtomicBool::new(false),
            next: AtomicU64::new(1),
            uncertain: AtomicU64::new(0),
            overflows: AtomicU64::new(0),
            leases: Mutex::new(BTreeMap::new()),
            changed: Notify::new(),
        }
    }
    pub fn leases(&self) -> Vec<Arc<Lease>> {
        self.leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect()
    }
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
        for lease in self.leases() {
            lease.revoke();
        }
        self.changed.notify_waiters();
    }
}

pub(super) struct Lease {
    pub desktop_consent: Option<mux_application_sdk::DesktopConsentV1>,
    pub desktop_version: AtomicU32,
    pub desktop_ready: AtomicBool,
    pub id: u64,
    pub recipient: RecipientId,
    pub origin: String,
    pub epoch: u64,
    pub events: Grant,
    pub controls: ControlGrant,
    pub deadline: Instant,
    pub allowed: AtomicBool,
    pub issued: AtomicBool,
    pub issuing: AtomicBool,
    pub connected: AtomicBool,
    pub network_done: AtomicBool,
    pub cleanup_done: AtomicBool,
    pub descriptor: Mutex<Option<PathBuf>>,
    // Never included in Debug, status, logs, URLs or host/client command payloads.
    pub credential: Mutex<Option<String>>,
    pub changed: Notify,
}
impl Lease {
    pub fn new(id: u64, origin: String, ready: EventReady, deadline: Instant) -> Self {
        Self {
            desktop_consent: ready.desktop,
            desktop_version: AtomicU32::new(0),
            desktop_ready: AtomicBool::new(false),
            id,
            recipient: ready.recipient,
            origin,
            epoch: ready.epoch,
            events: ready.events,
            controls: ready.controls,
            deadline,
            allowed: AtomicBool::new(true),
            issued: AtomicBool::new(false),
            issuing: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            network_done: AtomicBool::new(true),
            cleanup_done: AtomicBool::new(false),
            descriptor: Mutex::new(None),
            credential: Mutex::new(None),
            changed: Notify::new(),
        }
    }
    pub fn live(&self, shared: &Shared, now: Instant) -> bool {
        !shared.stop.load(Ordering::Acquire)
            && self.allowed.load(Ordering::Acquire)
            && now < self.deadline
    }
    pub fn matches(&self, ready: &EventReady) -> bool {
        (!self.desktop_ready.load(Ordering::Acquire)
            || ready.desktop
                == if self.desktop_version.load(Ordering::Acquire) == 1 {
                    self.desktop_consent
                } else {
                    None
                })
            && self.epoch == ready.epoch
            && self.recipient == ready.recipient
            && self.events == ready.events
            && self.controls == ready.controls
    }
    pub fn revoke(&self) {
        self.allowed.store(false, Ordering::Release);
        self.changed.notify_waiters();
    }
}

pub(super) fn validate_origins(origins: &[String]) -> Result<()> {
    ensure!(
        !origins.is_empty(),
        "event transport needs an explicit trusted origin"
    );
    crate::sdk_listener::validate_origins(origins)?;
    for (index, origin) in origins.iter().enumerate() {
        ensure!(
            !origins[..index].contains(origin),
            "duplicate trusted event origin"
        );
    }
    Ok(())
}

pub(super) fn token_equal(presented: &str, expected: &str) -> bool {
    let mut difference = presented.len() ^ expected.len();
    for index in 0..presented.len().max(expected.len()) {
        difference |= usize::from(
            presented.as_bytes().get(index).copied().unwrap_or(0)
                ^ expected.as_bytes().get(index).copied().unwrap_or(0),
        );
    }
    difference == 0
}
