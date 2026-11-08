//! Bounded, detached storage worker for the legacy Macro file controls.
use anyhow::{Context, Result, ensure};
use mux_core::automation::{Clip, MAX_CLIP_JSON_BYTES};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ContextId {
    pub session: u64,
    pub mutation: u64,
    pub macro_revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Save,
    Load,
}

enum Job {
    Save {
        ticket: u64,
        path: PathBuf,
        clip: Arc<Clip>,
    },
    Load {
        ticket: u64,
        path: PathBuf,
    },
}

impl Job {
    fn ticket(&self) -> u64 {
        match self {
            Self::Save { ticket, .. } | Self::Load { ticket, .. } => *ticket,
        }
    }
}

struct Pending {
    ticket: u64,
    kind: Kind,
    context: ContextId,
    valid: bool,
}

pub enum Value {
    Saved,
    Loaded(Clip),
}

pub struct Completion {
    pub kind: Kind,
    pub context: ContextId,
    pub result: Result<Value, String>,
}

struct Reply {
    ticket: u64,
    result: Result<Value, String>,
}

trait Storage: Send + 'static {
    fn save(&mut self, path: &Path, clip: &Clip) -> Result<()>;
    fn load(&mut self, path: &Path) -> Result<Clip>;
}

struct Filesystem;
impl Storage for Filesystem {
    fn save(&mut self, path: &Path, clip: &Clip) -> Result<()> {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            ensure!(
                !path.as_os_str().encode_wide().any(|unit| unit == 0),
                "macro file path contains NUL"
            );
        }
        let json = clip.to_json()?;
        ensure!(json.len() <= MAX_CLIP_JSON_BYTES, "macro file is too large");
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        atomic_save(path, json.as_bytes())?;
        Ok(())
    }

    fn load(&mut self, path: &Path) -> Result<Clip> {
        let bytes = read_bounded(File::open(path)?, MAX_CLIP_JSON_BYTES)?;
        Clip::from_json(std::str::from_utf8(&bytes)?).context("invalid macro file")
    }
}

fn read_bounded(reader: impl Read, maximum: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= maximum, "macro file is too large");
    Ok(bytes)
}

static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub(crate) fn atomic_save(path: &Path, bytes: &[u8]) -> Result<()> {
    atomic_save_with(
        path,
        |file| {
            file.write_all(bytes)?;
            file.sync_all()
        },
        replace_file,
    )
}

