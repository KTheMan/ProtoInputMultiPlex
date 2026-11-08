//! Explicit remote-only renderer qualification; never starts the product application.
#[cfg(windows)]
#[path = "../src/renderer_config.rs"]
mod renderer_config;

#[cfg(windows)]
mod smoke {
    use anyhow::{Context, Result, ensure};
    use eframe::egui::{self, Color32};
    use ouroboros_ui::{
        Mode, Theme,
        atoms::{Button, Text},
        theme::typography,
        tokens::core,
    };
    use serde_json::{Value, json};
    use std::{
        cell::RefCell,
        fs::{File, OpenOptions},
        io::Write,
        path::PathBuf,
        rc::Rc,
        time::{Duration, Instant},
    };

    const LOCAL_DEADLINE: Duration = Duration::from_secs(10);
    const MARKER: &str = "software-renderer-smoke-v1";
    const COLORS: [Color32; 4] = [Color32::RED, Color32::GREEN, Color32::BLUE, Color32::YELLOW];
    const CENTERS: [[f32; 2]; 4] = [[96.0, 152.0], [224.0, 152.0], [96.0, 200.0], [224.0, 200.0]];

    struct Evidence {
        file: File,
        started: Instant,
        sequence: u64,
        frames: u64,
        verified: bool,
        close_count: u32,
        failure: Option<&'static str>,
    }

    impl Evidence {
        fn record(&mut self, milestone: &str, details: Value) -> Result<()> {
            self.sequence += 1;
            let line = json!({"schema": MARKER, "sequence": self.sequence,
                "elapsed_ms": self.started.elapsed().as_millis(),
                "milestone": milestone, "details": details});
            serde_json::to_writer(&mut self.file, &line)?;
            self.file.write_all(b"\n")?;
            self.file.sync_data()?;
            Ok(())
        }
    }

    struct RendererSmoke {
        evidence: Rc<RefCell<Evidence>>,
        requested: bool,
        request_scale: f32,
    }

    // This validates renderer readback, not native presentation or physical scanout.
    fn verify_pixels(image: &egui::ColorImage, scale: f32) -> Result<Value> {
        ensure!(scale.is_finite() && scale > 0.0, "invalid screenshot scale");
        ensure!(image.size[0] > 0 && image.size[1] > 0, "empty readback");
        let mut samples = Vec::new();
        for (center, expected) in CENTERS.iter().zip(COLORS) {
            let x = (center[0] * scale).round() as usize;
            let y = (center[1] * scale).round() as usize;
            ensure!(
                x >= 2 && y >= 2 && x + 2 < image.size[0] && y + 2 < image.size[1],
                "pattern outside screenshot"
            );
            let expected = expected.to_array();
            for py in y - 2..=y + 2 {
                for px in x - 2..=x + 2 {
                    let actual = image[(px, py)].to_array();
                    ensure!(
                        actual
                            .iter()
                            .zip(expected)
                            .all(|(&a, e)| a.abs_diff(e) <= 12),
                        "pattern mismatch at ({px},{py}): {actual:?} != {expected:?}"
                    );
                }
            }
            samples.push(json!({"center_px": [x,y], "expected_rgba": expected,
                "actual_rgba": image[(x,y)].to_array(), "matching_pixels": 25}));
        }
        Ok(json!({"size_px": image.size, "pixels_per_point": scale,
            "regions": samples, "verified_nonuniform": true,
            "physical_display_or_presentation_proof": false}))
    }

