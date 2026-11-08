//! Bounded PNG preparation off-thread, authorization, then atomic publication off-thread.
use crate::magnifier_runtime::{ExportFrame, ExportInfo};
use anyhow::{Context, Result, bail, ensure};
use image::ImageEncoder;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};

const MAX_PNG_BYTES: usize = mux_core::magnifier::MAX_PIXELS as usize * 4 + 1024 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

pub(crate) struct Prepared<M> {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
    pub metadata: M,
}

pub(crate) struct ExportJob<M = ExportInfo> {
    ready: Option<Receiver<Result<(Instant, M)>>>,
    permit: Option<SyncSender<bool>>,
    done: Option<Receiver<Result<M>>>,
}

pub(crate) fn validate_path(path: &str) -> Result<()> {
    ensure!(
        !path.trim().is_empty()
            && path.len() <= 32_768
            && !path.contains('\0')
            && Path::new(path).file_name().is_some()
            && Path::new(path)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("png")),
        "PNG export needs a nonblank .png path of at most 32768 bytes without NUL"
    );
    Ok(())
}

impl ExportJob<ExportInfo> {
    pub(crate) fn start(frame: ExportFrame, path: String) -> Result<Self> {
        // Preserve validation before accepting a completed magnifier frame.
        validate_path(&path)?;
        validate_pixels(frame.width, frame.height, &frame.rgba)?;
        let metadata = ExportInfo {
            user: frame.user,
            path: path.clone(),
            width: frame.width,
            height: frame.height,
        };
        Self::start_prepared(path, move || {
            Ok(Prepared {
                width: frame.width,
                height: frame.height,
                rgba: frame.rgba,
                metadata,
            })
        })
    }
}

impl<M: Clone + Send + 'static> ExportJob<M> {
    pub(crate) fn start_prepared(
        path: String,
        prepare: impl FnOnce() -> Result<Prepared<M>> + Send + 'static,
    ) -> Result<Self> {
        Self::start_with_permit_timeout(path, prepare, Duration::from_secs(5))
    }

    pub(crate) fn start_annotation_prepared(
        path: String,
        prepare: impl FnOnce() -> Result<Prepared<M>> + Send + 'static,
    ) -> Result<Self> {
        Self::start_with_permit_timeout(path, prepare, Duration::from_secs(9))
    }

    fn start_with_permit_timeout(
        path: String,
        prepare: impl FnOnce() -> Result<Prepared<M>> + Send + 'static,
        permit_timeout: Duration,
    ) -> Result<Self> {
        validate_path(&path)?;
        let destination = if Path::new(&path).is_absolute() {
            PathBuf::from(&path)
        } else {
            std::env::current_dir()?.join(&path)
        };
        let (prepared, ready) = mpsc::sync_channel(1);
        let (permit, decision) = mpsc::sync_channel(1);
        let (finished, done) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("agent-png-export".into())
            .spawn(move || {
                let encoded = (|| -> Result<(Vec<u8>, M)> {
                    let frame = prepare()?;
                    validate_pixels(frame.width, frame.height, &frame.rgba)?;
                    let mut output = LimitedPng(Vec::new());
                    image::codecs::png::PngEncoder::new(&mut output)
                        .write_image(
                            &frame.rgba,
                            frame.width,
                            frame.height,
                            image::ExtendedColorType::Rgba8,
                        )
                        .context("Encode PNG")?;
                    Ok((output.0, frame.metadata))
                })();
                let result = (|| -> Result<M> {
                    let (png, metadata) = match encoded {
                        Ok(prepared) => prepared,
                        Err(error) => {
                            let _ = prepared.send(Err(anyhow::anyhow!("{error:#}")));
                            return Err(error);
                        }
                    };
                    // Stamp actual preparation completion, independently of when the
                    // input thread next gets time to observe this ready message.
                    let authorization_metadata = metadata.clone();
                    let prepared_at = Instant::now();
                    prepared
                        .send(Ok((prepared_at, authorization_metadata)))
                        .map_err(|_| anyhow::anyhow!("Export requester disconnected"))?;
                    ensure!(
                        decision.recv_timeout(permit_timeout).unwrap_or(false),
                        "PNG export was cancelled before commit"
                    );
                    // Approval is the commit point. All filesystem writes stay here;
                    // later revocation cannot undo an already authorized publication.
                    write_atomic(&destination, &png)?;
                    Ok(metadata)
                })();
                let _ = finished.send(result);
            })?;
        Ok(Self {
            ready: Some(ready),
            permit: Some(permit),
            done: Some(done),
        })
    }

    #[cfg(test)]
    pub(crate) fn ready_fixture(
        prepared_at: Instant,
        metadata: M,
    ) -> (Self, Receiver<bool>, SyncSender<Result<M>>) {
        let (prepared, ready) = mpsc::sync_channel(1);
        let (permit, decision) = mpsc::sync_channel(1);
        let (finished, done) = mpsc::sync_channel(1);
        assert!(prepared.send(Ok((prepared_at, metadata))).is_ok());
        (
            Self {
                ready: Some(ready),
                permit: Some(permit),
                done: Some(done),
            },
            decision,
            finished,
        )
    }

    pub(crate) fn poll_ready(&mut self) -> Option<Result<M>> {
        self.poll_ready_at()
            .map(|ready| ready.map(|(_, metadata)| metadata))
    }

    pub(crate) fn poll_ready_at(&mut self) -> Option<Result<(Instant, M)>> {
        poll(&mut self.ready)
    }

    pub(crate) fn approve(&mut self, allowed: bool) -> Result<()> {
        self.permit
            .take()
            .context("Export commit was already decided")?
            .send(allowed)
            .context("Export worker stopped before commit")
    }

    pub(crate) fn poll_done(&mut self) -> Option<Result<M>> {
        poll(&mut self.done)
    }
}

