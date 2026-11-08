//! Bounded snapshot analysis without actuation. Local OCR assets load only on a worker.
use crate::agent_runtime::CapturedFrame;
use anyhow::{Context, Result, ensure};
use base64::Engine as _;
use image::{ImageFormat, ImageReader, Limits, RgbaImage};
use mux_core::screen::{self, Image, Region};
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::Cursor;

pub const MAX_TEMPLATE_BASE64_BYTES: usize = 256 * 1024;
const MAX_FRAME_PNG_BYTES: usize = 2 * 1024 * 1024;
const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Query {
    Ocr {
        #[serde(default)]
        region: Option<Region>,
    },
    Pixel {
        x: u32,
        y: u32,
    },
    ImageMatch {
        /// Base64-encoded PNG, at most 256 KiB encoded and 128 by 128 decoded.
        template_png_base64: String,
        #[serde(default)]
        region: Option<Region>,
        #[serde(default = "default_min_score")]
        min_score: f64,
    },
}
fn default_min_score() -> f64 {
    0.95
}

#[derive(Debug, Clone)]
pub struct PreparedQuery {
    kind: PreparedKind,
}
#[derive(Debug, Clone)]
enum PreparedKind {
    Ocr {
        region: Option<Region>,
    },
    Pixel {
        x: u32,
        y: u32,
    },
    ImageMatch {
        template: RgbaImage,
        region: Option<Region>,
        min_score: f64,
    },
}
impl PreparedQuery {
    /// Shared GUI entry point for a template cropped from its own capture.
    pub fn image_match(
        template: RgbaImage,
        region: Option<Region>,
        min_score: f64,
    ) -> Result<Self> {
        ensure!(
            template.width() <= screen::MAX_TEMPLATE_WIDTH
                && template.height() <= screen::MAX_TEMPLATE_HEIGHT,
            "Template must be at most 128 by 128 pixels"
        );
        Image::new(template.width(), template.height(), template.as_raw())?;
        validate_match_options(region, min_score)?;
        Ok(Self {
            kind: PreparedKind::ImageMatch {
                template,
                region,
                min_score,
            },
        })
    }
}

pub fn prepare_query(query: Query) -> Result<PreparedQuery> {
    match query {
        Query::Ocr { region } => {
            crate::screen_ocr::validate_region(region)?;
            Ok(PreparedQuery {
                kind: PreparedKind::Ocr { region },
            })
        }
        Query::Pixel { x, y } => {
            ensure!(
                u64::from(x) < screen::MAX_IMAGE_PIXELS as u64
                    && u64::from(y) < screen::MAX_IMAGE_PIXELS as u64,
                "Pixel coordinate exceeds the maximum capture extent"
            );
            Ok(PreparedQuery {
                kind: PreparedKind::Pixel { x, y },
            })
        }
        Query::ImageMatch {
            template_png_base64,
            region,
            min_score,
        } => {
            ensure!(
                template_png_base64.len() <= MAX_TEMPLATE_BASE64_BYTES,
                "Template exceeds the 256 KiB encoded limit"
            );
            validate_match_options(region, min_score)?;
            let png = base64::engine::general_purpose::STANDARD
                .decode(template_png_base64)
                .context("Template is not valid base64")?;
            let template = decode_png(
                &png,
                DecodeBounds {
                    width: screen::MAX_TEMPLATE_WIDTH,
                    height: screen::MAX_TEMPLATE_HEIGHT,
                    pixels: u64::from(screen::MAX_TEMPLATE_WIDTH)
                        * u64::from(screen::MAX_TEMPLATE_HEIGHT),
                    allocation: 4 * 1024 * 1024,
                    expected: None,
                },
            )
            .context("Invalid template PNG")?;
            PreparedQuery::image_match(template, region, min_score)
        }
    }
}

fn validate_match_options(region: Option<Region>, min_score: f64) -> Result<()> {
    ensure!(
        min_score.is_finite() && (0.0..=1.0).contains(&min_score),
        "Minimum score must be finite and between zero and one"
    );
    if let Some(region) = region {
        ensure!(
            region.width > 0 && region.height > 0,
            "Search region must have positive dimensions"
        );
        region
            .x
            .checked_add(region.width)
            .context("Search region x extent overflows")?;
        region
            .y
            .checked_add(region.height)
            .context("Search region y extent overflows")?;
        ensure!(
            u64::from(region.width) * u64::from(region.height) <= screen::MAX_IMAGE_PIXELS as u64,
            "Search region exceeds the maximum capture size"
        );
    }
    Ok(())
}

