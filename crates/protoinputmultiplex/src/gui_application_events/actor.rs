//! Retained background owner for network, private descriptor and join work.
use crate::application_event_transport::{
    ClientPermit, Config, NetworkEndpoint, Service, ServiceSnapshot,
};
use anyhow::{Context, Result, ensure};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

enum Command {
    Issue(ClientPermit),
    Copy(ClientPermit),
    ClearCopy,
}
#[derive(Clone, Default)]
pub(super) struct Snapshot {
    pub phase: String,
    pub service: Option<ServiceSnapshot>,
    pub service_sampled_at: Option<Instant>,
    pub error: Option<String>,
    pub retired: bool,
    pub issued: Option<u64>,
}
struct Shared {
    state: Mutex<Snapshot>,
    stop: AtomicBool,
    // This slot is consumed only after a local GUI Copy action and never serialized.
    copied: Mutex<Option<(u64, String)>>,
}
pub(super) struct Actor {
    shared: Arc<Shared>,
    commands: SyncSender<Command>,
    worker: Option<JoinHandle<()>>,
}

impl Actor {
    pub fn start(
        endpoint: NetworkEndpoint,
        origin: String,
        root: PathBuf,
        epoch: u64,
        guards: Arc<crate::gui::RuntimeGuards>,
    ) -> Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(Snapshot {
                phase: "starting".into(),
                ..Snapshot::default()
            }),
            stop: AtomicBool::new(false),
            copied: Mutex::new(None),
        });
        let (sender, receiver) = mpsc::sync_channel(8);
        let task = Arc::clone(&shared);
        let worker = thread::Builder::new().name("application-event-lifecycle".into()).spawn(move || {
            let _guards = guards;
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
                let startup = std::fs::canonicalize(root).context("resolve private application data directory")
                    .and_then(|root| Service::start(Config { allowed_origins: vec![origin] }, endpoint).map(|service| (root, service)));
                let (root, mut service) = match startup {
                    Ok(started) => started,
                    Err(_) => {
                        let mut state = task.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        state.phase = "start_failed".into(); state.retired = true; state.error = Some("Event listener could not start. End this cooperative session before trying again.".into());
                        return Ok(());
                    }
                };
                while !task.stop.load(Ordering::Acquire) {
                    let sampled_at = Instant::now();
                    let snapshot = service.snapshot();
                    let active = snapshot.running && !snapshot.stopping;
                    {
                        let mut state = task.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        state.phase = if active { "active" } else { "stopping" }.into();
                        state.service = Some(snapshot);
                        state.service_sampled_at = Some(sampled_at);
                    }
                    if !active { break; }
                    match receiver.recv_timeout(Duration::from_millis(20)) {
                        Ok(Command::Issue(permit)) => {
                            *task.copied.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                            let path = root.join(format!("protoinputmultiplex.application-events.{}.{}.{}.json", std::process::id(), epoch, permit.id()));
                            let result: Result<crate::application_event_transport::IssuedClient> = service.issue_client(&permit, &path);
                            let mut state = task.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            state.issued = result.as_ref().ok().filter(|issued| issued.descriptor_path == path && !issued.expires_in.is_zero()).map(|_| permit.id());
                            state.error = result.err().map(|_| "Event connection issuance failed; approve a fresh receiver and try again.".into());
                        }
                        Ok(Command::Copy(permit)) => {
                            let connection = service.private_connection_for_copy(&permit);
                            *task.copied.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = connection.map(|value| (permit.id(), value));
                        }
                        Ok(Command::ClearCopy) => { *task.copied.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None; }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                service.request_stop();
                *task.copied.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                task.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).phase = "stopping".into();
                // UI keeps ticking or has already called bridge.stop. A transport
                // error is unconfirmed retirement and must keep installation guards.
                service.finish_stop()?;
                let sampled_at = Instant::now();
                let snapshot = service.snapshot();
                let mut state = task.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                state.service = Some(snapshot); state.service_sampled_at = Some(sampled_at); state.phase = "off".into(); state.retired = true; state.issued = None;
                Ok(())
            }));
            match outcome {
                Ok(Ok(())) => {}
                _ => {
                    let mut state = task.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    // No error payload is emitted: backend errors must not expose
                    // descriptors or credential-bearing command/debug values.
                    state.phase = "cleanup_unconfirmed".into(); state.error = Some("Application-event cleanup is unconfirmed; restart is blocked.".into()); state.retired = false;
                    drop(state); task.stop.store(true, Ordering::Release);
                    *task.copied.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                    loop { thread::park(); }
                }
            }
        }).context("start application-event lifecycle owner")?;
        Ok(Self {
            shared,
            commands: sender,
            worker: Some(worker),
        })
    }
    pub fn snapshot(&self) -> Snapshot {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    pub fn retired(&self) -> bool {
        self.snapshot().retired && self.worker.as_ref().is_none_or(JoinHandle::is_finished)
    }
    pub fn active(&self) -> bool {
        !self.shared.stop.load(Ordering::Acquire) && self.snapshot().phase == "active"
    }
    pub fn request_stop(&self) {
        self.shared.stop.store(true, Ordering::Release);
        self.clear_copy();
    }
    pub fn clear_copy(&self) {
        *self
            .shared
            .copied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        let _ = self.commands.try_send(Command::ClearCopy);
    }
    pub fn issue(&self, permit: ClientPermit) -> Result<()> {
        ensure!(self.active(), "event listener is not active");
        self.commands
            .try_send(Command::Issue(permit))
            .map_err(|_| anyhow::anyhow!("event lifecycle queue is busy"))
    }
    pub fn copy(&self, permit: ClientPermit) -> Result<()> {
        ensure!(self.active(), "event listener is not active");
        self.commands
            .try_send(Command::Copy(permit))
            .map_err(|_| anyhow::anyhow!("event lifecycle queue is busy"))
    }
    pub fn take_copy(&self, permit: u64) -> Option<String> {
        if !self.active() {
            return None;
        }
        self.shared
            .copied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .and_then(|(id, value)| (id == permit).then_some(value))
    }
}
impl Drop for Actor {
    fn drop(&mut self) {
        self.request_stop();
    }
}
