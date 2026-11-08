//! GUI-owned SDK lifetime. Network and cleanup work stays on the retained actor.
use anyhow::{Context, Result, ensure};
use std::{
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct Snapshot {
    pub phase: &'static str,
    pub endpoint_path: Option<PathBuf>,
    pub url: Option<String>,
    pub error: Option<String>,
    pub retired: bool,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            phase: "off",
            endpoint_path: None,
            url: None,
            error: None,
            retired: true,
        }
    }
}

struct Shared {
    snapshot: Mutex<Snapshot>,
    // Never part of Snapshot, Debug, status, or scripted diagnostics.
    connection: Mutex<Option<String>>,
    stop: Arc<AtomicBool>,
    wake: Condvar,
}

pub(crate) struct Service {
    shared: Arc<Shared>,
    pub broker_generation: u64,
    pub broker_descriptor_path: PathBuf,
}

impl Service {
    pub fn start(
        mut config: crate::sdk_listener::Config,
        broker_generation: u64,
        guards: Arc<crate::gui::RuntimeGuards>,
    ) -> Result<Self> {
        let broker_descriptor_path = config.gui_connection.clone();
        let shared = Arc::new(Shared {
            snapshot: Mutex::new(Snapshot {
                phase: "starting",
                retired: false,
                ..Snapshot::default()
            }),
            connection: Mutex::new(None),
            stop: Arc::new(AtomicBool::new(false)),
            wake: Condvar::new(),
        });
        config.stop = Arc::clone(&shared.stop);
        let worker = Arc::clone(&shared);
        thread::Builder::new()
            .name("gui-sdk-lifecycle".into())
            .spawn(move || {
                let _guards = guards;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(&worker, config);
                }));
                if result.is_err() {
                    let mut state = lock(&worker);
                    state.phase = "failed";
                    state.error =
                        Some("SDK lifecycle worker failed; cleanup is unconfirmed.".into());
                    state.retired = false;
                    worker.stop.store(true, Ordering::Release);
                    drop(state);
                    // Cleanup is unconfirmed: retain guards and the retirement fence.
                    loop {
                        thread::park();
                    }
                }
            })
            .context("start SDK lifecycle worker")?;
        Ok(Self {
            shared,
            broker_generation,
            broker_descriptor_path,
        })
    }

    pub fn request_stop(&self) {
        // This atomic is also the transport's admission fence, including startup.
        self.shared.stop.store(true, Ordering::Release);
        *self
            .shared
            .connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        let mut state = lock(&self.shared);
        if !state.retired && matches!(state.phase, "starting" | "active") {
            state.phase = "stopping";
        }
        self.shared.wake.notify_one();
    }

    pub fn snapshot(&self) -> Snapshot {
        lock(&self.shared).clone()
    }

    /// An explicit GUI Copy action is the only consumer of this private value.
    pub fn connection_for_copy(&self) -> Option<String> {
        if self.shared.stop.load(Ordering::Acquire) {
            return None;
        }
        self.shared
            .connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.request_stop();
    }
}

fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, Snapshot> {
    shared
        .snapshot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn run(shared: &Shared, config: crate::sdk_listener::Config) {
    if shared.stop.load(Ordering::Acquire) {
        *lock(shared) = Snapshot::default();
        return;
    }
    let path = config.endpoint_path.clone();
    let mut service = match crate::sdk_listener::Service::start(config) {
        Ok(service) => service,
        Err(error) => {
            *lock(shared) = Snapshot {
                phase: "failed",
                error: Some(format!("SDK could not start: {error:#}")),
                ..Snapshot::default()
            };
            return;
        }
    };
    let connection = std::fs::read_to_string(&path)
        .context("read private SDK connection file")
        .and_then(|value| private_connection_for_copy(&value));
    {
        let mut state = lock(shared);
        state.endpoint_path = Some(path);
        state.url = Some(format!("ws://127.0.0.1:{}/", service.port()));
        if !shared.stop.load(Ordering::Acquire) {
            state.phase = "active";
            match connection {
                Ok(value) => {
                    *shared
                        .connection
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(value)
                }
                Err(_) => {
                    state.error = Some(
                        "Connection copying is unavailable; use the private connection file."
                            .into(),
                    )
                }
            }
        }
        while !shared.stop.load(Ordering::Acquire) {
            state = shared
                .wake
                .wait_timeout(state, Duration::from_millis(100))
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
            if !service.is_running() {
                shared.stop.store(true, Ordering::Release);
                state.error = Some("SDK listener stopped; finishing connection cleanup.".into());
            }
        }
        state.phase = "stopping";
        *shared
            .connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
    let mut retry = Duration::from_millis(250);
    loop {
        match service.stop() {
            Ok(()) => {
                *lock(shared) = Snapshot::default();
                return;
            }
            Err(error) => {
                let mut state = lock(shared);
                state.phase = "cleanup_pending";
                state.error = Some(format!("SDK cleanup is incomplete: {error:#}"));
            }
        }
        thread::sleep(retry);
        retry = (retry * 2).min(Duration::from_secs(2));
    }
}

fn private_connection_for_copy(value: &str) -> Result<String> {
    ensure!(value.len() <= 4096, "SDK connection file is too large");
    let endpoint: crate::sdk_wire::Endpoint = serde_json::from_str(value)?;
    ensure!(
        endpoint.protoinputmultiplex_sdk == crate::sdk_wire::VERSION,
        "Unsupported SDK connection version"
    );
    ensure!(
        endpoint.port != 0 && crate::sdk_wire::valid_token(&endpoint.token),
        "Invalid SDK connection"
    );
    Ok(serde_json::to_string(&endpoint)?)
}

/// Human entry is bounded before URL validation on the network actor.
pub(crate) fn origin_draft(value: &str) -> Result<Vec<String>> {
    ensure!(value.len() <= 4096, "Browser origin list is too large");
    let values: Vec<_> = value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect();
    ensure!(values.len() <= 16, "Use at most 16 trusted browser origins");
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_value_is_private_endpoint_json_without_diagnostic_fields() {
        let source = serde_json::json!({"protoinputmultiplex_sdk": 1, "port": 32123, "token": "a".repeat(64)});
        let copied: serde_json::Value =
            serde_json::from_str(&private_connection_for_copy(&source.to_string()).unwrap())
                .unwrap();
        assert_eq!(copied, source);
        let diagnostics = serde_json::to_string(&Snapshot::default()).unwrap();
        assert!(!diagnostics.contains("token"));
        assert!(
            private_connection_for_copy(
                r#"{"protoinputmultiplex_sdk":1,"port":0,"token":"secret"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn origin_entry_is_bounded_before_background_validation() {
        assert_eq!(
            origin_draft(" https://example.test, http://localhost:3000 ").unwrap(),
            vec!["https://example.test", "http://localhost:3000"]
        );
        assert!(origin_draft(&"x".repeat(4097)).is_err());
        assert!(origin_draft(&["https://example.test"; 17].join(",")).is_err());
    }

    #[test]
    fn stop_revokes_copy_without_claiming_cleanup_or_overwriting_failure() {
        let service = Service {
            shared: Arc::new(Shared {
                snapshot: Mutex::new(Snapshot {
                    phase: "active",
                    retired: false,
                    ..Snapshot::default()
                }),
                connection: Mutex::new(Some("private credential".into())),
                stop: Arc::new(AtomicBool::new(false)),
                wake: Condvar::new(),
            }),
            broker_generation: 7,
            broker_descriptor_path: PathBuf::from("private-descriptor"),
        };
        service.request_stop();
        assert!(service.shared.stop.load(Ordering::Acquire));
        assert!(service.connection_for_copy().is_none());
        let state = service.snapshot();
        assert_eq!(state.phase, "stopping");
        assert!(!state.retired);
        lock(&service.shared).phase = "cleanup_pending";
        service.request_stop();
        assert_eq!(service.snapshot().phase, "cleanup_pending");
        assert!(!service.snapshot().retired);
    }
}
