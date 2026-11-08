//! Explicitly enabled, finite evidence from this overlay's own final cursor raster.
//! This feature is synchronous acceptance instrumentation, never a capture API.
#![cfg(feature = "cursor-presentation-evidence")]

use crate::{
    cursor_presenter::{DibCanvas, PresentationStatus, Presenter},
    overlay::OverlayStatus,
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    marker::PhantomData,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::HWND,
    System::Threading::{GetCurrentProcessId, GetCurrentThreadId},
    UI::WindowsAndMessaging::IsWindow,
};

const OVERLAY_RECORDS: u64 = 32;
const OVERLAY_BYTES: u64 = 128 * 1024 * 1024;
const PROCESS_RECORDS: u64 = 96;
const PROCESS_BYTES: u64 = 256 * 1024 * 1024;
const METADATA_BYTES: u64 = 16 * 1024;
const CONTROL_BYTES: u64 = 64 * 1024;
const LEDGER_BYTES: u64 = 48 * 1024;

#[derive(Default, Clone, Copy)]
struct Budget {
    records: u64,
    bytes: u64,
}
impl Budget {
    fn next(self, records: u64, bytes: u64, max_records: u64, max_bytes: u64) -> Option<Self> {
        let next = Self {
            records: self.records.checked_add(records)?,
            bytes: self.bytes.checked_add(bytes)?,
        };
        (next.records <= max_records && next.bytes <= max_bytes).then_some(next)
    }
}
static PROCESS_BUDGET: Mutex<Budget> = Mutex::new(Budget {
    records: 0,
    bytes: 0,
});
static NEXT_OVERLAY: AtomicU64 = AtomicU64::new(1);
static RUN: OnceLock<std::result::Result<Run, String>> = OnceLock::new();
struct Run {
    id: String,
    directory: PathBuf,
    pid: u32,
}

fn process_reserve(records: u64, bytes: u64) -> Result<()> {
    reserve_shared(&PROCESS_BUDGET, records, bytes)
}
fn reserve_shared(shared: &Mutex<Budget>, records: u64, bytes: u64) -> Result<()> {
    let mut budget = shared
        .lock()
        .map_err(|_| anyhow::anyhow!("evidence budget mutex poisoned"))?;
    *budget = budget
        .next(records, bytes, PROCESS_RECORDS, PROCESS_BYTES)
        .context("whole-process evidence cap")?;
    Ok(())
}

