//! Disposable target application. Reports counters, never text typed by a user.
use anyhow::Result;
use eframe::egui;
use ouroboros_ui::{
    Mode, Theme,
    atoms::{Button, Heading, Text},
};
use serde::Serialize;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Default, Serialize)]
struct Snapshot {
    pid: u32,
    cursor: (i32, i32),
    test_keys: u8,
    buttons: u8,
    drag_moves: u64,
    wheel_events: u64,
    wheel_delta: f32,
    mouse_moves: u64,
    mouse_clicks: u64,
    key_presses: u64,
    sampled_test_key_presses: u64,
    frames: u64,
}
struct Probe {
    snapshot: Snapshot,
    report: PathBuf,
    last_save: Instant,
    error: String,
}
pub fn run(report: Option<PathBuf>) -> Result<()> {
    let pid = std::process::id();
    let report = report.unwrap_or_else(|| PathBuf::from(format!("runtime/probe-{pid}.json")));
    if let Some(parent) = report.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    eframe::run_native(
        &format!("ProtoInputMultiPlex Routing Test — PID {pid}"),
        eframe::NativeOptions {
            renderer: eframe::Renderer::Glow,
            viewport: egui::ViewportBuilder::default().with_inner_size([530.0, 430.0]),
            ..Default::default()
        },
        Box::new(move |cc| {
            Theme::install(&cc.egui_ctx, Mode::Dark);
            Ok(Box::new(Probe {
                snapshot: Snapshot {
                    pid,
                    ..Default::default()
                },
                report,
                last_save: Instant::now(),
                error: String::new(),
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("test window: {e}"))
}
impl eframe::App for Probe {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let (cursor, keys) = mux_windows::targets::probe_input();
        self.snapshot.sampled_test_key_presses +=
            (keys & !self.snapshot.test_keys).count_ones() as u64;
        self.snapshot.buttons = mux_windows::targets::probe_buttons();
        self.snapshot.cursor = cursor;
        self.snapshot.test_keys = keys;
        self.snapshot.frames += 1;
        ui.input(|input| {
            for event in &input.events {
                match event {
                    egui::Event::PointerMoved(_) => {
                        self.snapshot.mouse_moves += 1;
                        if self.snapshot.buttons != 0 {
                            self.snapshot.drag_moves += 1;
                        }
                    }
                    egui::Event::MouseWheel { delta, .. } => {
                        self.snapshot.wheel_events += 1;
                        self.snapshot.wheel_delta += delta.y;
                    }
                    egui::Event::PointerButton { pressed: true, .. } => {
                        self.snapshot.mouse_clicks += 1
                    }
                    egui::Event::Key {
                        pressed: true,
                        repeat: false,
                        ..
                    } => self.snapshot.key_presses += 1,
                    _ => (),
                }
            }
        });
        Heading::new("Routing test").h1().show(ui);
        Text::new(format!("Process {}", self.snapshot.pid))
            .code()
            .show(ui);
        Text::new("Assign this window to a user, start routing, then move that mouse and press W/A/S/D/Space. Compare a second test window.").wrap().show(ui);
        Text::new(format!("Cursor API: {}, {}", cursor.0, cursor.1)).show(ui);
        Text::new(format!(
            "Held test keys: {keys:05b} · presses {}",
            self.snapshot.sampled_test_key_presses
        ))
        .show(ui);
        Text::new(format!(
            "Window events: {} moves · {} clicks · {} keys",
            self.snapshot.mouse_moves, self.snapshot.mouse_clicks, self.snapshot.key_presses
        ))
        .wrap()
        .show(ui);
        if Button::new("Reset counters").outline().show(ui).clicked() {
            self.snapshot = Snapshot {
                pid: self.snapshot.pid,
                cursor,
                test_keys: keys,
                ..Default::default()
            };
        }
        Text::new(format!("Report: {}", self.report.display()))
            .caption()
            .muted()
            .wrap()
            .show(ui);
        if !self.error.is_empty() {
            Text::new(&self.error).wrap().show(ui);
        }
        if self.last_save.elapsed() >= Duration::from_millis(250) {
            let result = serde_json::to_vec_pretty(&self.snapshot)
                .map_err(anyhow::Error::from)
                .and_then(|json| std::fs::write(&self.report, json).map_err(Into::into));
            self.error = result
                .err()
                .map_or_else(String::new, |e| format!("Report: {e}"));
            self.last_save = Instant::now();
        }
        ui.ctx().request_repaint_after(Duration::from_millis(16));
    }
}
