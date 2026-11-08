//! RETIRED historical CPU-upload benchmark source; no longer compiled.
//! Superseded by `owned_gpu_rasterization_cache_fallback_and_window_contract`
//! in `overlay_native_acceptance.rs`. The original instructions/oracle below
//! describe historical baseline work, not current GPU acceptance.
//!
//! Finite, release-only overlay comparison harness.
//!
//! This module is intentionally not linked into production builds. Register it as a child
//! module of `overlay.rs` with `#[cfg(test)] #[path = "overlay_integration_benchmark.rs"]`
//! in both the 04658a2 GDI baseline extract and the integrated candidate. Keep the workload
//! body identical; only `presentation_snapshot` differs between source variants. The GDI
//! baseline starts at 04658a2, with only lifecycle.rs repinned from
//! ee241610f433541e7de0c4963265d14b844f0c2b.

use super::Overlay;
use anyhow::{Context, Result, ensure};
use mux_core::desktop::{CursorSettings, CursorShape};
use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

const TRIALS: usize = 3;
const WARMUP_DRAWS: usize = 10;
const CACHED_DRAWS: usize = 1_000;
const CHANGED_NORMAL_DRAWS: usize = 64;
const ALTERNATING_RESIZE_DRAWS: usize = 32;
const MAXIMUM_DRAWS: usize = 8;
const PREMULTIPLIED_CUSTOM_DRAWS: usize = 64;

// The baseline does not declare this optional feature. Its evidence build is explicitly
// feature-off; suppress only Cargo's check-cfg warning while reading the real build value.
#[allow(unexpected_cfgs)]
const PRESENTATION_EVIDENCE_ENABLED: bool = cfg!(feature = "cursor-presentation-evidence");

#[derive(Clone, Copy)]
struct RasterStatus {
    generation: u64,
    pixel_hash: u64,
    canvas_size: (i32, i32),
    hotspot: (i32, i32),
    painted_pixels: usize,
}

#[derive(Clone)]
struct BackendSnapshot {
    backend: String,
    gpu_uploads: Option<u64>,
    gpu_presents: Option<u64>,
    composition_commits: Option<u64>,
}

struct Sample {
    trial: usize,
    scenario: &'static str,
    index: usize,
    draw_ns: u128,
    status: RasterStatus,
    backend: BackendSnapshot,
}