struct DecodeBounds {
    width: u32,
    height: u32,
    pixels: u64,
    allocation: u64,
    expected: Option<(u32, u32)>,
}
fn decode_png(bytes: &[u8], bounds: DecodeBounds) -> Result<RgbaImage> {
    // Inspect the fixed IHDR before any decoder or decompression allocation.
    ensure!(
        bytes.len() >= 33 && &bytes[..8] == PNG_SIGNATURE,
        "Expected a complete PNG header"
    );
    ensure!(
        bytes[8..12] == 13_u32.to_be_bytes() && &bytes[12..16] == b"IHDR",
        "Invalid PNG IHDR header"
    );
    let width = u32::from_be_bytes(bytes[16..20].try_into()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into()?);
    ensure!(
        width > 0 && height > 0 && width <= bounds.width && height <= bounds.height,
        "PNG dimensions exceed the permitted bounds"
    );
    ensure!(
        u64::from(width) * u64::from(height) <= bounds.pixels,
        "PNG exceeds the decoded pixel limit"
    );
    if let Some(expected) = bounds.expected {
        ensure!(
            (width, height) == expected,
            "PNG dimensions do not match captured frame metadata"
        );
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(bounds.width);
    limits.max_image_height = Some(bounds.height);
    limits.max_alloc = Some(bounds.allocation);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), ImageFormat::Png);
    reader.limits(limits);
    let decoded = reader.decode().context("PNG decoding failed")?.into_rgba8();
    ensure!(
        decoded.dimensions() == (width, height),
        "Decoded PNG dimensions changed"
    );
    Image::new(width, height, decoded.as_raw())?;
    Ok(decoded)
}

pub fn decode_frame(frame: &CapturedFrame) -> Result<RgbaImage> {
    ensure!(
        frame.png.len() <= MAX_FRAME_PNG_BYTES,
        "Captured PNG exceeds the connection budget"
    );
    ensure!(
        frame.width > 0 && frame.height > 0,
        "Captured frame dimensions must be positive"
    );
    ensure!(
        u64::from(frame.width) * u64::from(frame.height) <= screen::MAX_IMAGE_PIXELS as u64,
        "Captured frame exceeds the 16 Mi-pixel limit"
    );
    ensure!(
        frame.bounds.valid()
            && i64::from(frame.bounds.right) - i64::from(frame.bounds.left)
                == i64::from(frame.width)
            && i64::from(frame.bounds.bottom) - i64::from(frame.bounds.top)
                == i64::from(frame.height),
        "Captured frame bounds do not match its dimensions"
    );
    decode_png(
        &frame.png,
        DecodeBounds {
            width: screen::MAX_IMAGE_PIXELS as u32,
            height: screen::MAX_IMAGE_PIXELS as u32,
            pixels: screen::MAX_IMAGE_PIXELS as u64,
            allocation: 256 * 1024 * 1024,
            expected: Some((frame.width, frame.height)),
        },
    )
}

fn check_capture_cancel(cancel: Option<&std::sync::atomic::AtomicBool>) -> Result<()> {
    ensure!(
        cancel.is_none_or(|flag| !flag.load(std::sync::atomic::Ordering::Acquire)),
        "Selected-window observation was canceled"
    );
    Ok(())
}

