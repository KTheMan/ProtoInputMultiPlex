//! One outstanding profile-file operation. Storage never runs under a GUI lock.
use anyhow::{Context, Result, ensure};
use mux_core::Profile;
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    InitialLoad,
    InitialLoadDefault,
    Save,
    ExitSave,
    Load,
    Import,
    Export,
    Restore,
}
impl Kind {
    pub fn mutates(self) -> bool {
        matches!(
            self,
            Self::Save | Self::ExitSave | Self::Export | Self::Restore
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Stamp {
    pub session: u64,
    pub profile: u64,
    pub interaction: u64,
    pub active_path: String,
    pub transfer_path: String,
}

struct Job {
    ticket: u64,
    kind: Kind,
    path: PathBuf,
    profile: Arc<Profile>,
    repaint_context: Option<eframe::egui::Context>,
}
struct Pending {
    ticket: u64,
    kind: Kind,
    path: PathBuf,
    profile: Arc<Profile>,
    stamp: Stamp,
    valid: bool,
}
pub enum Value {
    Saved { profile: Profile, existed: bool },
    Loaded(Profile),
    MissingDefault,
}
pub struct Completion {
    pub ticket: u64,
    pub kind: Kind,
    pub path: PathBuf,
    pub result: Result<Value, String>,
    pending: Pending,
}
impl Completion {
    /// Quit itself changes the interaction epoch. The owner may use this only
    /// while awaiting quiescence, stopped, without local drafts or Close ready.
    /// Every profile/path/session guard remains identical to an ordinary read.
    pub fn matches_quit_read(&self, stamp: &Stamp, profile: &Profile) -> bool {
        matches!(
            self.kind,
            Kind::InitialLoad
                | Kind::InitialLoadDefault
                | Kind::Load
                | Kind::Import
                | Kind::Restore
        ) && self.pending.valid
            && self.ticket == self.pending.ticket
            && self.kind == self.pending.kind
            && self.path == self.pending.path
            && self.pending.stamp.session == stamp.session
            && self.pending.stamp.profile == stamp.profile
            && self.pending.stamp.active_path == stamp.active_path
            && self.pending.stamp.transfer_path == stamp.transfer_path
            && *self.pending.profile == *profile
    }
    /// Bind an exit receipt to the exact admitted write, independently of GUI
    /// interaction stamps. A write invalidated by its owner is never accepted.
    pub fn matches_exit_save(&self, ticket: u64, path: &Path, profile: &Profile) -> bool {
        self.ticket == ticket
            && self.kind == Kind::ExitSave
            && self.path == path
            && self.pending.valid
            && self.pending.ticket == ticket
            && self.pending.kind == Kind::ExitSave
            && self.pending.path == path
            && *self.pending.profile == *profile
    }
    pub fn is_current(&self, stamp: &Stamp, profile: &Profile, stopped: bool) -> bool {
        self.pending.valid
            && stopped
            && self.pending.stamp == *stamp
            && *self.pending.profile == *profile
    }
}
struct Reply {
    ticket: u64,
    result: Result<Value, String>,
}
trait Storage: Send + 'static {
    fn run(&mut self, kind: Kind, path: &Path, profile: &Profile) -> Result<Value>;
}
struct Filesystem;
fn default_load_result(result: Result<Profile>) -> Result<Value> {
    match result {
        Ok(profile) => Ok(Value::Loaded(profile)),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|cause| cause.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(Value::MissingDefault)
        }
        Err(error) => Err(error),
    }
}
impl Storage for Filesystem {
    fn run(&mut self, kind: Kind, path: &Path, profile: &Profile) -> Result<Value> {
        match kind {
            Kind::Save | Kind::ExitSave | Kind::Export => {
                let mut profile = profile.clone();
                profile.normalize_ids()?;
                let existed = path.exists();
                if kind == Kind::ExitSave {
                    crate::profile_store::save_if_changed(path, &profile)?;
                } else {
                    crate::profile_store::save(path, &profile)?;
                }
                Ok(Value::Saved { profile, existed })
            }
            Kind::InitialLoad | Kind::Load | Kind::Import => {
                crate::profile_store::read(path).map(Value::Loaded)
            }
            Kind::InitialLoadDefault => default_load_result(crate::profile_store::read(path)),
            Kind::Restore => crate::profile_store::restore(path).map(Value::Loaded),
        }
    }
}

#[derive(Serialize)]
pub struct Status {
    pub ticket: u64,
    pub kind: Option<Kind>,
    pub path: Option<String>,
    pub pending: bool,
    pub mutating: bool,
    pub valid: bool,
    pub context: Option<Stamp>,
    pub test_delay_ms: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Installed,
    Saved,
    Exported,
    FinishedNotInstalled,
    Failed,
}
#[derive(Clone, Default, Serialize)]
pub struct UiState {
    pub initial_unresolved: bool,
    pub quit_waiting: bool,
    pub result: Option<Outcome>,
}
impl UiState {
    pub fn installed(&mut self, kind: Kind) {
        if matches!(
            kind,
            Kind::InitialLoad | Kind::InitialLoadDefault | Kind::Load
        ) {
            self.initial_unresolved = false;
        }
    }
    pub fn reset(&mut self) {
        self.initial_unresolved = false;
    }
    pub fn request_quit(&mut self, mutating: bool) -> bool {
        self.quit_waiting = mutating;
        mutating
    }
    /// A failed write stays visible instead of silently exiting on a latched Quit.
    pub fn finished(&mut self, outcome: Outcome) -> bool {
        self.result = Some(outcome);
        let quit = self.quit_waiting && outcome != Outcome::Failed;
        self.quit_waiting = false;
        quit
    }
}
#[derive(Serialize)]
pub struct Snapshot {
    pub operation: Status,
    #[serde(flatten)]
    pub ui: UiState,
}
pub struct Service {
    jobs: SyncSender<Job>,
    replies: Receiver<Reply>,
    pending: Option<Pending>,
    next_ticket: u64,
    last_stamp: Option<Stamp>,
    last_kind: Option<Kind>,
    last_path: Option<PathBuf>,
    delay: Duration,
    repaint_context: Option<eframe::egui::Context>,
}
impl Service {
    pub fn start(delay: Duration, guards: Arc<crate::gui::RuntimeGuards>) -> Result<Self> {
        ensure!(
            delay <= Duration::from_secs(15),
            "profile-file delay cannot exceed 15 seconds"
        );
        Self::start_with_guards(Filesystem, delay, Some(guards))
    }
    #[cfg(test)]
    fn start_with(storage: impl Storage, delay: Duration) -> Result<Self> {
        Self::start_with_guards(storage, delay, None)
    }
    fn start_with_guards(
        mut storage: impl Storage,
        delay: Duration,
        guards: Option<Arc<crate::gui::RuntimeGuards>>,
    ) -> Result<Self> {
        let (jobs, incoming) = mpsc::sync_channel::<Job>(1);
        let (outgoing, replies) = mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("gui-profile-file".into())
            .spawn(move || {
                let _guards = guards;
                while let Ok(job) = incoming.recv() {
                    if !delay.is_zero() {
                        std::thread::sleep(delay);
                    }
                    let result = storage
                        .run(job.kind, &job.path, &job.profile)
                        .map_err(|error| format!("{error:#}"));
                    if outgoing
                        .try_send(Reply {
                            ticket: job.ticket,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                    // Publish first: the root can now consume success or error,
                    // including while a minimized window waits to finish Quit.
                    if let Some(ctx) = job.repaint_context {
                        ctx.request_repaint_of(eframe::egui::ViewportId::ROOT);
                    }
                }
                drop(storage);
                drop(_guards);
            })?;
        drop(worker); // No GUI owner joins a blocked filesystem operation.
        Ok(Self {
            jobs,
            replies,
            pending: None,
            next_ticket: 0,
            last_stamp: None,
            last_kind: None,
            last_path: None,
            delay,
            repaint_context: None,
        })
    }
    pub fn attach_repaint_context(&mut self, ctx: &eframe::egui::Context) {
        self.repaint_context = Some(ctx.clone());
    }
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn mutating(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.kind.mutates())
    }
    pub fn invalidate(&mut self) {
        if let Some(pending) = &mut self.pending {
            pending.valid = false;
        }
    }
    pub fn status(&self) -> Status {
        Status {
            ticket: self.next_ticket,
            kind: self.last_kind,
            path: self
                .last_path
                .as_ref()
                .map(|path| path.display().to_string()),
            pending: self.pending(),
            mutating: self.mutating(),
            valid: self.pending.as_ref().is_some_and(|pending| pending.valid),
            context: self.last_stamp.clone(),
            test_delay_ms: self.delay.as_millis() as u64,
        }
    }
    pub fn submit(
        &mut self,
        kind: Kind,
        path: PathBuf,
        profile: Arc<Profile>,
        stamp: Stamp,
    ) -> Result<u64> {
        ensure!(
            !self.pending(),
            "A profile file operation is already pending"
        );
        let ticket = self
            .next_ticket
            .checked_add(1)
            .context("profile file ticket exhausted")?;
        self.jobs
            .try_send(Job {
                ticket,
                kind,
                path: path.clone(),
                profile: Arc::clone(&profile),
                repaint_context: self.repaint_context.clone(),
            })
            .map_err(|_| anyhow::anyhow!("Profile file worker is unavailable or busy"))?;
        self.next_ticket = ticket;
        self.last_stamp = Some(stamp.clone());
        self.last_kind = Some(kind);
        self.last_path = Some(path.clone());
        self.pending = Some(Pending {
            ticket,
            kind,
            path,
            profile,
            stamp,
            valid: true,
        });
        Ok(ticket)
    }
    pub fn poll(&mut self) -> Option<Completion> {
        let reply = match self.replies.try_recv() {
            Ok(reply) => reply,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                let pending = self.pending.take()?;
                return Some(Completion {
                    ticket: pending.ticket,
                    kind: pending.kind,
                    path: pending.path.clone(),
                    result: Err(
                        "Profile file worker stopped; disk completion is unconfirmed".into(),
                    ),
                    pending,
                });
            }
        };
        let pending = self.pending.take()?;
        let result = if reply.ticket == pending.ticket {
            reply.result
        } else {
            Err("Profile file worker returned an unexpected operation; disk completion is unconfirmed".into())
        };
        Some(Completion {
            ticket: pending.ticket,
            kind: pending.kind,
            path: pending.path.clone(),
            result,
            pending,
        })
    }
}

/// A new post-click interaction may change a hidden editor even if Profile is
/// unchanged. Hover, focus notifications and ordinary repaint do not invalidate.
pub fn local_interaction(input: &eframe::egui::RawInput, dragging: bool) -> bool {
    use eframe::egui::Event;
    input.events.iter().any(|event| match event {
        Event::PointerButton { .. }
        | Event::AccessKitActionRequest(_)
        | Event::Ime(_)
        | Event::Touch { .. }
        | Event::Text(_)
        | Event::Paste(_)
        | Event::Cut
        | Event::MouseWheel { .. }
        | Event::Key { pressed: true, .. } => true,
        Event::PointerMoved(_) => dragging,
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    fn profile(name: &str) -> Arc<Profile> {
        Arc::new(serde_json::from_value(serde_json::json!({"version":1,"desktop":{},"seats":[{"id":"a","name":name,"mouse":"virtual://mouse/1"}]})).unwrap())
    }
    fn stamp() -> Stamp {
        Stamp {
            session: 1,
            profile: 2,
            interaction: 3,
            active_path: "active.json".into(),
            transfer_path: "transfer.json".into(),
        }
    }
    fn await_reply(service: &mut Service) -> Completion {
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(reply) = service.poll() {
                return reply;
            }
            assert!(Instant::now() < until);
            std::thread::yield_now();
        }
    }
    struct Block {
        started: mpsc::Sender<(Kind, PathBuf, String)>,
        release: Receiver<()>,
        fail: bool,
    }
    impl Storage for Block {
        fn run(&mut self, kind: Kind, path: &Path, profile: &Profile) -> Result<Value> {
            self.started
                .send((kind, path.to_owned(), profile.seats[0].name.clone()))
                .unwrap();
            self.release.recv().unwrap();
            ensure!(!self.fail, "owned storage failure");
            Ok(if kind.mutates() && kind != Kind::Restore {
                Value::Saved {
                    profile: profile.clone(),
                    existed: true,
                }
            } else {
                Value::Loaded(profile.clone())
            })
        }
    }
    #[test]
    fn blocked_provider_is_nonblocking_and_keeps_exact_clicked_snapshot_without_duplicate() {
        for kind in [
            Kind::InitialLoad,
            Kind::InitialLoadDefault,
            Kind::Save,
            Kind::ExitSave,
            Kind::Load,
            Kind::Import,
            Kind::Export,
            Kind::Restore,
        ] {
            let (started_tx, started_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let mut service = Service::start_with(
                Block {
                    started: started_tx,
                    release: release_rx,
                    fail: false,
                },
                Duration::ZERO,
            )
            .unwrap();
            // Startup preserves the original OS path, including non-Unicode
            // Windows names; display text must never become the worker path.
            use std::os::windows::ffi::OsStringExt;
            let clicked_path = if kind == Kind::InitialLoad {
                PathBuf::from(std::ffi::OsString::from_wide(&[0xD800, 0x2E, 0x6A]))
            } else {
                PathBuf::from("clicked.json")
            };
            let ticket = service
                .submit(kind, clicked_path.clone(), profile("clicked"), stamp())
                .unwrap();
            assert_eq!(
                started_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
                (kind, clicked_path, "clicked".into())
            );
            assert!(service.poll().is_none());
            assert!(
                service
                    .submit(kind, "new.json".into(), profile("new"), stamp())
                    .is_err()
            );
            assert_eq!(service.mutating(), kind.mutates());
            release_tx.send(()).unwrap();
            let done = await_reply(&mut service);
            assert_eq!(done.ticket, ticket);
            assert!(done.is_current(&stamp(), &profile("clicked"), true));
            let mut changed = stamp();
            changed.interaction += 1;
            assert!(!done.is_current(&changed, &profile("clicked"), true));
            changed = stamp();
            changed.active_path = "changed.json".into();
            assert!(!done.is_current(&changed, &profile("clicked"), true));
            changed = stamp();
            changed.session += 1;
            assert!(!done.is_current(&changed, &profile("clicked"), true));
            assert!(!done.is_current(&stamp(), &profile("edited"), true));
            assert!(!done.is_current(&stamp(), &profile("clicked"), false));
            assert!(done.result.is_ok());
        }
    }
    #[test]
    fn blocked_owner_drop_detaches_and_stale_write_completion_is_still_reported() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut service = Service::start_with(
            Block {
                started: started_tx,
                release: release_rx,
                fail: false,
            },
            Duration::ZERO,
        )
        .unwrap();
        service
            .submit(
                Kind::Save,
                "original.json".into(),
                profile("clicked"),
                stamp(),
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        service.invalidate();
        release_tx.send(()).unwrap();
        let done = await_reply(&mut service);
        assert!(!done.is_current(&stamp(), &profile("clicked"), true));
        assert!(matches!(done.result, Ok(Value::Saved { .. })));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut service = Service::start_with(
            Block {
                started: started_tx,
                release: release_rx,
                fail: false,
            },
            Duration::ZERO,
        )
        .unwrap();
        service
            .submit(
                Kind::Load,
                "blocked.json".into(),
                profile("clicked"),
                stamp(),
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let owner = std::thread::spawn(move || {
            drop(service);
            dropped_tx.send(()).unwrap();
        });
        dropped_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("GUI owner joined blocked storage");
        owner.join().unwrap();
        release_tx.send(()).unwrap();
    }
    #[test]
    fn storage_error_clears_pending_and_preserves_monotonic_recovery() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut service = Service::start_with(
            Block {
                started: started_tx,
                release: release_rx,
                fail: true,
            },
            Duration::ZERO,
        )
        .unwrap();
        service
            .submit(Kind::Restore, "bad.json".into(), profile("old"), stamp())
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        release_tx.send(()).unwrap();
        assert!(
            await_reply(&mut service)
                .result
                .err()
                .unwrap()
                .contains("owned storage failure")
        );
        assert!(!service.pending());
        service
            .submit(Kind::Load, "retry.json".into(), profile("old"), stamp())
            .unwrap();
        assert_eq!(service.status().ticket, 2);
        started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        release_tx.send(()).unwrap();
        assert!(await_reply(&mut service).result.is_err());
    }
    #[test]
    fn real_store_actor_reuses_exact_backup_and_restore_contract() {
        let path = std::env::temp_dir().join(format!(
            "protoinputmultiplex-profile-actor-{}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        let target = path.join("profile.json");
        let original = format!(
            " \n{}\n",
            serde_json::to_string(&*profile("original")).unwrap()
        )
        .into_bytes();
        std::fs::write(&target, &original).unwrap();
        let mut service = Service::start_with(Filesystem, Duration::ZERO).unwrap();
        service
            .submit(Kind::Save, target.clone(), profile("replacement"), stamp())
            .unwrap();
        await_reply(&mut service)
            .result
            .unwrap_or_else(|error| panic!("store operation failed: {error}"));
        assert_eq!(
            std::fs::read(crate::profile_store::backup_path(&target)).unwrap(),
            original
        );
        let replacement = std::fs::read(&target).unwrap();
        service
            .submit(Kind::Restore, target.clone(), profile("unused"), stamp())
            .unwrap();
        let done = await_reply(&mut service);
        assert!(
            matches!(done.result, Ok(Value::Loaded(profile)) if profile.seats[0].name == "original")
        );
        assert_eq!(
            std::fs::read(crate::profile_store::backup_path(&target)).unwrap(),
            replacement
        );
        drop(service);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn write_quit_waits_for_actual_provider_completion_and_failure_stays_visible() {
        for fail in [false, true] {
            let (started_tx, started_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let mut service = Service::start_with(
                Block {
                    started: started_tx,
                    release: release_rx,
                    fail,
                },
                Duration::ZERO,
            )
            .unwrap();
            service
                .submit(
                    Kind::Restore,
                    "original.json".into(),
                    profile("clicked"),
                    stamp(),
                )
                .unwrap();
            started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            let mut ui = UiState {
                initial_unresolved: true,
                ..Default::default()
            };
            assert!(ui.request_quit(service.mutating()));
            assert!(service.poll().is_none());
            assert!(ui.quit_waiting);
            release_tx.send(()).unwrap();
            let completion = await_reply(&mut service);
            let outcome = if completion.result.is_ok() {
                Outcome::FinishedNotInstalled
            } else {
                Outcome::Failed
            };
            assert_eq!(ui.finished(outcome), !fail);
            assert!(!ui.quit_waiting);
            assert!(ui.initial_unresolved);
            ui.installed(Kind::Restore);
            assert!(ui.initial_unresolved);
            ui.installed(Kind::Load);
            assert!(!ui.initial_unresolved);
            ui.initial_unresolved = true;
            ui.reset();
            assert!(!ui.initial_unresolved);
        }
    }
    #[test]
    fn provider_panic_reports_unconfirmed_disk_completion_and_unblocks_owner() {
        struct Panic;
        impl Storage for Panic {
            fn run(&mut self, _: Kind, _: &Path, _: &Profile) -> Result<Value> {
                panic!("owned provider panic")
            }
        }
        let mut service = Service::start_with(Panic, Duration::ZERO).unwrap();
        service
            .submit(
                Kind::Save,
                "captured.json".into(),
                profile("clicked"),
                stamp(),
            )
            .unwrap();
        let completion = await_reply(&mut service);
        assert!(completion.result.err().unwrap().contains("unconfirmed"));
        assert!(!service.pending());
        assert!(
            service
                .submit(Kind::Load, "retry.json".into(), profile("clicked"), stamp())
                .is_err()
        );
    }
    #[test]
    fn accessibility_draft_edit_blocks_old_read_even_before_profile_is_committed() {
        use eframe::egui::{Event, RawInput, accesskit};
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut service = Service::start_with(
            Block {
                started: started_tx,
                release: release_rx,
                fail: false,
            },
            Duration::ZERO,
        )
        .unwrap();
        let unchanged_profile = profile("clicked");
        service
            .submit(
                Kind::Load,
                "clicked.json".into(),
                Arc::clone(&unchanged_profile),
                stamp(),
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        // AccessKit can edit an unsubmitted form without pointer/key events or
        // changing the installed Profile. The GUI samples this before polling.
        let input = RawInput {
            events: vec![Event::AccessKitActionRequest(accesskit::ActionRequest {
                action: accesskit::Action::SetValue,
                target_tree: accesskit::TreeId::ROOT,
                target_node: accesskit::NodeId(1),
                data: Some(accesskit::ActionData::Value("new unsubmitted draft".into())),
            })],
            ..Default::default()
        };
        assert!(local_interaction(&input, false));
        let mut current = stamp();
        current.interaction += u64::from(local_interaction(&input, false));
        release_tx.send(()).unwrap();
        let completion = await_reply(&mut service);
        assert!(completion.is_current(&stamp(), &unchanged_profile, true));
        assert!(!completion.is_current(&current, &unchanged_profile, true));
    }
    #[test]
    fn hover_and_background_events_do_not_invalidate_but_post_click_navigation_does() {
        use eframe::egui::{Event, Modifiers, PointerButton, Pos2, RawInput};
        let mut input = RawInput {
            events: vec![
                Event::PointerMoved(Pos2::new(4., 5.)),
                Event::WindowFocused(true),
            ],
            ..Default::default()
        };
        assert!(!local_interaction(&input, false));
        assert!(local_interaction(&input, true));
        input.events = vec![Event::PointerButton {
            pos: Pos2::ZERO,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        }];
        assert!(local_interaction(&input, false));
    }

    #[test]
    fn only_typed_not_found_is_a_missing_default() {
        use std::io::{Error, ErrorKind};
        for kind in [
            ErrorKind::PermissionDenied,
            ErrorKind::InvalidData,
            ErrorKind::Other,
        ] {
            let error = anyhow::Error::new(Error::new(kind, "file not found"))
                .context("open owned default");
            assert!(default_load_result(Err(error)).is_err());
        }
        let error = anyhow::Error::new(Error::new(ErrorKind::NotFound, "owned missing file"))
            .context("open owned default");
        assert!(matches!(
            default_load_result(Err(error)),
            Ok(Value::MissingDefault)
        ));
        assert!(default_load_result(Err(anyhow::anyhow!("NotFound: invalid JSON"))).is_err());
        assert!(default_load_result(crate::profile_store::decode(b"{ invalid JSON")).is_err());
        let mut ui = UiState {
            initial_unresolved: true,
            ..Default::default()
        };
        ui.installed(Kind::InitialLoadDefault);
        assert!(!ui.initial_unresolved);
    }

    #[test]
    fn exit_receipt_binds_ticket_kind_path_snapshot_and_validity_without_interaction_stamp() {
        use crate::gui_profile_policy::{ExitCoordinator, Phase};
        for scenario in [
            "success",
            "path",
            "profile",
            "invalid",
            "failure",
            "kind",
            "receipt_path",
            "snapshot",
        ] {
            let (started_tx, started_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let mut service = Service::start_with(
                Block {
                    started: started_tx,
                    release: release_rx,
                    fail: scenario == "failure",
                },
                Duration::ZERO,
            )
            .unwrap();
            let original = profile("original");
            let path = PathBuf::from("owned.json");
            let mut gate = ExitCoordinator::default();
            gate.begin();
            assert!(gate.ready_to_submit(true, true, true));
            let ticket = service
                .submit(Kind::ExitSave, path.clone(), original.clone(), stamp())
                .unwrap();
            assert!(gate.submitted(ticket, path.clone(), original.clone()));
            started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            if scenario == "invalid" {
                service.invalidate();
            }
            release_tx.send(()).unwrap();
            let mut completion = await_reply(&mut service);
            if scenario == "kind" {
                completion.kind = Kind::Save;
            }
            if scenario == "receipt_path" {
                completion.path = "different.json".into();
            }
            if scenario == "snapshot" {
                completion.pending.profile = profile("different");
            }
            let mut changed_stamp = stamp();
            changed_stamp.interaction += 1;
            assert!(!completion.is_current(&changed_stamp, &original, false));
            // An unrelated receipt cannot complete this Quit.
            completion.ticket += 1;
            assert!(!gate.observe(&completion, &path, &original));
            assert_eq!(gate.status().phase, Phase::Saving);
            completion.ticket = ticket;
            let current_path = if scenario == "path" {
                Path::new("new.json")
            } else {
                &path
            };
            let current_profile = if scenario == "profile" {
                profile("edited")
            } else {
                original.clone()
            };
            assert!(gate.observe(&completion, current_path, &current_profile));
            assert_eq!(
                gate.permits_close(current_path, &current_profile),
                scenario == "success"
            );
            if scenario == "success" {
                // A later edit is also detected immediately before Close.
                assert!(!gate.permits_close(&path, &profile("late edit")));
            }
            assert_eq!(gate.status().phase, Phase::Failed);
            assert!(!gate.ready_to_submit(true, true, true));
            gate.begin();
            assert!(gate.ready_to_submit(true, true, true));
        }
    }

    #[test]
    fn completion_wakes_root_after_success_and_storage_error() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for fail in [false, true] {
            let (started_tx, started_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let mut service = Service::start_with(
                Block {
                    started: started_tx,
                    release: release_rx,
                    fail,
                },
                Duration::ZERO,
            )
            .unwrap();
            let context = eframe::egui::Context::default();
            let wakes = Arc::new(AtomicUsize::new(0));
            let observed = wakes.clone();
            context.set_request_repaint_callback(move |info| {
                assert_eq!(info.viewport_id, eframe::egui::ViewportId::ROOT);
                observed.fetch_add(1, Ordering::SeqCst);
            });
            service.attach_repaint_context(&context);
            service
                .submit(
                    Kind::ExitSave,
                    "owned.json".into(),
                    profile("owned"),
                    stamp(),
                )
                .unwrap();
            started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            release_tx.send(()).unwrap();
            let completion = await_reply(&mut service);
            assert_eq!(completion.result.is_err(), fail);
            let until = Instant::now() + Duration::from_secs(3);
            while wakes.load(Ordering::SeqCst) == 0 {
                assert!(Instant::now() < until);
                std::thread::yield_now();
            }
        }
    }

    #[test]
    fn pending_read_or_export_must_settle_before_one_exit_save() {
        use crate::gui_profile_policy::{ExitCoordinator, Phase};
        for earlier_kind in [Kind::Load, Kind::Export] {
            let (started_tx, started_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let mut service = Service::start_with(
                Block {
                    started: started_tx,
                    release: release_rx,
                    fail: false,
                },
                Duration::ZERO,
            )
            .unwrap();
            let original = profile("original");
            let target = PathBuf::from("active.json");
            service
                .submit(
                    earlier_kind,
                    "earlier.json".into(),
                    original.clone(),
                    stamp(),
                )
                .unwrap();
            started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            let mut gate = ExitCoordinator::default();
            gate.begin();
            assert!(!gate.ready_to_submit(!service.pending(), true, true));
            release_tx.send(()).unwrap();
            let earlier = await_reply(&mut service);
            assert!(!gate.observe(&earlier, &target, &original));
            assert!(!gate.permits_close(&target, &original));
            assert!(gate.ready_to_submit(!service.pending(), true, true));
            let ticket = service
                .submit(Kind::ExitSave, target.clone(), original.clone(), stamp())
                .unwrap();
            gate.submitted(ticket, target.clone(), original.clone());
            assert_eq!(
                started_rx.recv_timeout(Duration::from_secs(3)).unwrap().0,
                Kind::ExitSave
            );
            gate.begin();
            assert!(!gate.ready_to_submit(!service.pending(), true, true));
            release_tx.send(()).unwrap();
            let completion = await_reply(&mut service);
            assert!(gate.observe(&completion, &target, &original));
            assert_eq!(gate.status().phase, Phase::Saved);
            assert!(gate.permits_close(&target, &original));
            assert!(started_rx.try_recv().is_err());
        }
    }

    #[test]
    fn quit_read_omits_only_interaction_epoch_and_requires_an_owned_valid_read() {
        for kind in [
            Kind::InitialLoad,
            Kind::InitialLoadDefault,
            Kind::Load,
            Kind::Import,
            Kind::Save,
            Kind::ExitSave,
            Kind::Export,
            Kind::Restore,
        ] {
            let (started_tx, started_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let mut service = Service::start_with(
                Block {
                    started: started_tx,
                    release: release_rx,
                    fail: false,
                },
                Duration::ZERO,
            )
            .unwrap();
            let original = profile("original");
            service
                .submit(kind, "owned.json".into(), original.clone(), stamp())
                .unwrap();
            started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            release_tx.send(()).unwrap();
            let mut completion = await_reply(&mut service);
            let mut after_quit = stamp();
            after_quit.interaction += 1;
            let read = matches!(
                kind,
                Kind::InitialLoad
                    | Kind::InitialLoadDefault
                    | Kind::Load
                    | Kind::Import
                    | Kind::Restore
            );
            assert_eq!(completion.matches_quit_read(&after_quit, &original), read);
            assert!(!completion.is_current(&after_quit, &original, true));
            for field in ["session", "profile", "active", "transfer"] {
                let mut changed = after_quit.clone();
                match field {
                    "session" => changed.session += 1,
                    "profile" => changed.profile += 1,
                    "active" => changed.active_path.push_str("changed"),
                    _ => changed.transfer_path.push_str("changed"),
                }
                assert!(!completion.matches_quit_read(&changed, &original));
            }
            assert!(!completion.matches_quit_read(&after_quit, &profile("edited")));
            completion.ticket += 1;
            assert!(!completion.matches_quit_read(&after_quit, &original));
            completion.ticket -= 1;
            completion.path = "changed.json".into();
            assert!(!completion.matches_quit_read(&after_quit, &original));
            completion.path = completion.pending.path.clone();
            completion.kind = if kind == Kind::Load {
                Kind::Import
            } else {
                Kind::Load
            };
            assert!(!completion.matches_quit_read(&after_quit, &original));
            completion.kind = kind;
            completion.pending.valid = false;
            assert!(!completion.matches_quit_read(&after_quit, &original));
        }
    }
}
