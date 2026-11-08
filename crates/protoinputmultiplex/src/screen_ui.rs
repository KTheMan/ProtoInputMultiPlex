//! Memory-only selected-window snapshot tools. Native capture stays in the worker.
use crate::{
    agent_runtime::{CaptureState, CapturedFrame},
    screen_analysis::Query,
};
use anyhow::{Result, ensure};
use base64::Engine as _;
use eframe::egui;
use mux_core::screen::{Image, Region};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::mpsc::{self, Receiver},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub session: u64,
    pub target_revision: u64,
    pub user_id: String,
    pub window: mux_core::routing::Window,
}
struct Preview {
    rgba: image::RgbaImage,
    thumbnail: image::RgbaImage,
}
struct Job<T> {
    generation: u64,
    receiver: Receiver<Result<T>>,
}

pub struct ScreenPage {
    pub user: usize,
    pub pixel: [String; 2],
    pub region: [String; 4],
    pub search: [String; 4],
    pub min_score: String,
    pub ocr_region: [String; 4],
    ocr_capability: Option<crate::screen_ocr::Capability>,
    ocr_capability_job: Option<Receiver<crate::screen_ocr::Capability>>,
    binding: Option<Binding>,
    generation: u64,
    pending_capture: Option<(Binding, u64)>,
    frame: Option<CapturedFrame>,
    preview: Option<Preview>,
    texture: Option<egui::TextureHandle>,
    preview_job: Option<Job<Preview>>,
    template_job: Option<Job<String>>,
    template: Option<String>,
    template_size: Option<[u32; 2]>,
    pub error: Option<String>,
}
impl Default for ScreenPage {
    fn default() -> Self {
        Self {
            user: 0,
            pixel: ["0".into(), "0".into()],
            region: ["0".into(), "0".into(), "16".into(), "16".into()],
            search: std::array::from_fn(|_| String::new()),
            min_score: "0.95".into(),
            ocr_region: std::array::from_fn(|_| String::new()),
            ocr_capability: None,
            ocr_capability_job: None,
            binding: None,
            generation: 0,
            pending_capture: None,
            frame: None,
            preview: None,
            texture: None,
            preview_job: None,
            template_job: None,
            template: None,
            template_size: None,
            error: None,
        }
    }
}
impl ScreenPage {
    pub fn check_ocr_capability(&mut self, ctx: &egui::Context, retry: bool) {
        if self.ocr_capability_job.is_some() || (!retry && self.ocr_capability.is_some()) {
            return;
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        let wake = ctx.clone();
        match std::thread::Builder::new()
            .name("ocr-model-check".into())
            .spawn(move || {
                let _ = sender.send(crate::screen_ocr::capability());
                wake.request_repaint();
            }) {
            Ok(_) => {
                self.ocr_capability_job = Some(receiver);
            }
            Err(_) => {
                self.ocr_capability = Some(crate::screen_ocr::Capability {
                    available: false,
                    language: "Latin".into(),
                    reason: Some("The local OCR model check could not start.".into()),
                });
            }
        }
    }
    pub fn ocr_available(&self) -> bool {
        self.ocr_capability
            .as_ref()
            .is_some_and(|value| value.available)
    }
    pub fn ocr_check_pending(&self) -> bool {
        self.ocr_capability_job.is_some()
    }
    pub fn ocr_status(&self) -> String {
        self.ocr_capability.as_ref().map_or_else(
            || "Checking local OCR models…".into(),
            |value| {
                if value.available {
                    "Local OCR · Latin text model · early preview".into()
                } else {
                    value
                        .reason
                        .clone()
                        .unwrap_or_else(|| "Local OCR is unavailable.".into())
                }
            },
        )
    }
    pub fn ocr_query(&self) -> Result<Query> {
        ensure!(self.ocr_available(), "{}", self.ocr_status());
        let region = if self.ocr_region.iter().all(|value| value.is_empty()) {
            None
        } else {
            Some(parse_region(&self.ocr_region)?)
        };
        Ok(Query::Ocr { region })
    }
    pub fn sync(&mut self, binding: Option<Binding>) {
        if self.binding == binding {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        self.binding = binding;
        self.clear_snapshot();
        // Keep the one in-flight uncorrelated local capture until its receipt.
        // A new request cannot mistake that older reply for its own capture.
    }
    fn clear_snapshot(&mut self) {
        self.frame = None;
        self.preview = None;
        self.texture = None;
        self.preview_job = None;
        self.template_job = None;
        self.template = None;
        self.template_size = None;
        self.error = None;
    }
    pub fn reset_session(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.binding = None;
        self.pending_capture = None;
        self.clear_snapshot();
    }
    pub fn available(&self) -> bool {
        self.binding.is_some() && !self.busy()
    }
    pub fn graph_template(&self) -> Option<&str> {
        self.template.as_deref()
    }
    pub fn has_snapshot(&self) -> bool {
        self.preview.is_some()
    }
    pub fn busy(&self) -> bool {
        self.pending_capture.is_some() || self.preview_job.is_some() || self.template_job.is_some()
    }
    pub fn request(&mut self) -> bool {
        if !self.available() {
            return false;
        }
        self.error = None;
        self.frame = None;
        self.preview = None;
        self.texture = None;
        self.pending_capture = self
            .binding
            .clone()
            .map(|binding| (binding, self.generation));
        true
    }
    pub fn capture_error(&mut self, message: String) {
        self.pending_capture = None;
        self.error = Some(message);
    }
    pub fn receive(&mut self, state: CaptureState, ctx: &egui::Context) {
        let Some((requested, generation)) = self.pending_capture.take() else {
            return;
        };
        if generation != self.generation || self.binding.as_ref() != Some(&requested) {
            return;
        }
        if let Some(error) = state.error {
            self.error = Some(error);
            return;
        }
        let Some(frame) = state.frame else {
            self.error = Some("Capture returned no snapshot.".into());
            return;
        };
        if frame.user_id != requested.user_id
            || frame.pid != requested.window.pid
            || frame.hwnd != requested.window.hwnd
        {
            self.error = Some("Selected target changed; capture again.".into());
            return;
        }
        self.preview = None;
        self.texture = None;
        let copy = frame.clone();
        let (tx, receiver) = mpsc::sync_channel(1);
        let wake = ctx.clone();
        match std::thread::Builder::new()
            .name("screen-preview".into())
            .spawn(move || {
                let result = crate::screen_analysis::decode_frame(&copy).map(|rgba| {
                    let scale = (800.0 / f64::from(rgba.width()))
                        .min(480.0 / f64::from(rgba.height()))
                        .min(1.0);
                    let width = (f64::from(rgba.width()) * scale).round().max(1.0) as u32;
                    let height = (f64::from(rgba.height()) * scale).round().max(1.0) as u32;
                    let thumbnail = image::imageops::resize(
                        &rgba,
                        width,
                        height,
                        image::imageops::FilterType::Triangle,
                    );
                    Preview { rgba, thumbnail }
                });
                let _ = tx.send(result);
                wake.request_repaint();
            }) {
            Ok(_) => {
                self.preview_job = Some(Job {
                    generation: self.generation,
                    receiver,
                });
                self.frame = Some(frame);
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }
    pub fn poll(&mut self, ctx: &egui::Context) {
        if let Some(receiver) = &self.ocr_capability_job {
            let result = match receiver.try_recv() {
                Ok(value) => Some(value),
                Err(mpsc::TryRecvError::Disconnected) => Some(crate::screen_ocr::Capability {
                    available: false,
                    language: "Latin".into(),
                    reason: Some("The local OCR model check ended without a result.".into()),
                }),
                Err(mpsc::TryRecvError::Empty) => None,
            };
            if let Some(value) = result {
                self.ocr_capability = Some(value);
                self.ocr_capability_job = None;
            }
        }
        let preview = self
            .preview_job
            .as_ref()
            .and_then(|job| match job.receiver.try_recv() {
                Ok(value) => Some((job.generation, value)),
                Err(mpsc::TryRecvError::Disconnected) => Some((
                    job.generation,
                    Err(anyhow::anyhow!("Preview worker exited")),
                )),
                Err(_) => None,
            });
        if let Some((generation, result)) = preview {
            self.preview_job = None;
            if generation == self.generation {
                match result {
                    Ok(preview) => {
                        let thumbnail = &preview.thumbnail;
                        self.texture = Some(ctx.load_texture(
                            "selected-window-snapshot",
                            egui::ColorImage::from_rgba_unmultiplied(
                                [thumbnail.width() as usize, thumbnail.height() as usize],
                                thumbnail.as_raw(),
                            ),
                            egui::TextureOptions::LINEAR,
                        ));
                        self.preview = Some(preview);
                    }
                    Err(error) => self.error = Some(format!("{error:#}")),
                }
            }
        }
        let template = self
            .template_job
            .as_ref()
            .and_then(|job| match job.receiver.try_recv() {
                Ok(value) => Some((job.generation, value)),
                Err(mpsc::TryRecvError::Disconnected) => Some((
                    job.generation,
                    Err(anyhow::anyhow!("Template worker exited")),
                )),
                Err(_) => None,
            });
        if let Some((generation, result)) = template {
            self.template_job = None;
            if generation == self.generation {
                match result {
                    Ok(template) => self.template = Some(template),
                    Err(error) => self.error = Some(format!("{error:#}")),
                }
            }
        }
    }
    pub fn pixel_query(&self) -> Result<Query> {
        Ok(Query::Pixel {
            x: self.pixel[0].parse()?,
            y: self.pixel[1].parse()?,
        })
    }
    pub fn match_query(&self) -> Result<Query> {
        let min_score = self.min_score.parse::<f64>()?;
        ensure!(
            min_score.is_finite() && (0.0..=1.0).contains(&min_score),
            "Minimum similarity must be finite and between 0 and 1"
        );
        let region = if self.search.iter().all(|s| s.is_empty()) {
            None
        } else {
            Some(parse_region(&self.search)?)
        };
        Ok(Query::ImageMatch {
            template_png_base64: self
                .template
                .clone()
                .ok_or_else(|| anyhow::anyhow!("Choose a template region first"))?,
            region,
            min_score,
        })
    }
    pub fn save_template(&mut self, ctx: &egui::Context) -> Result<()> {
        ensure!(!self.busy(), "Wait for the current snapshot operation");
        let region = parse_region(&self.region)?;
        ensure!(
            region.width <= 128 && region.height <= 128,
            "Template must be at most 128 × 128 pixels"
        );
        let image = &self
            .preview
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Capture a snapshot first"))?
            .rgba;
        let rgba = Image::new(image.width(), image.height(), image.as_raw())?.crop(region)?;
        let (tx, receiver) = mpsc::sync_channel(1);
        let wake = ctx.clone();
        std::thread::Builder::new()
            .name("screen-template".into())
            .spawn(move || {
                let result = (|| -> Result<String> {
                    let mut png = std::io::Cursor::new(Vec::new());
                    image::write_buffer_with_format(
                        &mut png,
                        &rgba,
                        region.width,
                        region.height,
                        image::ColorType::Rgba8,
                        image::ImageFormat::Png,
                    )?;
                    Ok(base64::engine::general_purpose::STANDARD.encode(png.into_inner()))
                })();
                let _ = tx.send(result);
                wake.request_repaint();
            })?;
        self.template = None;
        self.template_size = Some([region.width, region.height]);
        self.template_job = Some(Job {
            generation: self.generation,
            receiver,
        });
        self.error = None;
        Ok(())
    }
    pub fn snapshot(&self) -> Value {
        json!({"user":self.user,"available":self.available(),"busy":self.busy(),
            "target":self.binding.as_ref().map(|b|json!({"user_id":b.user_id,"pid":b.window.pid,"hwnd":b.window.hwnd})),
            "snapshot":self.frame.as_ref().map(|f|json!({"user_id":f.user_id,"pid":f.pid,"hwnd":f.hwnd,"width":f.width,"height":f.height,"bounds":f.bounds,"analysis":f.analysis.as_ref().map(|value| if value["type"] == "ocr" {json!({"type":"ocr","completed":true})} else {value.clone()})})),
            "ocr_available":self.ocr_available(),"ocr_check_pending":self.ocr_capability_job.is_some(),
            "template_size":self.template.as_ref().and(self.template_size),"error":self.error})
    }
    pub fn show_snapshot(&self, ui: &mut egui::Ui) {
        use ouroboros_ui::atoms::Text;
        if let Some(frame) = &self.frame {
            Text::new(format!(
                "Captured snapshot · PID {} · {} × {}",
                frame.pid, frame.width, frame.height
            ))
            .show(ui);
            Text::new("This is a snapshot. Inspect pixel, Find template and Read text capture the selected window again.").caption().wrap().show(ui);
            if let Some(result) = &frame.analysis {
                Text::new(analysis_summary(result)).wrap().show(ui);
            }
        }
        if let Some([w, h]) = self.template.as_ref().and(self.template_size) {
            Text::new(format!("Template ready · {w} × {h} · memory only")).show(ui);
        }
        if let Some(error) = &self.error {
            Text::new(error).wrap().show(ui);
        }
    }
    pub fn show_preview(&self, ui: &mut egui::Ui) {
        if let Some(texture) = &self.texture {
            let scale = (ui.available_width() / texture.size_vec2().x).min(1.0);
            ui.image((texture.id(), texture.size_vec2() * scale));
        }
    }
    pub fn apply_script_fields(&mut self, fields: &BTreeMap<String, String>) {
        for (name, value) in fields {
            let name = name.as_str();
            if name == "screen_user" {
                if let Ok(user) = value.parse() {
                    self.user = user;
                }
            } else if name == "screen_min_score" {
                self.min_score = value.clone();
            } else {
                for (prefix, values) in [
                    ("screen_pixel_", self.pixel.as_mut_slice()),
                    ("screen_region_", self.region.as_mut_slice()),
                    ("screen_search_", self.search.as_mut_slice()),
                    ("screen_ocr_", self.ocr_region.as_mut_slice()),
                ] {
                    if let Some(index) = name
                        .strip_prefix(prefix)
                        .and_then(|i| i.parse::<usize>().ok())
                        && let Some(field) = values.get_mut(index)
                    {
                        *field = value.clone();
                    }
                }
            }
        }
    }
}
fn analysis_summary(result: &Value) -> String {
    if result["type"] == "ocr" {
        let text = result["text"].as_str().unwrap_or_default();
        if text.is_empty() {
            "No text found in this snapshot.".into()
        } else {
            format!("Extracted text · Latin model\n{text}")
        }
    } else if result["type"] == "pixel" {
        let point = &result["client_position"];
        let rgba = &result["rgba"];
        format!(
            "Pixel ({}, {}) · RGB {}, {}, {} · alpha {}",
            point[0], point[1], rgba[0], rgba[1], rgba[2], rgba[3]
        )
    } else if result["match"].is_null() {
        format!(
            "No match at or above {:.1}% similarity.",
            result["min_score"].as_f64().unwrap_or(1.0) * 100.0
        )
    } else {
        let found = &result["match"];
        format!(
            "Match at ({}, {}) · {} × {} · {:.1}% similarity",
            found["top_left"][0],
            found["top_left"][1],
            found["size"][0],
            found["size"][1],
            found["score"].as_f64().unwrap_or(0.0) * 100.0
        )
    }
}
fn parse_region(fields: &[String; 4]) -> Result<Region> {
    Ok(Region {
        x: fields[0].parse()?,
        y: fields[1].parse()?,
        width: fields[2].parse()?,
        height: fields[3].parse()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binding(hwnd: usize) -> Binding {
        Binding {
            session: 1,
            target_revision: 1,
            user_id: "user-a".into(),
            window: mux_core::routing::Window { hwnd, pid: 7 },
        }
    }
    #[test]
    fn ocr_private_text_is_visible_locally_but_absent_from_script_state_and_cleared_on_rebind() {
        let mut page = ScreenPage::default();
        page.sync(Some(binding(11)));
        let analysis = json!({"type":"ocr", "text":"PRIVATE OCR 7319",
            "lines":[{"text":"PRIVATE OCR 7319", "client_box":[1,2,3,4]}]});
        assert!(analysis_summary(&analysis).contains("PRIVATE OCR 7319"));
        page.frame = Some(CapturedFrame {
            png: Vec::new(),
            bounds: mux_core::desktop::Bounds {
                left: 0,
                top: 0,
                right: 1,
                bottom: 1,
            },
            width: 1,
            height: 1,
            pid: 7,
            hwnd: 11,
            user_id: "user-a".into(),
            analysis: Some(analysis),
        });
        let snapshot = page.snapshot();
        assert_eq!(
            snapshot["snapshot"]["analysis"],
            json!({"type":"ocr","completed":true})
        );
        assert!(!snapshot.to_string().contains("PRIVATE OCR"));
        assert!(!snapshot.to_string().contains("client_box"));
        page.sync(Some(binding(12)));
        assert!(page.frame.is_none() && page.snapshot()["snapshot"].is_null());
    }
    #[test]
    fn ocr_controls_require_models_and_validate_an_optional_region() {
        let mut page = ScreenPage::default();
        assert!(page.ocr_query().is_err());
        page.ocr_capability = Some(crate::screen_ocr::Capability {
            available: true,
            language: "Latin".into(),
            reason: None,
        });
        assert!(matches!(
            page.ocr_query().unwrap(),
            Query::Ocr { region: None }
        ));
        page.apply_script_fields(&BTreeMap::from([
            ("screen_ocr_0".into(), "12".into()),
            ("screen_ocr_1".into(), "8".into()),
            ("screen_ocr_2".into(), "320".into()),
            ("screen_ocr_3".into(), "100".into()),
        ]));
        assert!(matches!(
            page.ocr_query().unwrap(),
            Query::Ocr {
                region: Some(Region {
                    x: 12,
                    y: 8,
                    width: 320,
                    height: 100
                })
            }
        ));
        page.ocr_region[0] = "-1".into();
        assert!(page.ocr_query().is_err());
        page.ocr_region[0].clear();
        assert!(
            page.ocr_query().is_err(),
            "a partially empty region is not the whole window"
        );
    }
    #[test]
    fn target_changes_invalidate_even_if_old_target_returns() {
        let mut page = ScreenPage::default();
        page.sync(Some(binding(11)));
        assert!(page.request());
        page.template = Some("private".into());
        page.template_size = Some([1, 1]);
        let initial = page.generation;
        page.sync(Some(binding(12)));
        page.sync(Some(binding(11)));
        assert!(page.generation > initial && page.template.is_none());
        assert!(page.pending_capture.is_some() && !page.available());
        // Same HWND returning cannot authorize an old outstanding receipt.
        page.receive(
            CaptureState {
                acknowledged: true,
                frame: Some(CapturedFrame {
                    png: Vec::new(),
                    bounds: mux_core::desktop::Bounds {
                        left: 0,
                        top: 0,
                        right: 1,
                        bottom: 1,
                    },
                    width: 1,
                    height: 1,
                    pid: 7,
                    hwnd: 11,
                    user_id: "user-a".into(),
                    analysis: None,
                }),
                error: None,
            },
            &egui::Context::default(),
        );
        assert!(page.frame.is_none() && page.preview_job.is_none());
    }
    #[test]
    fn a_coalesced_target_revision_invalidates_the_same_window() {
        let mut page = ScreenPage::default();
        page.sync(Some(binding(11)));
        page.template = Some("private".into());
        let generation = page.generation;
        let mut returned = binding(11);
        returned.target_revision = 3;
        page.sync(Some(returned));
        assert!(page.generation != generation && page.template.is_none());
    }
    #[test]
    fn gui_match_scores_are_rejected_before_the_worker_json_boundary() {
        let mut page = ScreenPage {
            template: Some("memory-template".into()),
            ..Default::default()
        };
        for invalid in ["NaN", "inf", "-inf", "-1", "1.01"] {
            page.min_score = invalid.into();
            assert!(
                page.match_query().is_err(),
                "{invalid} must not be sent to the worker"
            );
        }
        for valid in ["0", "0.95", "1"] {
            page.min_score = valid.into();
            let query = page.match_query().unwrap();
            let encoded = serde_json::to_vec(&query).unwrap();
            assert!(serde_json::from_slice::<Query>(&encoded).is_ok());
        }
        assert!(!page.has_snapshot());
    }
    #[test]
    fn reset_drops_private_snapshots_templates_and_jobs() {
        let mut page = ScreenPage::default();
        page.sync(Some(binding(11)));
        page.request();
        page.template = Some("private".into());
        page.reset_session();
        assert!(page.binding.is_none() && !page.busy() && page.template.is_none());
    }
}