fn validate_pixels(width: u32, height: u32, rgba: &[u8]) -> Result<()> {
    let pixels = u64::from(width) * u64::from(height);
    ensure!(
        pixels > 0 && pixels <= mux_core::magnifier::MAX_PIXELS && rgba.len() as u64 == pixels * 4,
        "Invalid PNG export frame"
    );
    Ok(())
}

fn poll<T>(receiver: &mut Option<Receiver<Result<T>>>) -> Option<Result<T>> {
    match receiver.as_ref()?.try_recv() {
        Ok(value) => {
            *receiver = None;
            Some(value)
        }
        Err(mpsc::TryRecvError::Empty) => None,
        Err(mpsc::TryRecvError::Disconnected) => {
            *receiver = None;
            Some(Err(anyhow::anyhow!("PNG export worker exited")))
        }
    }
}

struct LimitedPng(Vec<u8>);
impl Write for LimitedPng {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_PNG_BYTES.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other(
                "PNG exceeds its pixel-derived byte budget",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct TempFile(PathBuf);
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn write_atomic(destination: &Path, png: &[u8]) -> Result<()> {
    let name = destination
        .file_name()
        .context("Export destination needs a filename")?;
    let parent = destination
        .parent()
        .context("Export destination needs a parent")?;
    let mut created = None;
    for _ in 0..32 {
        let mut temporary = name.to_os_string();
        temporary.push(format!(
            ".mux-export.{}.{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let path = parent.join(temporary);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                created = Some((TempFile(path), file));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("Create PNG export temporary file"),
        }
    }
    let (temporary, mut file) = created.context("Cannot allocate PNG export temporary file")?;
    file.write_all(png).context("Write PNG export")?;
    file.sync_all().context("Flush PNG export")?;
    drop(file);
    use std::os::windows::ffi::OsStrExt;
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
        bail!("Publish PNG: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::Arc,
        time::{Instant, SystemTime, UNIX_EPOCH},
    };

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

    fn fixture() -> (ExportFrame, PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "mux-export-{}-{}-{}.png",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        (
            ExportFrame {
                user: 3,
                generation: 7,
                width: 2,
                height: 2,
                rgba: Arc::new([24, 80, 160, 255].repeat(4)),
            },
            path,
        )
    }
    fn wait<T>(mut poller: impl FnMut() -> Option<Result<T>>) -> Result<T> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = poller() {
                return result;
            }
            assert!(Instant::now() < deadline, "export test worker timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    #[test]
    fn malformed_paths_are_rejected_before_preparation() {
        for path in [
            "".to_string(),
            "  ".into(),
            "output.jpg".into(),
            "bad\0.png".into(),
            format!("{}.png", "x".repeat(32_768)),
        ] {
            let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let observed = ran.clone();
            let result = ExportJob::start_prepared(path, move || {
                observed.store(true, Ordering::Release);
                Ok(Prepared {
                    width: 1,
                    height: 1,
                    rgba: Arc::new(vec![0; 4]),
                    metadata: 1_u64,
                })
            });
            assert!(result.is_err());
            assert!(!ran.load(Ordering::Acquire));
        }
    }

    #[test]
    fn typed_metadata_is_ready_before_commit_and_matches_published_pixels() {
        #[derive(Clone, Debug, PartialEq, Eq)]
        struct InkMetadata {
            user: usize,
            revision: u64,
        }
        let (_, path) = fixture();
        let expected = InkMetadata {
            user: 2,
            revision: 37,
        };
        let metadata = expected.clone();
        let pixels = vec![255, 0, 0, 128, 0, 0, 0, 0];
        let rgba = pixels.clone();
        let mut job = ExportJob::start_prepared(path.to_string_lossy().into_owned(), move || {
            Ok(Prepared {
                width: 2,
                height: 1,
                rgba: Arc::new(rgba),
                metadata,
            })
        })
        .unwrap();
        assert_eq!(wait(|| job.poll_ready()).unwrap(), expected);
        assert!(!path.exists());
        job.approve(true).unwrap();
        assert_eq!(wait(|| job.poll_done()).unwrap(), expected);
        let image = image::open(&path).unwrap().to_rgba8();
        assert_eq!(image.dimensions(), (2, 1));
        assert_eq!(image.into_raw(), pixels);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn invalid_prepared_pixels_never_offer_a_commit_or_touch_the_destination() {
        let (_, path) = fixture();
        fs::write(&path, b"prior").unwrap();
        let mut job = ExportJob::start_prepared(path.to_string_lossy().into_owned(), || {
            Ok(Prepared {
                width: 2,
                height: 1,
                rgba: Arc::new(vec![0; 4]),
                metadata: 1_u64,
            })
        })
        .unwrap();
        assert!(wait(|| job.poll_ready()).is_err());
        assert!(wait(|| job.poll_done()).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"prior");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn preparation_and_denial_preserve_existing_destination() {
        let (frame, path) = fixture();
        fs::write(&path, b"existing").unwrap();
        let mut job = ExportJob::start(frame, path.to_string_lossy().into_owned()).unwrap();
        wait(|| job.poll_ready()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"existing");
        job.approve(false).unwrap();
        assert!(wait(|| job.poll_done()).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"existing");
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn dropping_uncommitted_request_stops_worker_without_creating_destination() {
        let (frame, path) = fixture();
        let mut job = ExportJob::start(frame, path.to_string_lossy().into_owned()).unwrap();
        wait(|| job.poll_ready()).unwrap();
        let completed = job.done.take().unwrap();
        drop(job);
        assert!(
            completed
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .is_err()
        );
        assert!(!path.exists());
    }
    #[test]
    fn approved_export_atomically_replaces_destination_with_decodable_pixels() {
        let (frame, path) = fixture();
        fs::write(&path, b"prior").unwrap();
        let mut job = ExportJob::start(frame, path.to_string_lossy().into_owned()).unwrap();
        wait(|| job.poll_ready()).unwrap();
        job.approve(true).unwrap();
        let exported = wait(|| job.poll_done()).unwrap();
        assert_eq!(exported.user, 3);
        let image = image::open(&path).unwrap().to_rgba8();
        assert_eq!(image.dimensions(), (2, 2));
        assert!(image.pixels().all(|pixel| pixel.0 == [24, 80, 160, 255]));
        fs::remove_file(path).unwrap();
    }
}