/// Capture already-authorized selected client pixels. There is no PNG encoding
/// or protocol transport budget on this internal raw observation path.
pub(crate) fn capture_pixels(
    window: mux_core::routing::Window,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<(mux_core::desktop::Bounds, RgbaImage)> {
    check_capture_cancel(cancel)?;
    let (bounds, rgba) = mux_windows::target_capture::client(window)?;
    check_capture_cancel(cancel)?;
    let width = u32::try_from(i64::from(bounds.right) - i64::from(bounds.left))?;
    let height = u32::try_from(i64::from(bounds.bottom) - i64::from(bounds.top))?;
    Image::new(width, height, &rgba)?;
    let rgba =
        RgbaImage::from_raw(width, height, rgba).context("Captured pixel dimensions changed")?;
    Ok((bounds, rgba))
}

/// Existing GUI/MCP PNG contract. Authority is still checked by the caller
/// after all capture/analysis work returns; this function never authorizes.
pub(crate) fn capture_selected(
    window: mux_core::routing::Window,
    user_id: String,
    query: Option<&PreparedQuery>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<CapturedFrame> {
    let (bounds, rgba) = capture_pixels(window, cancel)?;
    let (width, height) = rgba.dimensions();
    let mut png = Cursor::new(Vec::new());
    image::write_buffer_with_format(
        &mut png,
        rgba.as_raw(),
        width,
        height,
        image::ColorType::Rgba8,
        ImageFormat::Png,
    )?;
    let png = png.into_inner();
    ensure!(
        png.len() <= MAX_FRAME_PNG_BYTES,
        "Captured PNG exceeds the connection budget"
    );
    let analysis = query
        .map(|query| analyze_pixels_with_cancel(bounds, &rgba, query, cancel))
        .transpose()?;
    check_capture_cancel(cancel)?;
    Ok(CapturedFrame {
        png,
        bounds,
        width,
        height,
        pid: window.pid,
        hwnd: window.hwnd,
        user_id,
        analysis,
    })
}

/// Coordinates refer to the captured client bounds. Results are observations
/// of this snapshot; they never select a target, click, or start automation.
#[cfg(test)]
pub fn analyze(frame: &CapturedFrame, query: &PreparedQuery) -> Result<Value> {
    let rgba = decode_frame(frame)?;
    analyze_pixels(frame.bounds, &rgba, query)
}

/// Shared bounded analysis of raw client pixels, independent of PNG transport.
#[cfg(test)]
pub(crate) fn analyze_pixels(
    bounds: mux_core::desktop::Bounds,
    rgba: &RgbaImage,
    query: &PreparedQuery,
) -> Result<Value> {
    analyze_pixels_with_cancel(bounds, rgba, query, None)
}

/// Cancellation remains supervised by the existing physical capture task.
pub(crate) fn analyze_pixels_with_cancel(
    bounds: mux_core::desktop::Bounds,
    rgba: &RgbaImage,
    query: &PreparedQuery,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Value> {
    ensure!(
        bounds.valid()
            && i64::from(bounds.right) - i64::from(bounds.left) == i64::from(rgba.width())
            && i64::from(bounds.bottom) - i64::from(bounds.top) == i64::from(rgba.height()),
        "Captured pixel bounds do not match dimensions"
    );
    let image = Image::new(rgba.width(), rgba.height(), rgba.as_raw())?;
    match &query.kind {
        PreparedKind::Ocr { region } => crate::screen_ocr::analyze(bounds, rgba, *region, cancel),
        PreparedKind::Pixel { x, y } => Ok(json!({
            "type":"pixel", "rgba":image.pixel(*x,*y)?,
            "client_position":[x,y],
            "desktop_position":[i64::from(bounds.left)+i64::from(*x),i64::from(bounds.top)+i64::from(*y)],
            "snapshot_only":true,
        })),
        PreparedKind::ImageMatch {
            template,
            region,
            min_score,
        } => {
            let template = Image::new(template.width(), template.height(), template.as_raw())?;
            let found = image.find(&template, *region, *min_score)?;
            let found = found.map(|found| json!({
                "top_left":found.top_left, "size":found.size, "center":found.center, "score":found.score,
                "desktop_top_left":[i64::from(bounds.left)+i64::from(found.top_left[0]),
                                    i64::from(bounds.top)+i64::from(found.top_left[1])],
                "desktop_center":[f64::from(bounds.left)+found.center[0],
                                  f64::from(bounds.top)+found.center[1]],
            }));
            Ok(
                json!({"type":"image_match", "method":"rgb_mean_absolute_similarity", "alpha":"ignored",
                      "min_score":min_score,"region":region,"match":found,"snapshot_only":true}),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mux_core::desktop::Bounds;
    #[test]
    fn ocr_query_preparation_is_pure_bounded_and_preserves_default_whole_client() {
        let query: Query = serde_json::from_value(json!({"type":"ocr"})).unwrap();
        assert!(matches!(
            prepare_query(query).unwrap().kind,
            PreparedKind::Ocr { region: None }
        ));
        for value in [
            json!({"type":"ocr","pid":42}),
            json!({"type":"ocr","language":"auto"}),
            json!({"type":"ocr","region":{"x":0,"y":0,"width":4097,"height":1}}),
            json!({"type":"ocr","region":{"x":0,"y":0,"width":1025,"height":1024}}),
        ] {
            assert!(match serde_json::from_value::<Query>(value) {
                Ok(query) => prepare_query(query).is_err(),
                Err(_) => true,
            });
        }
    }
    fn png(width: u32, height: u32, bytes: &[u8]) -> Vec<u8> {
        let mut output = Cursor::new(Vec::new());
        image::write_buffer_with_format(
            &mut output,
            bytes,
            width,
            height,
            image::ColorType::Rgba8,
            ImageFormat::Png,
        )
        .unwrap();
        output.into_inner()
    }
    fn frame() -> CapturedFrame {
        CapturedFrame {
            png: png(
                3,
                2,
                &[
                    0, 0, 0, 255, 1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 20, 40, 60, 77, 30, 50,
                    70, 88,
                ],
            ),
            bounds: Bounds {
                left: -10,
                top: 20,
                right: -7,
                bottom: 22,
            },
            width: 3,
            height: 2,
            pid: 42,
            hwnd: 99,
            user_id: "virtual-a".into(),
            analysis: None,
        }
    }
    #[test]
    fn raw_pixel_and_match_analysis_match_the_existing_png_frame_contract() {
        let frame = frame();
        let pixels = decode_frame(&frame).unwrap();
        let queries = [
            prepare_query(Query::Pixel { x: 1, y: 1 }).unwrap(),
            PreparedQuery::image_match(
                RgbaImage::from_raw(1, 1, vec![20, 40, 60, 255]).unwrap(),
                None,
                1.0,
            )
            .unwrap(),
        ];
        for query in queries {
            assert_eq!(
                analyze_pixels(frame.bounds, &pixels, &query).unwrap(),
                analyze(&frame, &query).unwrap()
            );
        }
        let wrong = Bounds {
            right: frame.bounds.right + 1,
            ..frame.bounds
        };
        assert!(
            analyze_pixels(
                wrong,
                &pixels,
                &prepare_query(Query::Pixel { x: 0, y: 0 }).unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn pixel_returns_exact_rgba_and_negative_desktop_origin_coordinates() {
        let frame = frame();
        let result = analyze(&frame, &prepare_query(Query::Pixel { x: 1, y: 1 }).unwrap()).unwrap();
        assert_eq!(result["rgba"], json!([20, 40, 60, 77]));
        assert_eq!(result["client_position"], json!([1, 1]));
        assert_eq!(result["desktop_position"], json!([-9, 21]));
        assert_eq!(result["snapshot_only"], true);
        assert!(analyze(&frame, &prepare_query(Query::Pixel { x: 3, y: 1 }).unwrap()).is_err());
    }
    #[test]
    fn matching_ignores_alpha_and_reports_client_and_desktop_geometry_or_null() {
        let frame = frame();
        let template = RgbaImage::from_raw(2, 1, vec![20, 40, 60, 0, 30, 50, 70, 0]).unwrap();
        let query = PreparedQuery::image_match(template, None, 1.0).unwrap();
        let result = analyze(&frame, &query).unwrap();
        assert_eq!(result["method"], "rgb_mean_absolute_similarity");
        assert_eq!(result["match"]["top_left"], json!([1, 1]));
        assert_eq!(result["match"]["size"], json!([2, 1]));
        assert_eq!(result["match"]["center"], json!([2.0, 1.5]));
        assert_eq!(result["match"]["desktop_top_left"], json!([-9, 21]));
        assert_eq!(result["match"]["desktop_center"], json!([-8.0, 21.5]));
        let absent = PreparedQuery::image_match(
            RgbaImage::from_raw(1, 1, vec![210, 211, 212, 0]).unwrap(),
            None,
            1.0,
        )
        .unwrap();
        assert!(analyze(&frame, &absent).unwrap()["match"].is_null());
        let outside = PreparedQuery::image_match(
            RgbaImage::new(1, 1),
            Some(Region {
                x: 3,
                y: 0,
                width: 1,
                height: 1,
            }),
            0.95,
        )
        .unwrap();
        assert!(analyze(&frame, &outside).is_err());
    }
    #[test]
    fn templates_reject_encoded_overflow_bad_base64_non_png_and_dimension_bombs() {
        let prepare = |value: String| {
            prepare_query(Query::ImageMatch {
                template_png_base64: value,
                region: None,
                min_score: 0.95,
            })
        };
        assert!(prepare("A".repeat(MAX_TEMPLATE_BASE64_BYTES + 1)).is_err());
        assert!(prepare("not base64!".into()).is_err());
        assert!(prepare(base64::engine::general_purpose::STANDARD.encode(b"not PNG")).is_err());
        for (w, h) in [(129, 1), (1, 129), (129, 129)] {
            assert!(
                prepare(base64::engine::general_purpose::STANDARD.encode(png(
                    w,
                    h,
                    &vec![0; w as usize * h as usize * 4]
                )))
                .is_err()
            );
        }
        let mut header = png(1, 1, &[0, 0, 0, 255]);
        header[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(prepare(base64::engine::general_purpose::STANDARD.encode(header)).is_err());
        assert!(
            prepare(base64::engine::general_purpose::STANDARD.encode(&frame().png[..33])).is_err()
        );
    }
    #[test]
    fn png_decoder_allocation_budget_is_applied_even_to_small_encoded_input() {
        let encoded = png(1, 1, &[1, 2, 3, 255]);
        assert!(
            decode_png(
                &encoded,
                DecodeBounds {
                    width: 128,
                    height: 128,
                    pixels: 16_384,
                    allocation: 1,
                    expected: None
                }
            )
            .is_err()
        );
    }
    #[test]
    fn frame_header_bounds_and_pixel_budget_are_checked_before_decode() {
        let mut frame = self::frame();
        frame.width = 4;
        frame.bounds.right = -6;
        assert!(
            decode_frame(&frame)
                .unwrap_err()
                .to_string()
                .contains("metadata")
        );
        let mut frame = self::frame();
        frame.bounds.top = 19;
        assert!(decode_frame(&frame).is_err());
        let mut frame = self::frame();
        frame.width = screen::MAX_IMAGE_PIXELS as u32;
        frame.height = 2;
        assert!(decode_frame(&frame).is_err());
        let mut frame = self::frame();
        frame.png.resize(MAX_FRAME_PNG_BYTES + 1, 0);
        assert!(decode_frame(&frame).is_err());
    }
    #[test]
    fn schema_and_match_options_reject_unsupported_fields_and_unbounded_work() {
        let query: Query =
            serde_json::from_value(json!({"type":"image_match","template_png_base64":""})).unwrap();
        assert!(matches!(query,Query::ImageMatch{min_score,..} if min_score==0.95));
        for value in [
            json!({"type":"pixel","x":-1,"y":0}),
            json!({"type":"pixel","x":0,"y":0,"click":true}),
            json!({"type":"image_match","template_png_base64":"","region":{"x":0,"y":0,"width":1,"height":1,"pid":42}}),
        ] {
            assert!(serde_json::from_value::<Query>(value).is_err());
        }
        for score in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
            assert!(PreparedQuery::image_match(RgbaImage::new(1, 1), None, score).is_err());
        }
        assert!(
            PreparedQuery::image_match(
                RgbaImage::new(1, 1),
                Some(Region {
                    x: u32::MAX,
                    y: 0,
                    width: 2,
                    height: 1
                }),
                0.95
            )
            .is_err()
        );
        assert!(
            PreparedQuery::image_match(
                RgbaImage::new(1, 1),
                Some(Region {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 1
                }),
                0.95
            )
            .is_err()
        );
        let query = PreparedQuery::image_match(RgbaImage::new(128, 128), None, 0.95).unwrap();
        let mut large = frame();
        large.width = 256;
        large.height = 256;
        large.bounds = Bounds {
            left: 0,
            top: 0,
            right: 256,
            bottom: 256,
        };
        large.png = png(256, 256, &vec![0; 256 * 256 * 4]);
        assert!(
            analyze(&large, &query)
                .unwrap_err()
                .to_string()
                .contains("budget")
        );
    }
}
