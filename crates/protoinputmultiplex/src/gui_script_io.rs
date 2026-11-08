//! Bounded file service for the explicit UI script. No GUI callback waits for disk I/O.
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::{self, FileTimes, OpenOptions},
    io::{self, Read, Write},
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

const MAX_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;
const MAX_SNAPSHOTS_BYTES: usize = 8 * 1024 * 1024;
const MAX_CRITICAL_BYTES: usize = 64 * 1024;
const MAX_CRITICAL_QUEUE_BYTES: usize = 2 * 1024 * 1024;
const MAX_CRITICAL_ITEMS: usize = 64;
const MAX_SAMPLE_BYTES: u64 = 64;
const SAMPLE_POLL: Duration = Duration::from_millis(50);
const DRIVER_SNAPSHOT_BURST: usize = 2;
const SNAPSHOT_REPLACE_RETRY_BUDGET: Duration = Duration::from_millis(750);
const SNAPSHOT_REPLACE_RETRY_POLL: Duration = Duration::from_millis(25);

struct Snapshot {
    bytes: Vec<u8>,
    requested_at: SystemTime,
}

struct Critical {
    path: PathBuf,
    line: String,
    sequence: u64,
}

#[derive(Default)]
struct State {
    snapshots: BTreeMap<PathBuf, Snapshot>,
    snapshot_order: VecDeque<PathBuf>,
    snapshot_bytes: usize,
    critical: VecDeque<Critical>,
    critical_bytes: usize,
    next_sequence: u64,
    acked_sequence: u64,
    sample_expected: Option<usize>,
    sampled: Option<usize>,
    fault: Option<String>,
    stopped: bool,
    active: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    snapshot_paths: BTreeSet<PathBuf>,
    driver_snapshots: [PathBuf; 2],
    critical_paths: BTreeSet<PathBuf>,
    sampled_path: PathBuf,
}

pub struct ScriptIo {
    shared: Arc<Shared>,
}

#[derive(Clone, serde::Serialize)]
pub struct Status {
    pub submitted_sequence: u64,
    pub acked_sequence: u64,
    pub sampled: Option<usize>,
    pub fault: Option<String>,
    pub pending_snapshots: usize,
    pub pending_critical: usize,
    pub active: bool,
}

