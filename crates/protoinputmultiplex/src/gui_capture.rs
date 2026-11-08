//! Bounded, opt-in screenshot publication for the UI script.
use eframe::egui::{self, ColorImage};
use image::ImageEncoder;
use std::{
    fs::{self, File, FileTimes, OpenOptions},
    io::{self, BufWriter},
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    time::SystemTime,
};

const MAX_PIXELS: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct Ticket {
    pub script_id: u64,
    pub viewport_id: egui::ViewportId,
    pub generation: u64,
    pub requested_at: SystemTime,
}

struct Frame {
    ticket: Ticket,
    image: Arc<ColorImage>,
}

#[derive(Default)]
struct State {
    pending: Option<Frame>,
    active: Option<u64>,
    accepted: u64,
    published: Option<u64>,
    failed: Option<u64>,
    rejected: Option<CaptureRejection>,
    stopped: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

pub struct CaptureWorker {
    shared: Arc<Shared>,
}

#[derive(Default, serde::Serialize)]
pub struct CaptureStatus {
    active: Option<u64>,
    pending: Option<u64>,
    published: Option<u64>,
    failed: Option<u64>,
    rejected: Option<CaptureRejection>,
}

#[derive(Clone, Copy, serde::Serialize)]
struct CaptureRejection {
    generation: u64,
    reason: &'static str,
}

/// Metadata only. No frame, filename, content digest or writer error text.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PublicationSnapshot {
    pub accepted: u64,
    pub active: Option<u64>,
    pub pending: Option<u64>,
    pub published: Option<u64>,
    pub failed: Option<u64>,
    pub rejected: Option<PublicationRejection>,
    pub retired: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PublicationRejection {
    pub generation: u64,
}
impl PublicationSnapshot {
    pub(crate) fn supported(&self) -> bool {
        [self.active, self.pending, self.published, self.failed]
            .into_iter()
            .flatten()
            .all(|generation| generation != 0 && generation <= self.accepted)
            && self
                .rejected
                .as_ref()
                .is_none_or(|rejection| rejection.generation != 0)
            && (!self.retired || self.pending.is_none())
    }
}
#[derive(Clone)]
pub(crate) struct PublicationObserver {
    shared: Arc<Shared>,
}
impl std::fmt::Debug for PublicationObserver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PublicationObserver")
            .finish_non_exhaustive()
    }
}
impl PublicationObserver {
    /// One bounded attempt. Writer contention/poison is unknown, never a wait.
    pub(crate) fn snapshot(&self) -> Option<PublicationSnapshot> {
        let state = self.shared.state.try_lock().ok()?;
        Some(PublicationSnapshot {
            accepted: state.accepted,
            active: state.active,
            pending: state.pending.as_ref().map(|frame| frame.ticket.generation),
            published: state.published,
            failed: state.failed,
            rejected: state.rejected.map(|rejection| PublicationRejection {
                generation: rejection.generation,
            }),
            retired: state.stopped,
        })
    }
}

impl CaptureWorker {
    pub(crate) fn observer(&self) -> PublicationObserver {
        PublicationObserver {
            shared: self.shared.clone(),
        }
    }
    pub fn start(destination: PathBuf) -> io::Result<Self> {
        Self::start_with(move |frame, shared| publish(&destination, frame, shared))
    }

    pub fn start_guarded(
        destination: PathBuf,
        guards: Arc<crate::gui::RuntimeGuards>,
    ) -> io::Result<Self> {
        Self::start_with_guards(
            move |frame, shared| publish(&destination, frame, shared),
            Some(guards),
        )
    }

    fn start_with(
        writer: impl FnMut(&Frame, &Shared) -> io::Result<bool> + Send + 'static,
    ) -> io::Result<Self> {
        Self::start_with_guards(writer, None)
    }