fn atomic_save_with(
    path: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
    replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let mut created = None;
    for attempt in 0..32 {
        let candidate = parent.join(format!(
            ".protoinputmultiplex-macro-{}-{}-{attempt}.tmp",
            std::process::id(),
            serial,
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                created = Some((Temporary(candidate), file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    let (temporary, mut file) = created.context("could not allocate a unique macro temp file")?;
    // The injected writer is used by the test to fail before commit. Production writes exactly
    // the validated clicked snapshot and synchronizes it before replacement.
    write(&mut file)?;
    drop(file);
    replace(&temporary.0, path)?;
    Ok(())
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let moved = unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            windows_sys::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING
                | windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mux_core::automation::{Action, TimedAction};
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };

    fn clip(x: i32) -> Arc<Clip> {
        Arc::new(
            Clip::new(vec![TimedAction {
                at_micros: 0,
                action: Action::MoveTo { user: 0, x, y: 7 },
            }])
            .unwrap(),
        )
    }

    fn context() -> ContextId {
        ContextId {
            session: 3,
            mutation: 4,
            macro_revision: 9,
        }
    }

    struct BlockingStorage {
        started: mpsc::Sender<(PathBuf, String)>,
        release: mpsc::Receiver<()>,
    }
    impl Storage for BlockingStorage {
        fn save(&mut self, path: &Path, clip: &Clip) -> Result<()> {
            self.started
                .send((path.to_path_buf(), clip.to_json()?))
                .unwrap();
            self.release.recv().unwrap();
            Ok(())
        }
        fn load(&mut self, _: &Path) -> Result<Clip> {
            unreachable!()
        }
    }

    fn await_completion(service: &mut Service) -> Completion {
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(done) = service.poll() {
                return done;
            }
            assert!(Instant::now() < until, "worker did not complete");
            std::thread::yield_now();
        }
    }

    #[test]
    fn blocked_storage_keeps_clicked_snapshot_and_rejects_duplicate_without_blocking_drop() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut service = Service::start_with(BlockingStorage {
            started: started_tx,
            release: release_rx,
        })
        .unwrap();
        let clicked = clip(11);
        service
            .save(
                PathBuf::from("clicked.json"),
                Arc::clone(&clicked),
                context(),
            )
            .unwrap();
        let (path, json) = started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(path, PathBuf::from("clicked.json"));
        assert_eq!(json, clicked.to_json().unwrap());
        assert!(
            service
                .save(PathBuf::from("later.json"), clip(99), context())
                .is_err()
        );
        assert_eq!(service.status().ticket, 1);
        release_tx.send(()).unwrap();
        assert!(matches!(
            await_completion(&mut service).result,
            Ok(Value::Saved)
        ));
        assert!(!service.pending());

        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut service = Service::start_with(BlockingStorage {
            started: started_tx,
            release: release_rx,
        })
        .unwrap();
        service
            .save(PathBuf::from("blocked.json"), clip(1), context())
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let dropper = std::thread::spawn(move || {
            drop(service);
            dropped_tx.send(()).unwrap();
        });
        dropped_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("GUI owner Drop joined blocked storage");
        dropper.join().unwrap();
        release_tx.send(()).unwrap();
    }

    struct BlockingLoad {
        started: mpsc::Sender<PathBuf>,
        release: mpsc::Receiver<()>,
    }
    impl Storage for BlockingLoad {
        fn save(&mut self, _: &Path, _: &Clip) -> Result<()> {
            unreachable!()
        }
        fn load(&mut self, path: &Path) -> Result<Clip> {
            self.started.send(path.to_path_buf()).unwrap();
            self.release.recv().unwrap();
            Ok((*clip(18)).clone())
        }
    }

    #[test]
    fn invalidated_load_reply_cannot_be_dispatched_and_new_ticket_can_start() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut service = Service::start_with(BlockingLoad {
            started: started_tx,
            release: release_rx,
        })
        .unwrap();
        service
            .load(PathBuf::from("original.json"), context())
            .unwrap();
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            PathBuf::from("original.json")
        );
        service.invalidate(); // Immediate Stop/profile replacement, before storage returns.
        assert!(!service.status().valid);
        release_tx.send(()).unwrap();
        let until = Instant::now() + Duration::from_secs(3);
        while service.pending() {
            assert!(
                service.poll().is_none(),
                "invalidated Load became dispatchable"
            );
            assert!(Instant::now() < until, "worker did not release stale Load");
            std::thread::yield_now();
        }
        assert!(!service.pending());
        assert_eq!(service.status().ticket, 1);
        release_tx.send(()).unwrap();
        let newer = ContextId {
            session: 5,
            mutation: 0,
            macro_revision: 0,
        };
        service.load(PathBuf::from("new.json"), newer).unwrap();
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            PathBuf::from("new.json")
        );
        let completion = await_completion(&mut service);
        assert_eq!(completion.context, newer);
        assert!(matches!(completion.result, Ok(Value::Loaded(_))));
        assert_eq!(service.status().ticket, 2);
    }

    #[test]
    fn atomic_save_preserves_previous_bytes_on_write_and_commit_error() {
        static NEXT_TEST: AtomicU64 = AtomicU64::new(1);
        let folder = std::env::temp_dir().join(format!(
            "protoinputmultiplex-macro-test-{}-{}",
            std::process::id(),
            NEXT_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&folder).unwrap();
        let target = folder.join("clip.json");
        fs::write(&target, b"previous").unwrap();
        #[cfg(windows)]
        {
            let forged = PathBuf::from(format!("{}\0suffix", target.display()));
            let mut storage = Filesystem;
            assert!(storage.save(&forged, &clip(5)).is_err());
            assert_eq!(fs::read(&target).unwrap(), b"previous");
        }
        let failed_write = atomic_save_with(
            &target,
            |file| {
                file.write_all(b"partial")?;
                Err(io::Error::other("write failed"))
            },
            replace_file,
        );
        assert!(failed_write.is_err());
        assert_eq!(fs::read(&target).unwrap(), b"previous");
        let failed_commit = atomic_save_with(
            &target,
            |file| {
                file.write_all(b"new")?;
                file.sync_all()
            },
            |_, _| Err(io::Error::other("commit failed")),
        );
        assert!(failed_commit.is_err());
        assert_eq!(fs::read(&target).unwrap(), b"previous");
        let clicked = clip(23);
        let mut service = Service::start_with(Filesystem).unwrap();
        service
            .save(target.clone(), Arc::clone(&clicked), context())
            .unwrap();
        assert!(matches!(
            await_completion(&mut service).result,
            Ok(Value::Saved)
        ));
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            clicked.to_json().unwrap()
        );
        service.load(target.clone(), context()).unwrap();
        assert!(matches!(await_completion(&mut service).result,
            Ok(Value::Loaded(Clip { events, .. })) if events.len() == 1));
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn bounded_read_rejects_growing_and_invalid_files_then_recovers() {
        struct Growing {
            remaining: usize,
        }
        impl Read for Growing {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let count = self.remaining.min(buf.len());
                buf[..count].fill(b'x');
                self.remaining -= count;
                Ok(count)
            }
        }
        assert!(read_bounded(Growing { remaining: 1024 }, 32).is_err());
        static NEXT_TEST: AtomicU64 = AtomicU64::new(1);
        let folder = std::env::temp_dir().join(format!(
            "protoinputmultiplex-macro-read-{}-{}",
            std::process::id(),
            NEXT_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&folder).unwrap();
        let target = folder.join("clip.json");
        let mut storage = Filesystem;
        fs::write(&target, b"invalid").unwrap();
        assert!(storage.load(&target).is_err());
        let file = File::create(&target).unwrap();
        file.set_len(MAX_CLIP_JSON_BYTES as u64 + 1).unwrap();
        drop(file);
        assert!(storage.load(&target).is_err());
        storage.save(&target, &clip(8)).unwrap();
        assert_eq!(storage.load(&target).unwrap().events.len(), 1);
        fs::remove_dir_all(folder).unwrap();
    }
}