/// Shared body used by both variants. The baseline adapter returns GDI with absent GPU
/// counters; the candidate adapter reads the live backend and presentation counters.
fn run_with_backend_adapter(actual_backend: impl Fn(&Overlay) -> BackendSnapshot) -> Result<()> {
    let output = std::env::var_os("PROTOINPUTMULTIPLEX_OVERLAY_BENCH_OUTPUT")
        .context("set PROTOINPUTMULTIPLEX_OVERLAY_BENCH_OUTPUT to a fresh output directory")?;
    let expected_backend = std::env::var("PROTOINPUTMULTIPLEX_OVERLAY_BENCH_EXPECT_BACKEND").context(
        "set PROTOINPUTMULTIPLEX_OVERLAY_BENCH_EXPECT_BACKEND to gdi or direct_composition_d3d11",
    )?;
    let output = PathBuf::from(output);
    fs::create_dir(&output).context("create fresh benchmark output directory")?;

    let samples_path = output.join("samples.csv");
    let mut samples = Vec::with_capacity(
        TRIALS
            * (CACHED_DRAWS
                + CHANGED_NORMAL_DRAWS
                + ALTERNATING_RESIZE_DRAWS
                + MAXIMUM_DRAWS
                + PREMULTIPLIED_CUSTOM_DRAWS),
    );
    let mut startup_ns = Vec::with_capacity(TRIALS);
    let mut drop_ns = Vec::with_capacity(TRIALS);
    let mut observed_backends = Vec::with_capacity(TRIALS);

    for trial in 0..TRIALS {
        let startup = Instant::now();
        let mut overlay = Overlay::new(&format!("release benchmark trial {trial}"))?;
        startup_ns.push(startup.elapsed().as_nanos());
        let mut settings = CursorSettings {
            visible: true,
            label: true,
            badge: Some("U1".into()),
            shape: CursorShape::Arrow,
            ..Default::default()
        };

        // Warm the window, thread, and cached normal raster before collecting any samples.
        for index in 0..WARMUP_DRAWS {
            overlay.draw(
                400 + index as i32,
                300 + index as i32,
                "Benchmark user",
                &settings,
                false,
            )?;
        }
        let warmed_backend = actual_backend(&overlay);
        ensure!(
            warmed_backend.backend == expected_backend,
            "backend mismatch after warmup in trial {trial}: expected {expected_backend}, got {}",
            warmed_backend.backend
        );
        if expected_backend == "direct_composition_d3d11" {
            ensure!(
                warmed_backend.gpu_uploads.unwrap_or_default() > 0
                    && warmed_backend.gpu_presents.unwrap_or_default() > 0
                    && warmed_backend.composition_commits.unwrap_or_default() > 0,
                "GPU backend reported no successful upload, present, or composition commit after warmup"
            );
        }
        observed_backends.push(warmed_backend.backend);
        let cached_generation = overlay.status.paint_generation;
        let cached_hash = overlay.status.pixel_hash;
        ensure!(
            cached_hash != 0 && overlay.status.painted_pixels > 0,
            "warmup did not paint a cursor"
        );

        for index in 0..CACHED_DRAWS {
            let (draw_ns, status, backend) = timed_draw(
                &mut overlay,
                &actual_backend,
                &expected_backend,
                |overlay| {
                    overlay.draw(
                        500 + index as i32,
                        360 + index as i32,
                        "Benchmark user",
                        &settings,
                        false,
                    )
                },
            )?;
            ensure!(
                status.generation == cached_generation,
                "cached movement repainted at sample {index}"
            );
            ensure!(
                status.pixel_hash == cached_hash,
                "cached movement changed raster at sample {index}"
            );
            samples.push(Sample {
                trial,
                scenario: "cached",
                index,
                draw_ns,
                status,
                backend,
            });
        }

        let mut normal_generation = overlay.status.paint_generation;
        let mut normal_hash = overlay.status.pixel_hash;
        for index in 0..CHANGED_NORMAL_DRAWS {
            settings.color = [
                (31 + index * 3) as u8,
                (91 + index * 5) as u8,
                (151 + index * 7) as u8,
            ];
            let sample = draw_sample(
                &mut overlay,
                trial,
                "changed_normal",
                index,
                &actual_backend,
                &expected_backend,
                |overlay| overlay.draw(600 + index as i32, 400, "Benchmark user", &settings, false),
            )?;
            ensure!(
                sample.status.generation > normal_generation
                    && sample.status.pixel_hash != normal_hash,
                "changed-normal workload did not produce a new raster at sample {index}"
            );
            normal_generation = sample.status.generation;
            normal_hash = sample.status.pixel_hash;
            samples.push(sample);
        }

        let mut resize_sizes = Vec::with_capacity(ALTERNATING_RESIZE_DRAWS);
        let mut resize_generations = Vec::with_capacity(ALTERNATING_RESIZE_DRAWS);
        for index in 0..ALTERNATING_RESIZE_DRAWS {
            settings.size = if index % 2 == 0 { 12.0 } else { 96.0 };
            let sample = draw_sample(
                &mut overlay,
                trial,
                "alternating_resize",
                index,
                &actual_backend,
                &expected_backend,
                |overlay| overlay.draw(700, 420, "Resize benchmark", &settings, false),
            )?;
            resize_sizes.push(sample.status.canvas_size);
            resize_generations.push(sample.status.generation);
            samples.push(sample);
        }
        ensure!(
            resize_sizes
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == 2,
            "alternating resize workload did not produce two canvas sizes"
        );
        ensure!(
            resize_generations.windows(2).all(|pair| pair[1] > pair[0]),
            "alternating resize workload did not repaint each changed raster"
        );

        settings.size = 256.0;
        settings.color = [246, 214, 38];
        settings.badge = Some("MAXIMUM8".into());
        let maximum_name =
            "Maximum-size benchmark cursor with a deliberately long label for the bounded layout";
        let mut maximum_status = None;
        for index in 0..MAXIMUM_DRAWS {
            let sample = draw_sample(
                &mut overlay,
                trial,
                "maximum",
                index,
                &actual_backend,
                &expected_backend,
                |overlay| overlay.draw(800 + index as i32, 450, maximum_name, &settings, false),
            )?;
            ensure!(
                sample.status.canvas_size.0 <= 1024
                    && sample.status.canvas_size.1 <= 1024
                    && sample.status.painted_pixels > 0,
                "maximum-size sample exceeded the cursor canvas bounds or painted nothing"
            );
            if let Some((generation, pixel_hash, canvas_size)) = maximum_status {
                ensure!(
                    sample.status.generation == generation
                        && sample.status.pixel_hash == pixel_hash
                        && sample.status.canvas_size == canvas_size,
                    "cached maximum-size cursor changed its raster"
                );
            } else {
                maximum_status = Some((
                    sample.status.generation,
                    sample.status.pixel_hash,
                    sample.status.canvas_size,
                ));
            }
            samples.push(sample);
        }

        settings.size = 48.0;
        settings.badge = Some("PX".into());
        let custom_image = premultiplied_custom_image();
        let custom_cursor = super::AppCursorObservation {
            state: "benchmark".into(),
            capability: true,
            sequence: 1,
            kind: 32512,
            visible: Some(true),
            image: Some(custom_image),
            ..Default::default()
        };
        let mut custom_generation = None;
        for index in 0..PREMULTIPLIED_CUSTOM_DRAWS {
            let sample = draw_sample(
                &mut overlay,
                trial,
                "premultiplied_custom",
                index,
                &actual_backend,
                &expected_backend,
                |overlay| {
                    overlay.draw_application(
                        (900 + index as i32, 510),
                        "Custom image",
                        &settings,
                        false,
                        Some(&custom_cursor),
                    )
                },
            )?;
            ensure!(
                sample.status.painted_pixels > 0,
                "custom cursor sample painted nothing"
            );
            ensure!(
                sample.status.hotspot == (5, 8),
                "custom cursor hotspot changed"
            );
            if index == 0 {
                custom_generation = Some(sample.status.generation);
            } else {
                ensure!(
                    Some(sample.status.generation) == custom_generation,
                    "cached custom cursor repainted at sample {index}"
                );
            }
            samples.push(sample);
        }

        ensure!(
            actual_backend(&overlay).backend == expected_backend,
            "presentation backend changed before drop in trial {trial}"
        );
        let drop_started = Instant::now();
        drop(overlay);
        drop_ns.push(drop_started.elapsed().as_nanos());
    }

    let observed_backend = observed_backends
        .into_iter()
        .next()
        .context("no benchmark trials ran")?;
    write_samples(&samples_path, &samples)?;
    let metadata_path = output.join("metadata.json");
    let metadata = serde_json::json!({
        "schema_version": 1,
        "expected_backend": expected_backend,
        "observed_backend": observed_backend,
        "instrumented_feature_enabled": PRESENTATION_EVIDENCE_ENABLED,
        "trials": TRIALS,
        "warmup_draws_per_process": TRIALS * WARMUP_DRAWS,
        "warmup_draws_per_trial": WARMUP_DRAWS,
        "draws_per_trial": CACHED_DRAWS + CHANGED_NORMAL_DRAWS + ALTERNATING_RESIZE_DRAWS + MAXIMUM_DRAWS + PREMULTIPLIED_CUSTOM_DRAWS,
        "draw_samples": samples.len(),
        "test_harness_cfg_test_branches_compiled": true,
        "cursor_presentation_evidence_feature_enabled": PRESENTATION_EVIDENCE_ENABLED,
        "presentation_counters_recorded_outside_draw_timing": true,
        "startup_ns": startup_ns,
        "drop_ns": drop_ns,
        "samples_file": "samples.csv",
        "scanout_or_photon_latency_measured": false,
    });
    fs::write(metadata_path, serde_json::to_vec_pretty(&metadata)?)
        .context("write benchmark metadata")?;
    println!("benchmark output: {}", output.display());
    println!(
        "backend: {observed_backend}; retained {} draw samples",
        samples.len()
    );
    Ok(())
}