    fn start_with_guards(
        writer: impl FnMut(&Frame, &Shared) -> io::Result<bool> + Send + 'static,
        guards: Option<Arc<crate::gui::RuntimeGuards>>,
    ) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        });
        let worker_shared = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("ui-script-png".into())
            .spawn(move || {
                let _guards = guards;
                run(worker_shared, writer)
            })?;
        drop(handle);
        Ok(Self { shared })
    }

    /// A short swap only. The renderer already owns these pixels; no copy or I/O occurs here.
    pub fn submit(&self, ticket: Ticket, image: Arc<ColorImage>) -> bool {
        let rejection = match image.size[0].checked_mul(image.size[1]) {
            None => Some("capture dimensions overflow"),
            Some(0) => Some("capture is empty"),
            Some(pixels) if pixels > MAX_PIXELS => Some("capture exceeds 16 Mi pixels"),
            Some(pixels) if pixels != image.pixels.len() => Some("capture pixel count mismatch"),
            Some(_) => None,
        };
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.stopped || ticket.generation <= state.accepted {
            return false;
        }
        if let Some(reason) = rejection {
            if state
                .rejected
                .is_none_or(|previous| ticket.generation > previous.generation)
            {
                state.rejected = Some(CaptureRejection {
                    generation: ticket.generation,
                    reason,
                });
            }
            return false;
        }
        state.accepted = ticket.generation;
        state.rejected = None;
        state.pending = Some(Frame { ticket, image });
        self.shared.wake.notify_one();
        true
    }

    pub fn status(&self) -> CaptureStatus {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        CaptureStatus {
            active: state.active,
            pending: state.pending.as_ref().map(|f| f.ticket.generation),
            published: state.published,
            failed: state.failed,
            rejected: state.rejected,
        }
    }
}

impl Drop for CaptureWorker {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.stopped = true;
        state.pending = None;
        self.shared.wake.notify_one();
        // A filesystem call cannot be cancelled safely. The detached thread owns its state;
        // process exit ends any blocked call, with no in-process completion guarantee.
    }
}

fn run(shared: Arc<Shared>, mut writer: impl FnMut(&Frame, &Shared) -> io::Result<bool>) {
    loop {
        let frame = {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            while !state.stopped && state.pending.is_none() {
                state = shared.wake.wait(state).unwrap_or_else(|e| e.into_inner());
            }
            if state.stopped {
                return;
            }
            let frame = state.pending.take().expect("pending frame");
            state.active = Some(frame.ticket.generation);
            frame
        };
        // No queue or status lock is held across encoding or disk access.
        let result = writer(&frame, &shared);
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.active = None;
        match result {
            Ok(true) => {
                state.published = Some(frame.ticket.generation);
                state.failed = None;
            }
            Ok(false) => {}
            Err(_) => state.failed = Some(frame.ticket.generation),
        }
        shared.wake.notify_all();
    }
}

struct Temporary(PathBuf);

impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn publish(destination: &Path, frame: &Frame, shared: &Shared) -> io::Result<bool> {
    publish_with(destination, frame, shared, || Ok(()))
}