struct LimitedVec {
    bytes: Vec<u8>,
    max: usize,
}
impl Write for LimitedVec {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|size| size > self.max)
        {
            return Err(io::Error::other("UI script JSON exceeds its bound"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

trait IoOps: Send + 'static {
    fn snapshot(&mut self, path: &Path, job: &Snapshot, shared: &Shared) -> io::Result<()>;
    fn critical(&mut self, path: &Path, line: &str) -> io::Result<()>;
    fn sample(&mut self, path: &Path) -> Option<usize>;
}

struct Filesystem;
impl IoOps for Filesystem {
    fn snapshot(&mut self, path: &Path, job: &Snapshot, shared: &Shared) -> io::Result<()> {
        publish_snapshot(path, job, shared)
    }
    fn critical(&mut self, path: &Path, line: &str) -> io::Result<()> {
        append_line(path, line)
    }
    fn sample(&mut self, path: &Path) -> Option<usize> {
        read_sample(path)
    }
}

#[cfg(test)]
struct BlockingPhaseOps {
    phase_path: PathBuf,
    started: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
    blocked: bool,
}
#[cfg(test)]
impl IoOps for BlockingPhaseOps {
    fn snapshot(&mut self, path: &Path, job: &Snapshot, shared: &Shared) -> io::Result<()> {
        publish_snapshot(path, job, shared)
    }
    fn critical(&mut self, path: &Path, line: &str) -> io::Result<()> {
        if !self.blocked && path == self.phase_path.as_path() {
            self.blocked = true;
            self.started
                .send(())
                .map_err(|error| io::Error::other(error.to_string()))?;
            self.release
                .recv()
                .map_err(|error| io::Error::other(error.to_string()))?;
        }
        append_line(path, line)
    }
    fn sample(&mut self, path: &Path) -> Option<usize> {
        read_sample(path)
    }
}

impl ScriptIo {
    pub fn start(script_path: &Path) -> io::Result<Self> {
        Self::start_with(script_path, Filesystem)
    }

    pub fn start_guarded(
        script_path: &Path,
        guards: Arc<crate::gui::RuntimeGuards>,
    ) -> io::Result<Self> {
        Self::start_with_guards(script_path, Filesystem, Some(guards))
    }

    #[cfg(test)]
    pub(crate) fn start_with_phase_barrier(
        script_path: &Path,
        started: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) -> io::Result<Self> {
        Self::start_with(
            script_path,
            BlockingPhaseOps {
                phase_path: script_path.with_extension("phases.jsonl"),
                started,
                release,
                blocked: false,
            },
        )
    }

    fn start_with(script_path: &Path, operations: impl IoOps) -> io::Result<Self> {
        Self::start_with_guards(script_path, operations, None)
    }

    fn start_with_guards(
        script_path: &Path,
        operations: impl IoOps,
        guards: Option<Arc<crate::gui::RuntimeGuards>>,
    ) -> io::Result<Self> {
        let extension = |name| script_path.with_extension(name);
        let snapshot_paths = [
            "ready.json",
            "progress.json",
            "window-lock.json",
            "launch.json",
            "discovery.json",
            "state.json",
            "state.screen.json",
            "window-selection.json",
            "recovery.json",
            "annotation.json",
            "magnifier.json",
            "macro-studio.json",
            "macro-file.json",
            "virtual-users.json",
            "graph.json",
            "mapper.json",
            "user-pause.json",
            "output.json",
            "app-preferences.json",
            "profile.json",
            "agent.json",
        ]
        .into_iter()
        .map(extension)
        .chain([
            script_path.with_file_name("forwarding-state.json"),
            extension("output.json").with_extension("page.json"),
            extension("profile.json").with_extension("navigation.json"),
            extension("app-preferences.json").with_extension("shortcuts.json"),
        ])
        .collect();
        let critical_paths = ["events.jsonl", "phases.jsonl"]
            .into_iter()
            .map(extension)
            .chain(
                [
                    "window-selection.json",
                    "annotation.json",
                    "magnifier.json",
                    "macro-studio.json",
                    "virtual-users.json",
                    "graph.json",
                    "mapper.json",
                    "user-pause.json",
                    "output.json",
                    "app-preferences.json",
                    "agent.json",
                ]
                .into_iter()
                .map(|name| extension(name).with_extension("acks.jsonl")),
            )
            .chain([script_path.with_file_name("forwarding-acks.jsonl")])
            .collect();
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            snapshot_paths,
            driver_snapshots: [extension("ready.json"), extension("progress.json")],
            critical_paths,
            sampled_path: extension("sampled.json"),
        });
        let worker_shared = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("ui-script-files".into())
            .spawn(move || {
                let _guards = guards;
                worker(worker_shared, operations)
            })?;
        drop(handle);
        Ok(Self { shared })
    }

    fn serialize<T: serde::Serialize + ?Sized>(
        &self,
        value: &T,
        max: usize,
        pretty: bool,
    ) -> Option<Vec<u8>> {
        if self.status().fault.is_some() {
            return None;
        }
        let mut output = LimitedVec {
            bytes: Vec::new(),
            max,
        };
        let result = if pretty {
            serde_json::to_writer_pretty(&mut output, value)
        } else {
            serde_json::to_writer(&mut output, value)
        };
        if result.is_err() {
            self.record_fault("UI script JSON serialization exceeded its bound or failed");
            return None;
        }
        Some(output.bytes)
    }

    pub fn snapshot_json<T: serde::Serialize + ?Sized>(&self, value: &T) -> Option<Vec<u8>> {
        self.serialize(value, MAX_SNAPSHOT_BYTES, true)
    }

    pub fn line_json<T: serde::Serialize + ?Sized>(&self, value: &T) -> Option<String> {
        let bytes = self.serialize(value, MAX_CRITICAL_BYTES, false)?;
        match String::from_utf8(bytes) {
            Ok(line) => Some(line),
            Err(_) => {
                self.record_fault("UI script JSON was not UTF-8");
                None
            }
        }
    }

    fn record_fault(&self, reason: &str) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.fault.is_none() {
            state.fault = Some(reason.to_owned());
        }
        self.shared.wake.notify_all();
    }

    pub fn snapshot(&self, path: &Path, bytes: Vec<u8>) -> bool {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.stopped || state.fault.is_some() {
            return false;
        }
        if !self.shared.snapshot_paths.contains(path) || bytes.len() > MAX_SNAPSHOT_BYTES {
            state.fault = Some("UI script snapshot path or payload exceeded its bound".into());
            return false;
        }
        let previous = state.snapshots.get(path).map_or(0, |job| job.bytes.len());
        if state.snapshot_bytes - previous + bytes.len() > MAX_SNAPSHOTS_BYTES {
            state.fault = Some("UI script snapshot queue exceeded 8 MiB".into());
            return false;
        }
        state.snapshot_bytes = state.snapshot_bytes - previous + bytes.len();
        if !state.snapshots.contains_key(path) {
            state.snapshot_order.push_back(path.to_path_buf());
        }
        state.snapshots.insert(
            path.to_path_buf(),
            Snapshot {
                bytes,
                requested_at: SystemTime::now(),
            },
        );
        self.shared.wake.notify_one();
        true
    }

    /// Critical records are never coalesced. A failure latches the script closed.
    pub fn append(&self, path: &Path, line: String) -> Option<u64> {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.stopped || state.fault.is_some() {
            return None;
        }
        if !self.shared.critical_paths.contains(path) || line.len() > MAX_CRITICAL_BYTES {
            state.fault = Some("UI script critical path or record exceeded its bound".into());
            return None;
        }
        if state.critical.len() >= MAX_CRITICAL_ITEMS
            || state.critical_bytes + line.len() + 1 > MAX_CRITICAL_QUEUE_BYTES
        {
            state.fault = Some("UI script critical queue exceeded its bound".into());
            return None;
        }
        state.next_sequence = state.next_sequence.saturating_add(1);
        let sequence = state.next_sequence;
        state.critical_bytes += line.len() + 1;
        state.critical.push_back(Critical {
            path: path.to_path_buf(),
            line,
            sequence,
        });
        self.shared.wake.notify_one();
        Some(sequence)
    }

    pub fn request_sample(&self, expected: usize) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.sample_expected != Some(expected) {
            state.sample_expected = Some(expected);
            state.sampled = None;
        }
        self.shared.wake.notify_one();
    }

    pub fn clear_sample(&self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.sample_expected = None;
        state.sampled = None;
    }

    pub fn status(&self) -> Status {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        Status {
            submitted_sequence: state.next_sequence,
            acked_sequence: state.acked_sequence,
            sampled: state.sampled,
            fault: state.fault.clone(),
            pending_snapshots: state.snapshots.len(),
            pending_critical: state.critical.len(),
            active: state.active,
        }
    }

    pub fn allows(&self, sequence: Option<u64>) -> bool {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.fault.is_none() && sequence.is_none_or(|sequence| state.acked_sequence >= sequence)
    }

    #[cfg(test)]
    pub fn wait_for(&self, predicate: impl Fn(&Status) -> bool) {
        let mut state = self.shared.state.lock().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let status = Status {
                submitted_sequence: state.next_sequence,
                acked_sequence: state.acked_sequence,
                sampled: state.sampled,
                fault: state.fault.clone(),
                pending_snapshots: state.snapshots.len(),
                pending_critical: state.critical.len(),
                active: state.active,
            };
            if predicate(&status) {
                return;
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            assert!(
                !remaining.is_zero(),
                "UI script I/O worker made no progress"
            );
            (state, _) = self.shared.wake.wait_timeout(state, remaining).unwrap();
        }
    }
}