fn timed_draw(
    overlay: &mut Overlay,
    actual_backend: &impl Fn(&Overlay) -> BackendSnapshot,
    expected_backend: &str,
    draw: impl FnOnce(&mut Overlay) -> Result<()>,
) -> Result<(u128, RasterStatus, BackendSnapshot)> {
    let start = Instant::now();
    draw(overlay)?;
    let draw_ns = start.elapsed().as_nanos();
    let status = snapshot(overlay);
    ensure!(
        status.canvas_size.0 > 0
            && status.canvas_size.1 > 0
            && status.hotspot.0 >= 0
            && status.hotspot.1 >= 0
            && status.hotspot.0 < status.canvas_size.0
            && status.hotspot.1 < status.canvas_size.1,
        "draw returned invalid canvas or hotspot status"
    );
    let backend = actual_backend(overlay);
    ensure!(
        backend.backend == expected_backend,
        "presentation backend changed during benchmark draw: expected {expected_backend}, got {}",
        backend.backend
    );
    Ok((draw_ns, status, backend))
}

fn draw_sample(
    overlay: &mut Overlay,
    trial: usize,
    scenario: &'static str,
    index: usize,
    actual_backend: &impl Fn(&Overlay) -> BackendSnapshot,
    expected_backend: &str,
    draw: impl FnOnce(&mut Overlay) -> Result<()>,
) -> Result<Sample> {
    let (draw_ns, status, backend) = timed_draw(overlay, actual_backend, expected_backend, draw)?;
    Ok(Sample {
        trial,
        scenario,
        index,
        draw_ns,
        status,
        backend,
    })
}