fn publish_with(
    destination: &Path,
    frame: &Frame,
    shared: &Shared,
    before_replace: impl FnOnce() -> io::Result<()>,
) -> io::Result<bool> {
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let mut opened = None;
    for attempt in 0..32 {
        let path = parent.join(format!(
            ".ui-capture-{}-{}-{}.tmp",
            std::process::id(),
            frame.ticket.generation,
            attempt
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                opened = Some((Temporary(path), file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    let (temporary, file) = opened
        .ok_or_else(|| io::Error::new(io::ErrorKind::AlreadyExists, "PNG temp names exhausted"))?;
    let rgba: Vec<u8> = frame
        .image
        .pixels
        .iter()
        .flat_map(|pixel| pixel.to_array())
        .collect();
    let mut output = BufWriter::new(file);
    image::codecs::png::PngEncoder::new(&mut output)
        .write_image(
            &rgba,
            frame.image.size[0] as u32,
            frame.image.size[1] as u32,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(io::Error::other)?;
    let file: File = output.into_inner().map_err(|e| e.into_error())?;
    file.set_times(FileTimes::new().set_modified(frame.ticket.requested_at))?;
    file.sync_all()?;
    drop(file);
    before_replace()?;
    {
        let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.stopped || state.accepted > frame.ticket.generation {
            return Ok(false);
        }
    }
    let source: Vec<_> = temporary
        .0
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let target: Vec<_> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let moved = unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            windows_sys::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING
                | windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::mpsc,
        time::{Duration, UNIX_EPOCH},
    };

    fn frame(generation: u64, requested_at: SystemTime) -> Frame {
        Frame {
            ticket: Ticket {
                script_id: 1,
                viewport_id: egui::ViewportId::ROOT,
                generation,
                requested_at,
            },
            image: Arc::new(ColorImage::new([1, 1], vec![egui::Color32::RED])),
        }
    }

    fn wait_for(shared: &Shared, predicate: impl Fn(&State) -> bool) {
        let state = shared.state.lock().unwrap();
        let (state, _) = shared
            .wake
            .wait_timeout_while(state, Duration::from_secs(5), |state| !predicate(state))
            .unwrap();
        assert!(predicate(&state), "capture worker made no progress");
    }

    fn publication_sample(observer: &crate::gui_activity::Observer) -> PublicationSnapshot {
        let end = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(value) = observer
                .snapshot()
                .render_capture
                .and_then(|capture| capture.publication)
            {
                return value;
            }
            assert!(
                std::time::Instant::now() < end,
                "publication metadata remained unknown"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn independent_publication_reader_advances_while_gui_idle_and_never_waits_for_queue_lock() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = CaptureWorker::start_with(move |frame, _| {
            if frame.ticket.generation == 1 {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                return Err(io::Error::other("controlled writer failure"));
            }
            Ok(true)
        })
        .unwrap();
        let probe = crate::gui_activity::Probe::new();
        let observer = probe.observer();
        let reporter = probe.bind_render_capture(1, true, Some(worker.observer()));
        let now = std::time::Instant::now();
        let first = frame(1, SystemTime::now());
        reporter.requested(1, now);
        let accepted = worker.submit(first.ticket, first.image);
        reporter.received(1, [1, 1], accepted, now);
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let blocked = observer.snapshot();
        assert_eq!(blocked.observation.unwrap().ui.entered, 0);
        assert_eq!(
            blocked.render_capture.unwrap().publication.unwrap().active,
            Some(1)
        );
        let held = worker.shared.state.lock().unwrap();
        let reader = observer.clone();
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || tx.send(reader.snapshot()).unwrap());
        let sampled = rx.recv_timeout(Duration::from_secs(1));
        drop(held);
        let sampled = sampled.expect("metadata read blocked on the queue lock");
        thread.join().unwrap();
        let capture = sampled.render_capture.unwrap();
        assert_eq!(capture.received_generation, 1);
        assert!(capture.publication.is_none());
        release_tx.send(()).unwrap();
        wait_for(&worker.shared, |state| state.failed == Some(1));
        assert_eq!(publication_sample(&observer).failed, Some(1));
        let second = frame(2, SystemTime::now());
        reporter.requested(2, now);
        let accepted = worker.submit(second.ticket, second.image);
        reporter.received(2, [1, 1], accepted, now);
        wait_for(&worker.shared, |state| state.published == Some(2));
        let idle = observer.snapshot();
        assert_eq!(idle.observation.unwrap().ui.entered, 0);
        let publication = publication_sample(&observer);
        assert_eq!(publication.published, Some(2));
        assert_eq!(publication.failed, None);
        drop(worker);
        assert!(publication_sample(&observer).retired);
        drop(reporter);
        assert!(observer.snapshot().render_capture.unwrap().retired);
    }

    #[test]
    fn blocked_writer_coalesces_to_latest_frame_and_bytes() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let writer_seen = Arc::clone(&seen);
        let worker = CaptureWorker::start_with(move |frame, _| {
            if frame.ticket.generation == 1 {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            }
            writer_seen
                .lock()
                .unwrap()
                .push((frame.ticket.generation, frame.image.pixels[0].to_array()));
            Ok(true)
        })
        .unwrap();
        let now = SystemTime::now();
        assert!(worker.submit(frame(1, now).ticket, frame(1, now).image));
        started_rx.recv().unwrap();
        for generation in 2..=5 {
            let mut next = frame(generation, now);
            next.image = Arc::new(ColorImage::new(
                [1, 1],
                vec![egui::Color32::from_rgb(generation as u8, 0, 0)],
            ));
            assert!(worker.submit(next.ticket, next.image));
        }
        assert_eq!(worker.status().active, Some(1));
        assert_eq!(worker.status().pending, Some(5));
        let invalid = Arc::new(ColorImage {
            size: [MAX_PIXELS + 1, 1],
            source_size: egui::Vec2::ZERO,
            pixels: vec![egui::Color32::RED],
        });
        assert!(!worker.submit(frame(6, now).ticket, invalid));
        let rejection = worker.status().rejected.unwrap();
        assert_eq!(rejection.generation, 6);
        assert_eq!(rejection.reason, "capture exceeds 16 Mi pixels");
        release_tx.send(()).unwrap();
        wait_for(&worker.shared, |state| state.published == Some(5));
        assert_eq!(
            *seen.lock().unwrap(),
            vec![(1, [255, 0, 0, 255]), (5, [5, 0, 0, 255])]
        );
    }

    #[test]
    fn drop_does_not_join_blocked_writer_and_clears_pending() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = CaptureWorker::start_with(move |_, shared| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(!shared.state.lock().unwrap().stopped)
        })
        .unwrap();
        let shared = Arc::clone(&worker.shared);
        let now = SystemTime::now();
        let first = frame(1, now);
        assert!(worker.submit(first.ticket, first.image));
        started_rx.recv().unwrap();
        let pending = frame(2, now);
        assert!(worker.submit(pending.ticket, pending.image));
        let (dropped_tx, dropped_rx) = mpsc::channel();
        std::thread::spawn(move || {
            drop(worker);
            dropped_tx.send(()).unwrap();
        });
        dropped_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("drop waited for blocked writer");
        {
            let state = shared.state.lock().unwrap();
            assert!(state.stopped);
            assert!(state.pending.is_none());
        }
        release_tx.send(()).unwrap();
        wait_for(&shared, |state| state.active.is_none());
        assert!(shared.state.lock().unwrap().published.is_none());
    }

    #[test]
    fn failed_generation_recovers_and_never_reports_publication() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let writer_seen = Arc::clone(&seen);
        let worker = CaptureWorker::start_with(move |frame, _| {
            writer_seen.lock().unwrap().push(frame.ticket.generation);
            if frame.ticket.generation == 1 {
                Err(io::Error::other("injected write failure"))
            } else {
                Ok(true)
            }
        })
        .unwrap();
        let now = SystemTime::now();
        let first = frame(1, now);
        assert!(worker.submit(first.ticket, first.image));
        wait_for(&worker.shared, |state| state.failed == Some(1));
        assert_eq!(worker.status().published, None);
        let second = frame(2, now);
        assert!(worker.submit(second.ticket, second.image));
        wait_for(&worker.shared, |state| state.published == Some(2));
        let status = worker.status();
        assert_eq!(status.failed, None);
        assert_eq!(*seen.lock().unwrap(), vec![1, 2]);
    }

    #[test]
    fn failed_replace_preserves_prior_png_and_request_time_controls_freshness() {
        let directory = std::env::temp_dir().join(format!(
            "protoinputmultiplex-capture-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let destination = directory.join("ui.png");
        let request = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let event = request + Duration::from_secs(1);
        let shared = Shared {
            state: Mutex::new(State {
                accepted: 1,
                ..State::default()
            }),
            wake: Condvar::new(),
        };
        let mut prior = frame(1, request - Duration::from_secs(1));
        prior.image = Arc::new(ColorImage::new([1, 1], vec![egui::Color32::BLUE]));
        assert!(publish(&destination, &prior, &shared).unwrap());
        let prior_png = fs::read(&destination).unwrap();
        shared.state.lock().unwrap().accepted = 2;
        let next = frame(2, request);
        assert!(
            publish_with(&destination, &next, &shared, || Err(io::Error::other(
                "before replace"
            )))
            .is_err()
        );
        assert_eq!(fs::read(&destination).unwrap(), prior_png);
        assert!(publish(&destination, &next, &shared).unwrap());
        assert_eq!(
            image::open(&destination).unwrap().to_rgba8().into_raw(),
            vec![255, 0, 0, 255]
        );
        let modified = fs::metadata(&destination).unwrap().modified().unwrap();
        assert_eq!(modified, request);
        assert!(
            modified <= event,
            "late publication of a pre-event request must stay stale"
        );
        assert!(
            fs::read_dir(&directory)
                .unwrap()
                .all(|entry| entry.unwrap().path() == destination)
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