pub struct Service {
    jobs: SyncSender<Job>,
    replies: Receiver<Reply>,
    pending: Option<Pending>,
    next_ticket: u64,
    last_kind: Option<Kind>,
}

#[derive(serde::Serialize)]
pub struct Status {
    pub ticket: u64,
    pub kind: Option<Kind>,
    pub pending: bool,
    pub pending_context: Option<ContextId>,
    pub valid: bool,
}

impl Service {
    pub fn start(guards: Arc<crate::gui::RuntimeGuards>) -> Result<Self> {
        Self::start_with_guards(Filesystem, Some(guards))
    }

    #[cfg(test)]
    fn start_with(storage: impl Storage) -> Result<Self> {
        Self::start_with_guards(storage, None)
    }

    fn start_with_guards(
        mut storage: impl Storage,
        guards: Option<Arc<crate::gui::RuntimeGuards>>,
    ) -> Result<Self> {
        let (jobs, incoming) = mpsc::sync_channel::<Job>(1);
        let (outgoing, replies) = mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("gui-macro-file".into())
            .spawn(move || {
                let _guards = guards;
                while let Ok(job) = incoming.recv() {
                    let ticket = job.ticket();
                    let result = match job {
                        Job::Save { path, clip, .. } => {
                            storage.save(&path, &clip).map(|()| Value::Saved)
                        }
                        Job::Load { path, .. } => storage.load(&path).map(Value::Loaded),
                    }
                    .map_err(|error| format!("{error:#}"));
                    if outgoing.try_send(Reply { ticket, result }).is_err() {
                        break;
                    }
                }
                drop(storage);
                drop(_guards);
            })?;
        drop(worker); // A blocked filesystem operation must never delay GUI owner Drop.
        Ok(Self {
            jobs,
            replies,
            pending: None,
            next_ticket: 0,
            last_kind: None,
        })
    }

    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn status(&self) -> Status {
        Status {
            ticket: self.next_ticket,
            kind: self
                .pending
                .as_ref()
                .map_or(self.last_kind, |pending| Some(pending.kind)),
            pending: self.pending.is_some(),
            pending_context: self.pending.as_ref().map(|pending| pending.context),
            valid: self.pending.as_ref().is_none_or(|pending| pending.valid),
        }
    }

    pub fn invalidate(&mut self) {
        if let Some(pending) = &mut self.pending {
            pending.valid = false;
        }
    }

    pub fn save(&mut self, path: PathBuf, clip: Arc<Clip>, context: ContextId) -> Result<()> {
        self.submit(Kind::Save, context, |ticket| Job::Save {
            ticket,
            path,
            clip,
        })
    }

    pub fn load(&mut self, path: PathBuf, context: ContextId) -> Result<()> {
        self.submit(Kind::Load, context, |ticket| Job::Load { ticket, path })
    }

    fn submit(
        &mut self,
        kind: Kind,
        context: ContextId,
        job: impl FnOnce(u64) -> Job,
    ) -> Result<()> {
        ensure!(
            self.pending.is_none(),
            "macro file operation is already pending"
        );
        let ticket = self
            .next_ticket
            .checked_add(1)
            .context("macro file ticket exhausted")?;
        match self.jobs.try_send(job(ticket)) {
            Ok(()) => {
                self.next_ticket = ticket;
                self.last_kind = Some(kind);
                self.pending = Some(Pending {
                    ticket,
                    kind,
                    context,
                    valid: true,
                });
                Ok(())
            }
            Err(TrySendError::Full(_)) => anyhow::bail!("macro file worker is busy"),
            Err(TrySendError::Disconnected(_)) => anyhow::bail!("macro file worker stopped"),
        }
    }

    pub fn poll(&mut self) -> Option<Completion> {
        let reply = match self.replies.try_recv() {
            Ok(reply) => reply,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                let pending = self.pending.take()?;
                return pending.valid.then_some(Completion {
                    kind: pending.kind,
                    context: pending.context,
                    result: Err("macro file worker stopped".into()),
                });
            }
        };
        let pending = self.pending.take()?;
        if reply.ticket != pending.ticket || !pending.valid {
            return None;
        }
        Some(Completion {
            kind: pending.kind,
            context: pending.context,
            result: reply.result,
        })
    }
}