    impl RendererSmoke {
        fn step(&mut self, ui: &mut egui::Ui) -> Result<()> {
            let ctx = ui.ctx().clone();
            let mut evidence = self.evidence.borrow_mut();
            evidence.frames += 1;
            if evidence.frames <= 6 {
                let count = evidence.frames;
                evidence.record("ui_frame_progress", json!({"frame": count}))?;
            }
            ui.spacing_mut().item_spacing = egui::vec2(core::SPACE_3, core::SPACE_3);
            Text::new("ProtoInputMux renderer qualification").show(ui);
            Text::new(format!("UI frame {}", evidence.frames)).show(ui);
            Button::new("Renderer fixture only").enabled(false).show(ui);
            for (index, color) in COLORS.into_iter().enumerate() {
                let min = egui::pos2(
                    32.0 + (index % 2) as f32 * 128.0,
                    128.0 + (index / 2) as f32 * 48.0,
                );
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(min, egui::vec2(128.0, 48.0)),
                    0.0,
                    color,
                );
            }
            let replies = ctx.input(|input| {
                input
                    .events
                    .iter()
                    .filter_map(|event| {
                        if let egui::Event::Screenshot {
                            viewport_id,
                            user_data,
                            image,
                        } = event
                        {
                            let ours = user_data
                                .data
                                .as_ref()
                                .and_then(|data| data.downcast_ref::<&'static str>())
                                == Some(&MARKER);
                            if *viewport_id == egui::ViewportId::ROOT && ours {
                                return Some(image.clone());
                            }
                        }
                        None
                    })
                    .collect::<Vec<_>>()
            });
            for image in replies {
                evidence.record(
                    "screenshot_readback_received",
                    json!({"size_px": image.size}),
                )?;
                let result = verify_pixels(&image, self.request_scale);
                match result {
                    Ok(details) => {
                        evidence.record("known_pattern_pixels_verified", details)?;
                        evidence.verified = true;
                    }
                    Err(error) => {
                        evidence.failure = Some("readback_error");
                        evidence.record("readback_error", json!({"error": error.to_string()}))?;
                    }
                }
            }
            if evidence.frames >= 3 && !self.requested {
                self.requested = true;
                self.request_scale = ctx.pixels_per_point();
                evidence.record(
                    "screenshot_requested",
                    json!({"pixels_per_point": self.request_scale}),
                )?;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(
                    MARKER,
                )));
            }
            if evidence.started.elapsed() >= LOCAL_DEADLINE
                && evidence.close_count == 0
                && evidence.failure.is_none()
            {
                evidence.failure = Some("timeout");
                evidence.record(
                    "timeout",
                    json!({"phase": "readback_or_ui", "local_deadline_ms": 10000}),
                )?;
            }
            if evidence.close_count == 0
                && ((evidence.verified && evidence.frames >= 6) || evidence.failure.is_some())
            {
                evidence.record("close_requested", json!({"count": 1}))?;
                evidence.close_count = 1;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ctx.request_repaint_after(Duration::from_millis(40));
            Ok(())
        }
    }

    impl eframe::App for RendererSmoke {
        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            if let Err(error) = self.step(ui) {
                eprintln!("renderer smoke evidence/UI error: {error:#}");
                let mut evidence = self.evidence.borrow_mut();
                evidence.failure = Some("evidence_or_ui_error");
                if evidence.close_count == 0 {
                    evidence.close_count = 1;
                    // Best effort only: inability to persist evidence must never pass.
                    let _ = evidence
                        .record("close_requested", json!({"count": 1, "after_error": true}));
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    pub fn run() -> Result<()> {
        let args = std::env::args_os().skip(1).collect::<Vec<_>>();
        ensure!(
            args.len() == 4
                && args[0] == "--renderer"
                && args[1] == "software-dx12"
                && args[2] == "--report",
            "explicit command required: --renderer software-dx12 --report NEW_PATH.jsonl"
        );
        let report = PathBuf::from(&args[3]);
        ensure!(report.is_absolute(), "report path must be absolute");
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&report)
            .with_context(|| format!("create fresh evidence file {}", report.display()))?;
        let evidence = Rc::new(RefCell::new(Evidence {
            file,
            started: Instant::now(),
            sequence: 0,
            frames: 0,
            verified: false,
            close_count: 0,
            failure: None,
        }));
        evidence.borrow_mut().record(
            "requested_mode",
            json!({"mode": "software-dx12",
            "hardware_fallback": false, "external_watchdog_required": true}),
        )?;
        // Shared policy returns before native initialization if the feature is missing.
        let mut options = super::renderer_config::renderer_options(true)?;
        options.viewport = egui::ViewportBuilder::default()
            .with_inner_size([400.0, 280.0])
            .with_min_inner_size([400.0, 280.0])
            .with_max_inner_size([400.0, 280.0])
            .with_resizable(false);
        options.persist_window = false;
        options.run_and_return = true;
        let app_evidence = evidence.clone();
        let result = eframe::run_native(
            "ProtoInputMux renderer qualification",
            options,
            Box::new(move |cc| {
                super::renderer_config::verify_renderer(true, cc)?;
                let state = cc
                    .wgpu_render_state
                    .as_ref()
                    .context("missing verified wgpu state")?;
                let info = state.adapter.get_info();
                app_evidence.borrow_mut().record("actual_adapter_verified", json!({
                "backend": format!("{:?}", info.backend), "device_type": format!("{:?}",info.device_type),
                "name": info.name, "vendor": info.vendor, "device": info.device,
                "driver": info.driver, "driver_info": info.driver_info,
                "cpu_and_dx12": true, "source": "CreationContext.wgpu_render_state.adapter"}))?;
                Theme::install(&cc.egui_ctx, Mode::Dark);
                let mut fonts = egui::FontDefinitions::default();
                typography::register(&mut fonts);
                cc.egui_ctx.set_fonts(fonts);
                Ok(Box::new(RendererSmoke {
                    evidence: app_evidence,
                    requested: false,
                    request_scale: 1.0,
                }))
            }),
        );
        let mut evidence = evidence.borrow_mut();
        match result {
            Ok(()) => evidence.record("run_native_returned", json!({"normal": true}))?,
            Err(error) => {
                evidence.record("run_native_returned", json!({"normal": false}))?;
                let message = error.to_string();
                let classification = if message.contains("Software DX12 renderer unavailable") {
                    "unsupported"
                } else {
                    "initialization_or_native_error"
                };
                evidence.failure = Some(classification);
                evidence.record(classification, json!({"error": message}))?;
            }
        }
        if evidence.failure.is_none()
            && !(evidence.verified && evidence.frames >= 6 && evidence.close_count == 1)
        {
            evidence.failure = Some("incomplete");
            let details = json!({"verified": evidence.verified, "frames": evidence.frames,
                "close_count": evidence.close_count});
            evidence.record("incomplete", details)?;
        }
        ensure!(
            evidence.failure.is_none(),
            "renderer qualification failed: {:?}",
            evidence.failure
        );
        evidence.record(
            "clean_exit_ready",
            json!({"intended_exit_code": 0,
            "controller_must_attest_actual_process_exit": true,
            "physical_display_or_presentation_proof": false}),
        )?;
        Ok(())
    }
}

fn main() -> std::process::ExitCode {
    #[cfg(windows)]
    match smoke::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("software renderer smoke failed: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("software renderer smoke unsupported: Windows DX12 required");
        std::process::ExitCode::FAILURE
    }
}