impl Drop for ScriptIo {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.stopped = true;
        state.snapshots.clear();
        state.snapshot_order.clear();
        state.critical.clear();
        state.snapshot_bytes = 0;
        state.critical_bytes = 0;
        self.shared.wake.notify_one();
        // A blocked OS operation cannot be cancelled here; process exit ends the detached worker.
    }
}

fn worker(shared: Arc<Shared>, mut operations: impl IoOps) {
    let mut last_sample_poll = Instant::now() - SAMPLE_POLL;
    let mut critical_streak = 0usize;
    let mut driver_snapshot_streak = 0;
    loop {
        let (critical, snapshot, expected) = {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.stopped || state.fault.is_some() {
                return;
            }
            let expected = if state.fault.is_none() && last_sample_poll.elapsed() >= SAMPLE_POLL {
                state.sample_expected
            } else {
                None
            };
            let snapshot_due = critical_streak >= 4 && !state.snapshot_order.is_empty();
            let critical = if expected.is_none() && !snapshot_due {
                state.critical.pop_front()
            } else {
                None
            };
            if let Some(job) = &critical {
                state.critical_bytes -= job.line.len() + 1;
            }
            let snapshot = if critical.is_none() && expected.is_none() {
                // Ready and progress gate the script driver. Prefer them over bulk
                // telemetry, with a bounded burst so older telemetry still advances.
                let prefer_driver = driver_snapshot_streak < DRIVER_SNAPSHOT_BURST;
                let index = state
                    .snapshot_order
                    .iter()
                    .position(|path| shared.driver_snapshots.contains(path) == prefer_driver)
                    .unwrap_or(0);
                state
                    .snapshot_order
                    .remove(index)
                    .and_then(|path| state.snapshots.remove(&path).map(|job| (path, job)))
            } else {
                None
            };
            if let Some((_, job)) = &snapshot {
                state.snapshot_bytes -= job.bytes.len();
            }
            state.active = critical.is_some() || snapshot.is_some() || expected.is_some();
            (critical, snapshot, expected)
        };
        if let Some(job) = critical {
            critical_streak = critical_streak.saturating_add(1);
            let result = operations.critical(&job.path, &job.line);
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            match result {
                Ok(()) => state.acked_sequence = job.sequence,
                Err(error) => {
                    state.fault = Some(format!("UI script critical write failed: {error}"));
                    state.critical.clear();
                    state.critical_bytes = 0;
                }
            }
            shared.wake.notify_all();
        } else if let Some((path, job)) = snapshot {
            critical_streak = 0;
            driver_snapshot_streak = if shared.driver_snapshots.contains(&path) {
                (driver_snapshot_streak + 1).min(DRIVER_SNAPSHOT_BURST)
            } else {
                0
            };
            let result = operations.snapshot(&path, &job, &shared);
            if let Err(error) = result {
                let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                if !state.stopped {
                    state.fault = Some(format!("UI script snapshot write failed: {error}"));
                }
                shared.wake.notify_all();
            }
        } else if let Some(expected) = expected {
            let sampled = operations
                .sample(&shared.sampled_path)
                .filter(|count| *count >= expected);
            last_sample_poll = Instant::now();
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.sample_expected == Some(expected) {
                state.sampled = sampled;
            }
            shared.wake.notify_all();
        }
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.active = false;
        shared.wake.notify_all();
        if !state.stopped && state.critical.is_empty() && state.snapshots.is_empty() {
            if state.sample_expected.is_some() && state.fault.is_none() {
                let remaining = SAMPLE_POLL.saturating_sub(last_sample_poll.elapsed());
                drop(shared.wake.wait_timeout(state, remaining));
            } else {
                drop(shared.wake.wait(state));
            }
        }
    }
}