fn new_file(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

struct LimitedBytes {
    bytes: Vec<u8>,
    limit: usize,
}
impl LimitedBytes {
    fn new(limit: u64) -> Self {
        let limit = usize::try_from(limit).expect("small fixed metadata budget");
        Self {
            bytes: Vec::with_capacity(limit),
            limit,
        }
    }
}
impl Write for LimitedBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let end = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .filter(|end| *end <= self.limit)
            .ok_or_else(|| io::Error::other("evidence JSON exceeds reserved budget"))?;
        self.bytes.extend_from_slice(bytes);
        debug_assert_eq!(self.bytes.len(), end);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn json_bytes(value: &impl Serialize, limit: u64) -> Result<Vec<u8>> {
    let mut writer = LimitedBytes::new(limit);
    serde_json::to_writer(&mut writer, value)?;
    writer.write_all(b"\n")?;
    Ok(writer.bytes)
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = new_file(path).with_context(|| format!("create new {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("sync {}", path.display()))?;
    Ok(())
}

fn initialize_run(directory: PathBuf) -> Result<Run> {
    ensure!(
        directory.is_absolute() && directory.is_dir(),
        "evidence directory must be absolute and already created by the harness"
    );
    process_reserve(0, CONTROL_BYTES)?;
    let pid = unsafe { GetCurrentProcessId() };
    let epoch = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let id = format!("p{pid}-t{epoch}");
    let run = Run { id, directory, pid };
    let value = serde_json::json!({ "schema": 1, "type": "run", "run_id": run.id, "pid": pid,
        "capture_scope": "owned_cursor_final_raster_only", "process_record_cap": PROCESS_RECORDS,
        "process_byte_cap": PROCESS_BYTES, "overlay_record_cap": OVERLAY_RECORDS,
        "overlay_byte_cap": OVERLAY_BYTES, "performance_binary": false });
    write_new(
        &run.directory.join(format!("{}-run.json", run.id)),
        &json_bytes(&value, CONTROL_BYTES)?,
    )?;
    Ok(run)
}

#[derive(Clone, Copy)]
struct Reservation {
    generation: u64,
    size: (i32, i32),
    gpu: bool,
    sequence: u64,
}

/// Never Send/Sync: initialization, GPU reads, files, and cleanup belong to the HWND owner.
pub(crate) struct Recorder {
    directory: PathBuf,
    run_id: String,
    pid: u32,
    tid: u32,
    hwnd: usize,
    overlay_id: u64,
    budget: Budget,
    pending: Option<Reservation>,
    last: Option<(u64, bool)>,
    records_completed: u64,
    errors: u64,
    cached_skips: u64,
    cap_skips: u64,
    blocked: bool,
    ledger: File,
    ledger_bytes: u64,
    ledger_failed: bool,
    _owner: PhantomData<Rc<()>>,
}

impl Recorder {
    /// No environment value means no storage, GPU instrumentation, wait, or I/O.
    pub(crate) fn from_environment(hwnd: HWND) -> Option<Self> {
        let directory = std::env::var_os("PROTOINPUTMULTIPLEX_GPU_CURSOR_EVIDENCE_DIR")?;
        match Self::open(hwnd, PathBuf::from(directory)) {
            Ok(recorder) => Some(recorder),
            Err(error) => {
                eprintln!("cursor presentation evidence initialization failed: {error:#}");
                None
            }
        }
    }

    fn open(hwnd: HWND, directory: PathBuf) -> Result<Self> {
        let run = RUN
            .get_or_init(|| initialize_run(directory.clone()).map_err(|error| format!("{error:#}")))
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        ensure!(
            directory == run.directory,
            "evidence directory changed within process"
        );
        let overlay_id = NEXT_OVERLAY
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| anyhow::anyhow!("overlay identity exhausted"))?;
        process_reserve(0, CONTROL_BYTES)?;
        let ledger = new_file(&directory.join(format!("{}-o{overlay_id}-ledger.jsonl", run.id)))?;
        let mut recorder = Self {
            directory,
            run_id: run.id.clone(),
            pid: run.pid,
            tid: unsafe { GetCurrentThreadId() },
            hwnd: hwnd as usize,
            overlay_id,
            budget: Budget {
                records: 0,
                bytes: CONTROL_BYTES,
            },
            pending: None,
            last: None,
            records_completed: 0,
            errors: 0,
            cached_skips: 0,
            cap_skips: 0,
            blocked: false,
            ledger,
            ledger_bytes: 0,
            ledger_failed: false,
            _owner: PhantomData,
        };
        recorder.ledger_event(serde_json::json!({ "type":"overlay_start", "run_id":recorder.run_id,
            "overlay_id":overlay_id, "pid":recorder.pid, "tid":recorder.tid, "hwnd":recorder.hwnd }));
        Ok(recorder)
    }

    fn reserve(&mut self, generation: u64, size: (i32, i32), gpu: bool) -> Result<Reservation> {
        let bytes = raster_bytes(size)?;
        // Charge both metadata names even where the final name is a hard link.
        let total = bytes
            .checked_mul(if gpu { 2 } else { 1 })
            .and_then(|bytes| bytes.checked_add(METADATA_BYTES * 2))
            .context("evidence reservation arithmetic overflow")?;
        let local = self
            .budget
            .next(1, total, OVERLAY_RECORDS, OVERLAY_BYTES)
            .context("per-overlay evidence cap")?;
        // Reserve atomically for all workers. Reservations are never refunded:
        // partial files and failed attempts remain charged and reviewable.
        process_reserve(1, total)?;
        self.budget = local;
        Ok(Reservation {
            generation,
            size,
            gpu,
            sequence: local.records,
        })
    }

    /// Called before a changed generation's upload. False must disable its staging copy.
    pub(crate) fn prepare(
        &mut self,
        generation: u64,
        size: (i32, i32),
        gpu_expected: bool,
    ) -> bool {
        if self.blocked || self.last == Some((generation, gpu_expected)) {
            return false;
        }
        if self.pending.is_some() {
            self.failure(
                "prepare",
                generation,
                "previous reserved generation was not recorded",
            );
            self.pending = None;
        }
        match self.reserve(generation, size, gpu_expected) {
            Ok(reservation) => {
                self.pending = Some(reservation);
                gpu_expected
            }
            Err(error) => {
                self.blocked = true;
                self.cap_skips = self.cap_skips.saturating_add(1);
                self.failure("reserve", generation, &format!("{error:#}"));
                false
            }
        }
    }

    /// Diagnostic-only: never return an error into routing or presenter fallback.
    pub(crate) fn record(
        &mut self,
        presenter: &Presenter,
        canvas: Option<&DibCanvas>,
        status: &OverlayStatus,
    ) {
        let presentation = presenter.status();
        let gpu = match presentation.backend.as_str() {
            "direct_composition_d3d11" => true,
            "gdi" => false,
            _ => {
                self.failure(
                    "backend",
                    status.paint_generation,
                    "no successful presentation backend",
                );
                return;
            }
        };
        if self.last == Some((status.paint_generation, gpu)) {
            self.cached_skips = self.cached_skips.saturating_add(1);
            return;
        }
        if self.blocked {
            self.cap_skips = self.cap_skips.saturating_add(1);
            return;
        }
        self.last = Some((status.paint_generation, gpu));
        let result = (|| -> Result<()> {
            ensure!(
                unsafe { GetCurrentThreadId() } == self.tid && status.hwnd == self.hwnd,
                "evidence owner/HWND mismatch"
            );
            ensure!(
                status.paint_generation > 0
                    && canvas.is_none_or(|canvas| status.canvas_size == canvas.size),
                "evidence generation/extent mismatch"
            );
            let reservation = match self.pending.take() {
                Some(reservation) => {
                    ensure!(
                        reservation.generation == status.paint_generation
                            && reservation.size == status.canvas_size,
                        "reserved generation/extent mismatch"
                    );
                    ensure!(
                        !gpu || reservation.gpu,
                        "GPU presentation was not reserved before copy"
                    );
                    reservation
                }
                // A cached GPU health failure may switch to GDI without repaint.
                None if !gpu => self.reserve(status.paint_generation, status.canvas_size, false)?,
                None => anyhow::bail!("GPU presentation has no pre-copy reservation"),
            };
            self.store(presenter, canvas, status, &presentation, reservation, gpu)
        })();
        match result {
            Ok(()) => self.records_completed += 1,
            Err(error) => self.failure("record", status.paint_generation, &format!("{error:#}")),
        }
    }

    fn store(
        &mut self,
        presenter: &Presenter,
        canvas: Option<&DibCanvas>,
        status: &OverlayStatus,
        presentation: &PresentationStatus,
        reservation: Reservation,
        gpu: bool,
    ) -> Result<()> {
        let bytes = raster_bytes(status.canvas_size)?;
        let byte_len = usize::try_from(bytes)?;
        let stem = format!(
            "{}-o{}-r{}-g{}-{}",
            self.run_id,
            self.overlay_id,
            reservation.sequence,
            status.paint_generation,
            if gpu { "gpu" } else { "gdi" }
        );
        // GPU artwork has no CPU reference canvas. Optional WM_PRINT replay is
        // a separate compatibility rendering, never a GPU parity oracle.
        let source = if presentation.raster_backend
            == crate::cursor_presenter::RasterBackend::Direct2D
        {
            None
        } else {
            let canvas = canvas.context("missing GDI source raster")?;
            ensure!(!canvas.bits.is_null(), "missing CPU source raster");
            let packed = unsafe { std::slice::from_raw_parts(canvas.bits.cast::<u8>(), byte_len) };
            let name = format!("{stem}-source.bgra");
            write_new(&self.directory.join(&name), packed)?;
            Some(PixelFile {
                file: name,
                bytes,
                digest_fnv1a64: digest(packed),
                provenance: "current_worker_final_cpu_raster",
            })
        };
        let (gpu_file, present_count, composition_frame_time) = if gpu {
            let (pixels, present_count, composition_frame_time) =
                presenter.readback_for_generation(status.paint_generation)?;
            ensure!(
                pixels.len().checked_mul(4) == Some(byte_len),
                "GPU readback size mismatch"
            );
            ensure!(
                present_count > 0 && composition_frame_time > 0,
                "GPU presentation acknowledgement missing"
            );
            let packed =
                unsafe { std::slice::from_raw_parts(pixels.as_ptr().cast::<u8>(), byte_len) };
            let name = format!("{stem}-gpu-readback.bgra");
            write_new(&self.directory.join(&name), packed)?;
            (
                Some(PixelFile {
                    file: name,
                    bytes,
                    digest_fnv1a64: digest(packed),
                    provenance: "same_generation_pre_present_gpu_staging_after_commit_ack",
                }),
                Some(present_count),
                Some(composition_frame_time),
            )
        } else {
            (None, None, None)
        };
        let receipt = Receipt {
            schema: 2,
            complete: true,
            run_id: &self.run_id,
            overlay_id: self.overlay_id,
            pid: self.pid,
            tid: self.tid,
            hwnd: self.hwnd,
            sequence: reservation.sequence,
            generation: status.paint_generation,
            width: status.canvas_size.0,
            height: status.canvas_size.1,
            format: "BGRA8_UNORM",
            alpha: "premultiplied",
            row_packing: "tightly_packed_top_down",
            source,
            gpu: gpu_file,
            present_count,
            composition_frame_time,
            presentation,
            overlay: status,
            limitation: "same-generation final raster and acknowledged owned presentation; GPU artwork has no CPU-parity source; no physical scanout or independent layout proof",
        };
        // JSON is the commit receipt. Raw files without it are incomplete partials.
        let metadata = json_bytes(&receipt, METADATA_BYTES)?;
        let partial = self.directory.join(format!("{stem}-receipt.partial.json"));
        write_new(&partial, &metadata)?;
        // A hard link publishes the already closed/synced receipt atomically and
        // refuses an existing destination. No rename-with-overwrite is used.
        // Unsupported filesystems fail evidence collection, never presentation.
        std::fs::hard_link(
            &partial,
            self.directory.join(format!("{stem}-complete.json")),
        )
        .context("publish immutable completed receipt without overwrite")?;
        self.ledger_event(
            serde_json::json!({ "type":"record_complete", "sequence":reservation.sequence,
            "generation":status.paint_generation, "backend":presentation.backend }),
        );
        Ok(())
    }

    fn failure(&mut self, phase: &str, generation: u64, message: &str) {
        self.errors = self.errors.saturating_add(1);
        let message: String = message.chars().take(512).collect();
        eprintln!(
            "cursor presentation evidence failed: run={} overlay={} generation={} phase={} error={}",
            self.run_id, self.overlay_id, generation, phase, message
        );
        self.ledger_event(serde_json::json!({ "type":"error", "phase":phase, "generation":generation, "message":message }));
    }

    fn ledger_event(&mut self, value: serde_json::Value) {
        if self.ledger_failed {
            return;
        }
        let result = (|| -> Result<()> {
            let bytes = json_bytes(&value, 4096)?;
            let next = self
                .ledger_bytes
                .checked_add(bytes.len().try_into()?)
                .context("ledger arithmetic overflow")?;
            ensure!(next <= LEDGER_BYTES, "bounded evidence ledger full");
            self.ledger.write_all(&bytes)?;
            self.ledger.flush()?;
            self.ledger_bytes = next;
            Ok(())
        })();
        if let Err(error) = result {
            self.ledger_failed = true;
            self.errors = self.errors.saturating_add(1);
            eprintln!(
                "cursor presentation evidence ledger failed: run={} overlay={} error={error:#}",
                self.run_id, self.overlay_id
            );
        }
    }
}

#[derive(Serialize)]
struct PixelFile {
    file: String,
    bytes: u64,
    digest_fnv1a64: String,
    provenance: &'static str,
}
#[derive(Serialize)]
struct Receipt<'a> {
    schema: u32,
    complete: bool,
    run_id: &'a str,
    overlay_id: u64,
    pid: u32,
    tid: u32,
    hwnd: usize,
    sequence: u64,
    generation: u64,
    width: i32,
    height: i32,
    format: &'static str,
    alpha: &'static str,
    row_packing: &'static str,
    source: Option<PixelFile>,
    gpu: Option<PixelFile>,
    present_count: Option<u32>,
    composition_frame_time: Option<i64>,
    presentation: &'a PresentationStatus,
    overlay: &'a OverlayStatus,
    limitation: &'static str,
}
fn raster_bytes(size: (i32, i32)) -> Result<u64> {
    ensure!(
        (1..=1024).contains(&size.0) && (1..=1024).contains(&size.1),
        "invalid bounded cursor extent"
    );
    u64::try_from(size.0)?
        .checked_mul(u64::try_from(size.1)?)
        .and_then(|pixels| pixels.checked_mul(4))
        .context("raster byte arithmetic overflow")
}
fn digest(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    });
    format!("{hash:016x}")
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let cleanup = serde_json::json!({ "schema":1, "type":"overlay_cleanup", "run_id":self.run_id,
            "overlay_id":self.overlay_id, "pid":self.pid, "tid":self.tid, "hwnd":self.hwnd,
            "owner_thread":unsafe { GetCurrentThreadId() } == self.tid,
            "hwnd_still_alive":unsafe { IsWindow(self.hwnd as HWND) } != 0,
            "reserved_records":self.budget.records, "reserved_bytes":self.budget.bytes,
            "records_completed":self.records_completed, "errors":self.errors,
            "cached_metadata_skips":self.cached_skips, "cap_skips":self.cap_skips,
            "cap_blocked":self.blocked, "unrecorded_reservation":self.pending.is_some(),
            "ledger_failed":self.ledger_failed });
        let result = (|| -> Result<()> {
            self.ledger.sync_all()?;
            let bytes = json_bytes(&cleanup, CONTROL_BYTES - LEDGER_BYTES)?;
            write_new(
                &self
                    .directory
                    .join(format!("{}-o{}-cleanup.json", self.run_id, self.overlay_id)),
                &bytes,
            )
        })();
        if let Err(error) = result {
            eprintln!(
                "cursor presentation evidence cleanup receipt failed: run={} overlay={} error={error:#}",
                self.run_id, self.overlay_id
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reservations_are_checked_and_bounded() {
        let budget = Budget {
            records: 31,
            bytes: OVERLAY_BYTES - 20,
        };
        assert!(budget.next(1, 20, OVERLAY_RECORDS, OVERLAY_BYTES).is_some());
        assert!(budget.next(2, 0, OVERLAY_RECORDS, OVERLAY_BYTES).is_none());
        assert!(budget.next(1, 21, OVERLAY_RECORDS, OVERLAY_BYTES).is_none());
        assert!(
            Budget {
                records: u64::MAX,
                bytes: 0
            }
            .next(1, 0, u64::MAX, u64::MAX)
            .is_none()
        );
        assert!(
            Budget {
                records: 0,
                bytes: u64::MAX
            }
            .next(0, 1, u64::MAX, u64::MAX)
            .is_none()
        );
        assert_eq!(raster_bytes((1024, 1024)).unwrap(), 4 * 1024 * 1024);
        assert!(raster_bytes((0, 1024)).is_err());
    }
    #[test]
    fn json_and_digest_contracts_are_bounded() {
        assert_eq!(digest(b""), "cbf29ce484222325");
        assert_eq!(digest(b"a"), "af63dc4c8601ec8c");
        assert!(json_bytes(&"abc", 3).is_err());
        assert_eq!(json_bytes(&"abc", 6).unwrap(), b"\"abc\"\n");
    }

    #[test]
    fn whole_process_reservations_remain_atomic_between_workers() {
        let budget = std::sync::Arc::new(Mutex::new(Budget::default()));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let budget = budget.clone();
                std::thread::spawn(move || {
                    (0..100)
                        .filter(|_| reserve_shared(&budget, 1, 1).is_ok())
                        .count()
                })
            })
            .collect();
        assert_eq!(
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .sum::<usize>(),
            96
        );
        let budget = budget.lock().unwrap();
        assert_eq!((budget.records, budget.bytes), (96, 96));
    }
}