fn snapshot(overlay: &Overlay) -> RasterStatus {
    RasterStatus {
        generation: overlay.status.paint_generation,
        pixel_hash: overlay.status.pixel_hash,
        canvas_size: overlay.status.canvas_size,
        hotspot: overlay.status.hotspot,
        painted_pixels: overlay.status.painted_pixels,
    }
}

fn premultiplied_custom_image() -> super::AppCursorImage {
    const WIDTH: usize = 64;
    const HEIGHT: usize = 64;
    let mut pixels = Vec::with_capacity(WIDTH * HEIGHT * 4);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let alpha = if (x / 8 + y / 8) % 2 == 0 {
                192u8
            } else {
                112u8
            };
            let red = ((x * 3) as u8).min(alpha);
            let green = ((y * 3) as u8).min(alpha);
            let blue = ((x + y) as u8).min(alpha);
            pixels.extend_from_slice(&[blue, green, red, alpha]);
        }
    }
    super::AppCursorImage {
        width: WIDTH as u32,
        height: HEIGHT as u32,
        hotspot: (7, 11),
        pixels: Arc::from(pixels),
        identity: 0x504d_5542,
    }
}

fn write_samples(path: &Path, samples: &[Sample]) -> Result<()> {
    let file = File::create(path).context("create benchmark CSV")?;
    let mut writer = BufWriter::new(file);
    writeln!(
        writer,
        "trial,scenario,index,draw_ns,generation,pixel_hash,width,height,hotspot_x,hotspot_y,painted_pixels,gpu_uploads,gpu_presents,composition_commits"
    )?;
    for sample in samples {
        writeln!(
            writer,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            sample.trial,
            sample.scenario,
            sample.index,
            sample.draw_ns,
            sample.status.generation,
            sample.status.pixel_hash,
            sample.status.canvas_size.0,
            sample.status.canvas_size.1,
            sample.status.hotspot.0,
            sample.status.hotspot.1,
            sample.status.painted_pixels,
            optional_counter(sample.backend.gpu_uploads),
            optional_counter(sample.backend.gpu_presents),
            optional_counter(sample.backend.composition_commits),
        )?;
    }
    writer.flush().context("flush benchmark CSV")?;
    Ok(())
}

fn optional_counter(value: Option<u64>) -> String {
    value.map_or_else(String::new, |value| value.to_string())
}

fn presentation_snapshot(overlay: &Overlay) -> BackendSnapshot {
    BackendSnapshot {
        backend: overlay.status.presentation.backend.clone(),
        gpu_uploads: Some(overlay.status.presentation.gpu_uploads),
        gpu_presents: Some(overlay.status.presentation.gpu_presents),
        composition_commits: Some(overlay.status.presentation.composition_commits),
    }
}

/// Ignored by default. The finite runner starts exactly four processes in ABBA
/// order and supplies an output folder and expected backend for each process.
#[test]
#[ignore = "manual finite release benchmark; use runtime/gpu-integration/run-release-benchmark.py"]
fn release_overlay_benchmark() -> Result<()> {
    run_with_backend_adapter(presentation_snapshot)
}