fn append_line(path: &Path, line: &str) -> io::Result<()> {
    let mut file = OpenOptions::new().append(true).create(true).open(path)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    file.flush()?;
    file.sync_data()
}

fn read_sample(path: &Path) -> Option<usize> {
    let mut bytes = Vec::new();
    OpenOptions::new()
        .read(true)
        .open(path)
        .ok()?
        .take(MAX_SAMPLE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_SAMPLE_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn publish_snapshot(path: &Path, job: &Snapshot, shared: &Shared) -> io::Result<()> {
    publish_snapshot_with_replace(path, job, shared, replace_snapshot)
}

fn snapshot_stage_error(
    stage: &str,
    target: &Path,
    temporary: Option<&Path>,
    error: io::Error,
) -> io::Error {
    let temporary = temporary.map_or_else(String::new, |path| format!(" ({})", path.display()));
    io::Error::new(
        error.kind(),
        format!(
            "{stage}{temporary} for target {}: {error}",
            target.display()
        ),
    )
}

fn is_transient_replace_error(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(5 | 32 | 33))
}

fn replace_snapshot(source: &Path, target: &Path) -> io::Result<()> {
    let source: Vec<_> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<_> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    let moved = unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
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

fn publish_snapshot_with_replace(
    path: &Path,
    job: &Snapshot,
    shared: &Shared,
    mut replace: impl FnMut(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let mut created = None;
    for attempt in 0..32 {
        let candidate = parent.join(format!(
            ".ui-script-{}-{}-{}.tmp",
            std::process::id(),
            serial,
            attempt
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
            Err(error) => {
                return Err(snapshot_stage_error(
                    "create temporary snapshot",
                    path,
                    Some(&candidate),
                    error,
                ));
            }
        }
    }
    let (temporary, mut file) = created.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "create temporary snapshot for target {}: UI script temporary names exhausted",
                path.display()
            ),
        )
    })?;
    file.write_all(&job.bytes).map_err(|error| {
        snapshot_stage_error("write temporary snapshot", path, Some(&temporary.0), error)
    })?;
    file.set_times(FileTimes::new().set_modified(job.requested_at))
        .map_err(|error| {
            snapshot_stage_error(
                "set temporary snapshot time",
                path,
                Some(&temporary.0),
                error,
            )
        })?;
    file.sync_all().map_err(|error| {
        snapshot_stage_error("sync temporary snapshot", path, Some(&temporary.0), error)
    })?;
    drop(file);
    if shared
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .stopped
    {
        return Ok(());
    }
    let deadline = Instant::now() + SNAPSHOT_REPLACE_RETRY_BUDGET;
    loop {
        if shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .stopped
        {
            return Ok(());
        }
        match replace(&temporary.0, path) {
            Ok(()) => return Ok(()),
            Err(error) if is_transient_replace_error(&error) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(snapshot_stage_error(
                        "replace snapshot after retry budget",
                        path,
                        None,
                        error,
                    ));
                }
                let state = shared
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if state.stopped {
                    return Ok(());
                }
                let (state, _) = shared
                    .wake
                    .wait_timeout_while(
                        state,
                        remaining.min(SNAPSHOT_REPLACE_RETRY_POLL),
                        |state| !state.stopped,
                    )
                    .unwrap_or_else(|error| error.into_inner());
                if state.stopped {
                    return Ok(());
                }
            }
            Err(error) => return Err(snapshot_stage_error("replace snapshot", path, None, error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::windows::fs::OpenOptionsExt, sync::mpsc};

    type SnapshotFn = Box<dyn FnMut(&Path, &Snapshot, &Shared) -> io::Result<()> + Send>;
    type CriticalFn = Box<dyn FnMut(&Path, &str) -> io::Result<()> + Send>;
    struct FakeOps {
        snapshot: SnapshotFn,
        critical: CriticalFn,
    }
    impl IoOps for FakeOps {
        fn snapshot(&mut self, path: &Path, job: &Snapshot, shared: &Shared) -> io::Result<()> {
            (self.snapshot)(path, job, shared)
        }
        fn critical(&mut self, path: &Path, line: &str) -> io::Result<()> {
            (self.critical)(path, line)
        }
        fn sample(&mut self, _: &Path) -> Option<usize> {
            None
        }
    }
    fn path() -> PathBuf {
        PathBuf::from("ui.json")
    }
    fn fake(snapshot: SnapshotFn, critical: CriticalFn) -> FakeOps {
        FakeOps { snapshot, critical }
    }

    fn snapshot_test_directory(label: &str) -> PathBuf {
        static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);
        let directory = std::env::temp_dir().join(format!(
            "ui-script-{label}-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        directory
    }

    fn open_reader_without_delete_sharing(path: &Path) -> std::fs::File {
        OpenOptions::new()
            .read(true)
            .share_mode(
                windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ
                    | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE,
            )
            .open(path)
            .unwrap()
    }

    #[test]
    fn blocked_snapshot_coalesces_latest_without_blocking_submit() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (latest_started_tx, latest_started_rx) = mpsc::channel();
        let (latest_release_tx, latest_release_rx) = mpsc::channel();
        let (written_tx, written_rx) = mpsc::channel();
        let service = ScriptIo::start_with(
            &path(),
            fake(
                Box::new(move |_, job, _| {
                    if job.bytes == b"first" {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    } else if job.bytes == b"latest" {
                        latest_started_tx.send(()).unwrap();
                        latest_release_rx.recv().unwrap();
                    }
                    written_tx.send(job.bytes.clone()).unwrap();
                    Ok(())
                }),
                Box::new(|_, _| Ok(())),
            ),
        )
        .unwrap();
        let target = PathBuf::from("ui.progress.json");
        assert!(service.snapshot(&target, b"first".to_vec()));
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        for value in ["second", "third", "latest"] {
            assert!(service.snapshot(&target, value.as_bytes().to_vec()));
        }
        let distinct = PathBuf::from("ui.window-lock.json");
        assert!(service.snapshot(&distinct, b"distinct".to_vec()));
        assert_eq!(service.status().pending_snapshots, 2);
        assert_eq!(
            service.shared.state.lock().unwrap().snapshots[&target].bytes,
            b"latest"
        );
        release_tx.send(()).unwrap();
        assert_eq!(
            written_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            b"first"
        );
        latest_started_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert!(service.snapshot(&target, b"newest".to_vec()));
        latest_release_tx.send(()).unwrap();
        assert_eq!(
            written_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            b"latest"
        );
        assert_eq!(
            written_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            b"distinct"
        );
        assert_eq!(
            written_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            b"newest"
        );
    }

    #[test]
    fn driver_snapshots_overtake_older_telemetry_with_a_bounded_burst() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let service = ScriptIo::start_with(
            &path(),
            fake(
                Box::new(move |path, job, _| {
                    started_tx
                        .send((path.to_path_buf(), job.bytes.clone()))
                        .unwrap();
                    release_rx.recv().map_err(io::Error::other)
                }),
                Box::new(|_, _| Ok(())),
            ),
        )
        .unwrap();
        let expect = |name: &str, bytes: &[u8]| {
            assert_eq!(
                started_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
                (PathBuf::from(name), bytes.to_vec())
            );
        };
        assert!(service.snapshot(Path::new("ui.launch.json"), vec![0]));
        expect("ui.launch.json", &[0]);

        for (name, value) in [
            ("ui.window-lock.json", 1),
            ("ui.recovery.json", 2),
            ("ui.ready.json", 3),
            ("ui.progress.json", 4),
        ] {
            assert!(service.snapshot(Path::new(name), vec![value]));
        }
        assert!(service.snapshot(Path::new("ui.progress.json"), vec![5]));
        assert_eq!(service.status().pending_snapshots, 4);

        release_tx.send(()).unwrap();
        expect("ui.ready.json", &[3]);
        release_tx.send(()).unwrap();
        expect("ui.progress.json", &[5]);
        assert!(service.snapshot(Path::new("ui.ready.json"), vec![6]));
        release_tx.send(()).unwrap();
        expect("ui.window-lock.json", &[1]);
        release_tx.send(()).unwrap();
        expect("ui.ready.json", &[6]);
        release_tx.send(()).unwrap();
        expect("ui.recovery.json", &[2]);
        release_tx.send(()).unwrap();
    }

    #[test]
    fn replenished_critical_fifo_cannot_starve_pending_snapshot() {
        let (critical_started_tx, critical_started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (snapshot_tx, snapshot_rx) = mpsc::channel();
        let service = ScriptIo::start_with(
            &path(),
            fake(
                Box::new(move |path, _, _| {
                    snapshot_tx.send(path.to_path_buf()).unwrap();
                    Ok(())
                }),
                Box::new(move |_, _| {
                    critical_started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                }),
            ),
        )
        .unwrap();
        let critical_path = Path::new("ui.phases.jsonl");
        assert!(service.append(critical_path, "active".into()).is_some());
        critical_started_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert!(service.snapshot(Path::new("ui.progress.json"), b"visible".to_vec()));
        for _ in 0..4 {
            assert!(service.append(critical_path, "queued".into()).is_some());
        }
        for completed in 0..4 {
            assert!(
                service
                    .append(critical_path, "replenished".into())
                    .is_some()
            );
            release_tx.send(()).unwrap();
            if completed < 3 {
                critical_started_rx
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
            }
        }
        assert_eq!(
            snapshot_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            PathBuf::from("ui.progress.json")
        );
        assert!(service.status().pending_critical > 0);
        let shared = Arc::clone(&service.shared);
        drop(service);
        let _ = release_tx.send(());
        let state = shared.state.lock().unwrap();
        let (state, _) = shared
            .wake
            .wait_timeout_while(state, Duration::from_secs(2), |state| state.active)
            .unwrap();
        assert!(
            !state.active,
            "detached fake worker did not finish its active call"
        );
    }

    #[test]
    fn blocked_writer_does_not_delay_shutdown() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let service = ScriptIo::start_with(
            &path(),
            fake(
                Box::new(move |_, _, _| {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                }),
                Box::new(|_, _| Ok(())),
            ),
        )
        .unwrap();
        assert!(service.snapshot(Path::new("ui.progress.json"), vec![1]));
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(service.snapshot(Path::new("ui.ready.json"), vec![2]));
        let shared = Arc::clone(&service.shared);
        let (closed_tx, closed_rx) = mpsc::channel();
        std::thread::spawn(move || {
            drop(service);
            closed_tx.send(()).unwrap();
        });
        closed_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("drop waited for I/O");
        assert!(shared.state.lock().unwrap().snapshots.is_empty());
        release_tx.send(()).unwrap();
    }

    #[test]
    fn critical_ack_gates_next_step_and_write_failure_stays_closed() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let service = ScriptIo::start_with(
            &path(),
            fake(
                Box::new(|_, _, _| Ok(())),
                Box::new(move |_, _| {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                }),
            ),
        )
        .unwrap();
        let sequence = service
            .append(
                Path::new("ui.events.jsonl"),
                "{\"button\":\"Compatibility\"}".into(),
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!service.allows(Some(sequence)));
        release_tx.send(()).unwrap();
        service.wait_for(|status| status.acked_sequence >= sequence);
        assert!(service.allows(Some(sequence)));

        let failing = ScriptIo::start_with(
            &path(),
            fake(
                Box::new(|_, _, _| Ok(())),
                Box::new(|_, _| Err(io::Error::other("injected failure"))),
            ),
        )
        .unwrap();
        let sequence = failing
            .append(Path::new("ui.events.jsonl"), "required".into())
            .unwrap();
        failing.wait_for(|status| status.fault.is_some());
        assert!(!failing.allows(Some(sequence)));
        assert_eq!(failing.status().acked_sequence, 0);
    }

    #[test]
    fn critical_overflow_fails_closed_and_sample_read_recovers_from_invalid_input() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let service = ScriptIo::start_with(
            &path(),
            fake(
                Box::new(|_, _, _| Ok(())),
                Box::new(move |_, _| {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                }),
            ),
        )
        .unwrap();
        let first = service
            .append(Path::new("ui.events.jsonl"), "first".into())
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        for _ in 0..MAX_CRITICAL_ITEMS {
            assert!(
                service
                    .append(Path::new("ui.phases.jsonl"), "phase".into())
                    .is_some()
            );
        }
        assert!(
            service
                .append(Path::new("ui.events.jsonl"), "overflow".into())
                .is_none()
        );
        assert!(!service.allows(Some(first)));
        assert!(service.status().fault.unwrap().contains("critical queue"));
        drop(service);
        release_tx.send(()).unwrap();

        let oversized = ScriptIo::start_with(
            &path(),
            fake(Box::new(|_, _, _| Ok(())), Box::new(|_, _| Ok(()))),
        )
        .unwrap();
        assert!(
            oversized
                .line_json(&"x".repeat(MAX_CRITICAL_BYTES))
                .is_none()
        );
        assert!(oversized.status().fault.is_some());

        let directory = std::env::temp_dir().join(format!(
            "ui-script-sample-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let service = ScriptIo::start(&directory.join("ui.json")).unwrap();
        service.request_sample(4);
        let sample = directory.join("ui.sampled.json");
        fs::write(&sample, vec![b'9'; MAX_SAMPLE_BYTES as usize + 1]).unwrap();
        assert_eq!(read_sample(&sample), None);
        fs::write(&sample, b"{").unwrap();
        assert_eq!(read_sample(&sample), None);
        fs::write(&sample, b"4").unwrap();
        assert_eq!(read_sample(&sample), Some(4));
        service.wait_for(|status| status.sampled == Some(4));
        service.clear_sample();
        drop(service);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn snapshot_mtime_is_request_time_before_atomic_publication() {
        let directory = std::env::temp_dir().join(format!(
            "ui-script-snapshot-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let service = ScriptIo::start(&directory.join("ui.json")).unwrap();
        let destination = directory.join("ui.progress.json");
        let request = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let event = request + Duration::from_secs(1);
        publish_snapshot(
            &destination,
            &Snapshot {
                bytes: b"{\"frame\":1}".to_vec(),
                requested_at: request,
            },
            &service.shared,
        )
        .unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"{\"frame\":1}");
        assert!(fs::metadata(&destination).unwrap().modified().unwrap() < event);
        drop(service);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn reader_sharing_denial_retries_atomic_replace_and_keeps_request_time() {
        let directory = snapshot_test_directory("reader-retry");
        let destination = directory.join("ui.progress.json");
        fs::write(&destination, b"old valid bytes").unwrap();
        let reader = open_reader_without_delete_sharing(&destination);
        let request = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let (first_attempt_tx, first_attempt_rx) = mpsc::channel();
        let mut first_attempt_tx = Some(first_attempt_tx);
        let service = ScriptIo::start_with(
            &directory.join("ui.json"),
            fake(
                Box::new(move |path, job, shared| {
                    let fixed_job = Snapshot {
                        bytes: job.bytes.clone(),
                        requested_at: request,
                    };
                    publish_snapshot_with_replace(path, &fixed_job, shared, |source, target| {
                        let result = replace_snapshot(source, target);
                        if let Some(sender) = first_attempt_tx.take() {
                            sender
                                .send(result.as_ref().err().and_then(io::Error::raw_os_error))
                                .unwrap();
                        }
                        result
                    })
                }),
                Box::new(|_, _| Ok(())),
            ),
        )
        .unwrap();
        assert!(service.snapshot(&destination, b"new valid bytes".to_vec()));
        assert!(matches!(
            first_attempt_rx
                .recv_timeout(Duration::from_secs(2))
                .unwrap(),
            Some(5 | 32 | 33)
        ));
        assert_eq!(fs::read(&destination).unwrap(), b"old valid bytes");
        drop(reader);
        service.wait_for(|status| !status.active && status.pending_snapshots == 0);
        assert!(service.status().fault.is_none());
        assert_eq!(fs::read(&destination).unwrap(), b"new valid bytes");
        assert_eq!(
            fs::metadata(&destination).unwrap().modified().unwrap(),
            request
        );
        drop(service);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn sustained_reader_denial_preserves_old_snapshot_and_reports_replace_target() {
        let directory = snapshot_test_directory("reader-denial");
        let destination = directory.join("ui.progress.json");
        fs::write(&destination, b"old valid bytes").unwrap();
        let reader = open_reader_without_delete_sharing(&destination);
        let service = ScriptIo::start(&directory.join("ui.json")).unwrap();
        assert!(service.snapshot(&destination, b"new valid bytes".to_vec()));
        service.wait_for(|status| status.fault.is_some());
        let fault = service.status().fault.unwrap();
        assert!(
            fault.contains("replace snapshot after retry budget"),
            "{fault}"
        );
        assert!(
            fault.contains(&destination.display().to_string()),
            "{fault}"
        );
        assert!(
            ["os error 5", "os error 32", "os error 33"]
                .into_iter()
                .any(|code| fault.contains(code)),
            "{fault}"
        );
        assert_eq!(fs::read(&destination).unwrap(), b"old valid bytes");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        drop(reader);
        drop(service);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn drop_cancels_replace_retry_without_publishing_or_waiting_for_reader() {
        let directory = snapshot_test_directory("reader-stop");
        let destination = directory.join("ui.progress.json");
        fs::write(&destination, b"old valid bytes").unwrap();
        let reader = open_reader_without_delete_sharing(&destination);
        let (first_attempt_tx, first_attempt_rx) = mpsc::channel();
        let mut first_attempt_tx = Some(first_attempt_tx);
        let service = ScriptIo::start_with(
            &directory.join("ui.json"),
            fake(
                Box::new(move |path, job, shared| {
                    publish_snapshot_with_replace(path, job, shared, |source, target| {
                        let result = replace_snapshot(source, target);
                        if let Some(sender) = first_attempt_tx.take() {
                            sender
                                .send(result.as_ref().err().and_then(io::Error::raw_os_error))
                                .unwrap();
                        }
                        result
                    })
                }),
                Box::new(|_, _| Ok(())),
            ),
        )
        .unwrap();
        assert!(service.snapshot(&destination, b"new valid bytes".to_vec()));
        assert!(matches!(
            first_attempt_rx
                .recv_timeout(Duration::from_secs(2))
                .unwrap(),
            Some(5 | 32 | 33)
        ));
        let shared = Arc::clone(&service.shared);
        let started = Instant::now();
        drop(service);
        assert!(started.elapsed() < Duration::from_millis(250));
        let state = shared.state.lock().unwrap();
        let (state, _) = shared
            .wake
            .wait_timeout_while(state, Duration::from_secs(2), |state| state.active)
            .unwrap();
        assert!(!state.active, "snapshot retry did not observe Drop");
        drop(state);
        drop(reader);
        assert_eq!(fs::read(&destination).unwrap(), b"old valid bytes");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_dir_all(directory).unwrap();
    }
}
