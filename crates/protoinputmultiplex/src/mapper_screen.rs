//! Private, bounded selected-window observation jobs for Mapper continuations.
use crate::screen_analysis::{self, PreparedQuery, Query};
use anyhow::{Context, Result, bail, ensure};
use mux_core::{
    mapper::{self, Compiled, ObservationMatch, ObservationResult},
    routing::Window,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

pub(crate) fn prepare(compiled: &Compiled) -> Result<BTreeMap<usize, Arc<PreparedQuery>>> {
    let mut queries = BTreeMap::new();
    for descriptor in compiled.observations() {
        let query = match &compiled.definition().nodes[descriptor.node_index].kind {
            mapper::NodeKind::Ocr { region, .. } => Query::Ocr { region: *region },
            mapper::NodeKind::PixelColor { x, y, .. } => Query::Pixel { x: *x, y: *y },
            mapper::NodeKind::ImageMatch {
                template_png_base64,
                region,
                min_score,
                ..
            } => Query::ImageMatch {
                template_png_base64: template_png_base64.clone(),
                region: *region,
                min_score: *min_score,
            },
            _ => bail!("Compiled observation has no matching query"),
        };
        queries.insert(
            descriptor.node_index,
            Arc::new(screen_analysis::prepare_query(query)?),
        );
    }
    Ok(queries)
}

pub(crate) trait Task {
    fn poll(&mut self) -> Option<Result<ObservationResult>>;
    fn cancel(&self);
}
pub(crate) trait Provider {
    fn busy(&self) -> bool {
        false
    }
    fn start(
        &mut self,
        window: Window,
        user_id: String,
        query: Arc<PreparedQuery>,
    ) -> Result<Box<dyn Task>>;
}
#[derive(Default)]
pub(crate) struct NativeProvider;
// A canceled task can outlive a routing session while PrintWindow is blocked.
// Its physical permit stays held by the THREAD, never by the logical task slot.
static PHYSICAL_TASK: AtomicBool = AtomicBool::new(false);
struct PhysicalPermit;
impl Drop for PhysicalPermit {
    fn drop(&mut self) {
        PHYSICAL_TASK.store(false, Ordering::Release);
    }
}
struct NativeTask {
    cancel: Arc<AtomicBool>,
    receiver: mpsc::Receiver<Result<ObservationResult>>,
}
impl Task for NativeTask {
    fn poll(&mut self) -> Option<Result<ObservationResult>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err(anyhow::anyhow!("Mapper capture worker exited")))
            }
        }
    }
    fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
}
impl Drop for NativeTask {
    fn drop(&mut self) {
        self.cancel();
    }
}
impl Provider for NativeProvider {
    fn busy(&self) -> bool {
        PHYSICAL_TASK.load(Ordering::Acquire)
    }
    fn start(
        &mut self,
        window: Window,
        _user_id: String,
        query: Arc<PreparedQuery>,
    ) -> Result<Box<dyn Task>> {
        ensure!(
            PHYSICAL_TASK
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            "A previous Mapper capture worker is still finishing"
        );
        let permit = PhysicalPermit;
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("mapper-screen-capture".into())
            .spawn(move || {
                let permit_guard = permit;
                let result = (|| {
                    let (bounds, pixels) =
                        screen_analysis::capture_pixels(window, Some(&worker_cancel))?;
                    let result = typed_result(screen_analysis::analyze_pixels_with_cancel(
                        bounds,
                        &pixels,
                        &query,
                        Some(&worker_cancel),
                    )?)?;
                    ensure!(
                        !worker_cancel.load(Ordering::Acquire),
                        "Mapper observation was canceled"
                    );
                    Ok(result)
                })();
                drop(permit_guard);
                let _ = sender.send(result);
            })?;
        Ok(Box::new(NativeTask { cancel, receiver }))
    }
}
#[cfg(test)]
fn observe_pixels(
    bounds: mux_core::desktop::Bounds,
    pixels: &image::RgbaImage,
    query: &PreparedQuery,
) -> Result<ObservationResult> {
    typed_result(screen_analysis::analyze_pixels(bounds, pixels, query)?)
}
fn typed_result(value: serde_json::Value) -> Result<ObservationResult> {
    match value["type"].as_str() {
        Some("ocr") => {
            let text = value["text"]
                .as_str()
                .context("Mapper OCR analysis has no text field")?;
            ensure!(
                text.len() <= mapper::MAX_TEXT_BYTES,
                "Mapper OCR text exceeds the byte limit"
            );
            ensure!(
                text.chars()
                    .all(|character| !character.is_control()
                        || matches!(character, '\t' | '\r' | '\n')),
                "Mapper OCR text contains an unsupported control character"
            );
            Ok(ObservationResult::Ocr {
                text: text.to_owned(),
            })
        }
        Some("pixel") => Ok(ObservationResult::PixelColor {
            rgba: serde_json::from_value(value["rgba"].clone())?,
        }),
        Some("image_match") => {
            let found = value
                .get("match")
                .context("Mapper image-match analysis has no match field")?;
            let matched = if found.is_null() {
                None
            } else {
                Some(ObservationMatch {
                    desktop_center: serde_json::from_value(
                        value["match"]["desktop_center"].clone(),
                    )?,
                    score: value["match"]["score"]
                        .as_f64()
                        .context("Mapper match has no score")?,
                })
            };
            Ok(ObservationResult::ImageMatch { matched })
        }
        _ => bail!("Unexpected Mapper screen analysis kind"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ocr_typed_result_accepts_empty_text_and_rejects_invalid_payload_without_echoing_it() {
        for text in ["", "Ready\nOK", "Latin résumé"] {
            assert!(
                matches!(typed_result(serde_json::json!({"type":"ocr","text":text})).unwrap(),
                ObservationResult::Ocr{text:actual} if actual==text)
            );
        }
        for payload in [
            serde_json::json!({"type":"ocr"}),
            serde_json::json!({"type":"ocr","text":2}),
            serde_json::json!({"type":"ocr","text":"x".repeat(mapper::MAX_TEXT_BYTES+1)}),
            serde_json::json!({"type":"ocr","text":"private\0secret"}),
        ] {
            let error = typed_result(payload).unwrap_err().to_string();
            assert!(!error.contains("secret"));
        }
    }
    #[test]
    fn graph_raw_observation_is_not_limited_by_mcp_png_transport_size() {
        let mut state = 0x1234_5678_u32;
        let mut bytes = vec![0_u8; 1024 * 1024 * 4];
        for pixel in bytes.chunks_exact_mut(4) {
            for channel in &mut pixel[..3] {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                *channel = state as u8;
            }
            pixel[3] = 255;
        }
        let pixels = image::RgbaImage::from_raw(1024, 1024, bytes).unwrap();
        let mut png = std::io::Cursor::new(Vec::new());
        image::write_buffer_with_format(
            &mut png,
            pixels.as_raw(),
            1024,
            1024,
            image::ColorType::Rgba8,
            image::ImageFormat::Png,
        )
        .unwrap();
        assert!(
            png.get_ref().len() > 2 * 1024 * 1024,
            "frame must exceed the existing MCP PNG budget"
        );
        let bounds = mux_core::desktop::Bounds {
            left: -20,
            top: 30,
            right: 1004,
            bottom: 1054,
        };
        let query = screen_analysis::prepare_query(Query::Pixel { x: 800, y: 700 }).unwrap();
        let result = observe_pixels(bounds, &pixels, &query).unwrap();
        let ObservationResult::PixelColor { rgba } = result else {
            panic!("pixel result")
        };
        assert_eq!(rgba, pixels.get_pixel(800, 700).0);
    }

    #[test]
    fn every_screen_catalogue_prototype_prepares_without_capture() {
        let mut count = 0;
        for entry in mapper::catalogue() {
            if !matches!(
                &entry.prototype,
                mapper::NodeKind::Ocr { .. }
                    | mapper::NodeKind::PixelColor { .. }
                    | mapper::NodeKind::ImageMatch { .. }
            ) {
                continue;
            }
            let definition: mapper::Definition =
                serde_json::from_value(serde_json::json!({"version":1,"nodes":[
                    {"id":"source","kind":{"type":"wheel","user":"a","horizontal":false}},
                    {"id":"pulse","kind":{"type":"impulse_to_pulse"}},
                    {"id":"query","kind":entry.prototype}
                ],"edges":[
                    {"from":{"node":"source","port":"value"},"to":{"node":"pulse","port":"value"}},
                    {"from":{"node":"pulse","port":"value"},"to":{"node":"query","port":"trigger"}}
                ]}))
                .unwrap();
            assert_eq!(prepare(&definition.compile().unwrap()).unwrap().len(), 1);
            count += 1;
        }
        assert_eq!(count, 3);
    }

    #[test]
    fn analysis_returns_only_typed_channels_and_never_fabricates_a_position() {
        assert!(matches!(
            typed_result(serde_json::json!({"type":"pixel","rgba":[1,2,3,255]})).unwrap(),
            ObservationResult::PixelColor {
                rgba: [1, 2, 3, 255]
            }
        ));
        assert!(matches!(
            typed_result(serde_json::json!({"type":"image_match","match":null})).unwrap(),
            ObservationResult::ImageMatch { matched: None }
        ));
        let result=typed_result(serde_json::json!({"type":"image_match","match":{"desktop_center":[-12.5,4.0],"score":1.0}})).unwrap();
        let ObservationResult::ImageMatch {
            matched: Some(found),
        } = result
        else {
            panic!("match missing")
        };
        assert_eq!(found.desktop_center, [-12.5, 4.0]);
        assert_eq!(found.score, 1.0);
        assert!(typed_result(serde_json::json!({"type":"pixel","rgba":[999,2,3,255]})).is_err());
    }
}
