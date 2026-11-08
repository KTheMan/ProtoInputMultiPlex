//! One explicit Quit's profile persistence gate. No I/O or automatic retry.
use crate::gui_profile_file::{Completion, Value};
use mux_core::Profile;
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone)]
struct Snapshot {
    path: PathBuf,
    profile: Arc<Profile>,
}
impl Snapshot {
    fn matches(&self, path: &Path, profile: &Profile) -> bool {
        self.path == path && *self.profile == *profile
    }
}
#[derive(Default)]
enum State {
    #[default]
    Idle,
    AwaitingQuiescence,
    Saving {
        ticket: u64,
        snapshot: Snapshot,
    },
    Saved {
        ticket: Option<u64>,
        snapshot: Option<Snapshot>,
    },
    Failed {
        ticket: Option<u64>,
        path: Option<PathBuf>,
        error: String,
    },
}
#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    AwaitingQuiescence,
    Saving,
    Saved,
    Failed,
}
#[derive(Serialize)]
pub struct Status {
    pub phase: Phase,
    pub ticket: Option<u64>,
    pub path: Option<String>,
    pub error: Option<String>,
}
#[derive(Default)]
pub struct ExitCoordinator {
    state: State,
}
impl ExitCoordinator {
    /// Call only for an explicit Quit. The owner must settle any older file job
    /// before using this to retry a failed Quit.
    pub fn begin(&mut self) {
        if matches!(self.state, State::Idle | State::Failed { .. }) {
            self.state = State::AwaitingQuiescence;
        }
    }
    pub fn cancel(&mut self) {
        self.state = State::Idle;
    }
    /// Quiescence includes every profile read/write, preference transaction,
    /// SDK retirement and routing cleanup. The owner freezes profile mutations.
    pub fn ready_to_submit(&mut self, quiescent: bool, enabled: bool, initialized: bool) -> bool {
        if !matches!(self.state, State::AwaitingQuiescence) || !quiescent {
            return false;
        }
        if !enabled {
            self.state = State::Saved {
                ticket: None,
                snapshot: None,
            };
            return false;
        }
        if !initialized {
            self.submission_failed(
                "Resolve the failed initial profile load before saving on exit.",
            );
            return false;
        }
        true
    }
    /// Record only a successfully admitted ExitSave. Repeated admission cannot
    /// replace the owned ticket. A failed admission must call submission_failed.
    pub fn submitted(&mut self, ticket: u64, path: PathBuf, profile: Arc<Profile>) -> bool {
        if !matches!(self.state, State::AwaitingQuiescence) {
            return false;
        }
        self.state = State::Saving {
            ticket,
            snapshot: Snapshot { path, profile },
        };
        true
    }
    pub fn submission_failed(&mut self, error: impl Into<String>) {
        if matches!(self.state, State::AwaitingQuiescence) {
            self.state = State::Failed {
                ticket: None,
                path: None,
                error: error.into(),
            };
        }
    }
    /// Consume only this Quit's receipt. Borrow before the GUI consumes result.
    /// Unrelated earlier reads/exports cannot satisfy the exit gate.
    pub fn observe(&mut self, completion: &Completion, path: &Path, profile: &Profile) -> bool {
        let State::Saving { ticket, snapshot } = &self.state else {
            return false;
        };
        if completion.ticket != *ticket {
            return false;
        }
        let ticket = *ticket;
        let snapshot = snapshot.clone();
        let error = if !completion.matches_exit_save(ticket, &snapshot.path, &snapshot.profile) {
            Some("Exit profile save receipt did not match its original operation. Quit again to retry.".into())
        } else if !snapshot.matches(path, profile) {
            Some("The current profile or destination changed during exit saving. Quit again to save the current setup.".into())
        } else {
            match &completion.result {
                Ok(Value::Saved { .. }) => None,
                Ok(_) => Some(
                    "Exit profile save returned no write confirmation. Quit again to retry.".into(),
                ),
                Err(error) => Some(format!(
                    "Could not save {} before exit: {error}. Correct the problem and Quit again.",
                    snapshot.path.display()
                )),
            }
        };
        self.state = match error {
            Some(error) => State::Failed {
                ticket: Some(ticket),
                path: Some(snapshot.path),
                error,
            },
            None => State::Saved {
                ticket: Some(ticket),
                snapshot: Some(snapshot),
            },
        };
        true
    }
    pub fn permits_close(&mut self, path: &Path, profile: &Profile) -> bool {
        let State::Saved { ticket, snapshot } = &self.state else {
            return false;
        };
        if let Some(snapshot) = snapshot
            && !snapshot.matches(path, profile)
        {
            self.state = State::Failed {
                    ticket: *ticket,
                    path: Some(snapshot.path.clone()),
                    error: "The current profile or destination changed after exit saving. Quit again to save the current setup.".into(),
                };
            return false;
        }
        true
    }
    pub fn status(&self) -> Status {
        let (phase, ticket, path, error) = match &self.state {
            State::Idle => (Phase::Idle, None, None, None),
            State::AwaitingQuiescence => (Phase::AwaitingQuiescence, None, None, None),
            State::Saving { ticket, snapshot } => {
                (Phase::Saving, Some(*ticket), Some(&snapshot.path), None)
            }
            State::Saved { ticket, snapshot } => (
                Phase::Saved,
                *ticket,
                snapshot.as_ref().map(|value| &value.path),
                None,
            ),
            State::Failed {
                ticket,
                path,
                error,
            } => (Phase::Failed, *ticket, path.as_ref(), Some(error.clone())),
        };
        Status {
            phase,
            ticket,
            path: path.map(|path| path.display().to_string()),
            error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn profile() -> Arc<Profile> {
        Arc::new(Profile::default())
    }
    #[test]
    fn waits_for_all_pending_work_and_never_duplicates_admission() {
        for _pending_read_or_export in ["read", "export"] {
            let mut gate = ExitCoordinator::default();
            gate.begin();
            assert!(!gate.ready_to_submit(false, true, true));
            gate.begin();
            assert_eq!(gate.status().phase, Phase::AwaitingQuiescence);
            assert!(gate.ready_to_submit(true, true, true));
            assert!(gate.submitted(7, "original.json".into(), profile()));
            gate.begin();
            assert!(!gate.ready_to_submit(true, true, true));
            assert!(!gate.submitted(8, "other.json".into(), profile()));
            assert_eq!(gate.status().ticket, Some(7));
            assert!(!gate.permits_close(Path::new("original.json"), &profile()));
        }
    }
    #[test]
    fn disabled_policy_needs_no_initialized_profile_and_failure_requires_explicit_retry() {
        let mut gate = ExitCoordinator::default();
        gate.begin();
        assert!(!gate.ready_to_submit(true, false, false));
        assert!(gate.permits_close(Path::new("any.json"), &profile()));
        gate.cancel();
        gate.begin();
        assert!(!gate.ready_to_submit(true, true, false));
        assert_eq!(gate.status().phase, Phase::Failed);
        assert!(!gate.ready_to_submit(true, true, true));
        gate.begin();
        assert!(gate.ready_to_submit(true, true, true));
        gate.submission_failed("Permission denied");
        assert!(!gate.ready_to_submit(true, true, true));
        assert_eq!(gate.status().error.as_deref(), Some("Permission denied"));
        gate.begin();
        assert!(gate.ready_to_submit(true, true, true));
    }
}
