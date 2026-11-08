//! Local Latin OCR of one authorized snapshot. Only fixed bundled models are read.
//! Call capability/model initialization and recognition from a supervised worker.
use anyhow::{Context, Result, ensure};
use image::RgbaImage;
use mux_core::{
    desktop::Bounds,
    mapper::MAX_TEXT_BYTES,
    screen::{Image, Region},
};
use ocrs::{ImageSource, OcrEngine, OcrEngineParams, TextItem};
use rten_imageproc::{
    Rect, RetrievalMode, RotatedRect, bounding_rect, find_contours, min_area_rect, simplify_polygon,
};
use rten_tensor::{NdTensorView, prelude::*};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

pub const MAX_OCR_PIXELS: u64 = 1_048_576;
pub const MAX_OCR_WORDS: usize = 256;
pub const MAX_OCR_LINES: usize = 64;
pub const MAX_OCR_DIMENSION: u32 = 4096;
// ocrs 0.12.2 uses the model's fixed recognition height (64 for these
// weights), and clamps normalized width to 2400. Reject excessive width
// BEFORE recognition instead of relying on that lossy width clamp.
const MAX_RECOGNITION_WIDTH: u32 = 2400;
// Deliberate deviation from ocrs's empirical 0.2 default: use its documented
// neutral sigmoid decision boundary. With these exact weights, the default
// labels most of a blank white image as text. This decision happens before
// recognition; recognized tokens are never filtered and callers cannot tune
// the threshold. Original pixels and library preprocessing are intact.
const TEXT_PROBABILITY: f32 = 0.5;
// The small attributed component adapter below is used under MIT. Keep the
// complete permission notice in the source/distribution input closure.
const _: &str = include_str!("../notices/ocrs-MIT.txt");
const DETECTION: ModelSpec = ModelSpec {
    name: "text-detection-ssfbcj81.rten",
    bytes: 2_523_564,
    sha256: "614aafabf27c94d386f7aa036c967c2e47e4b9938fa11531ca8f5698c1ca4c36",
};
const RECOGNITION: ModelSpec = ModelSpec {
    name: "text-rec-checkpoint-s52qdbqt.rten",
    bytes: 9_716_444,
    sha256: "606d9a0414c6b73c99df75b707c11c70d1c8b12e1d4f900922e185fc37bfca65",
};
struct ModelSpec {
    name: &'static str,
    bytes: usize,
    sha256: &'static str,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ModelGeometry {
    recognition_height: u32,
}
impl ModelGeometry {
    fn from_models(detection: &rten::Model, recognition: &rten::Model) -> Result<Self> {
        ensure!(
            detection.input_ids().len() == 1 && recognition.input_ids().len() == 1,
            "Bundled OCR models must each declare one input"
        );
        let shape = |model: &rten::Model| {
            model
                .input_ids()
                .first()
                .copied()
                .and_then(|id| model.node_info(id))
                .and_then(|node| node.shape())
        };
        Self::from_shapes(shape(detection).as_deref(), shape(recognition).as_deref())
    }
    fn from_shapes(
        detection: Option<&[rten::Dimension]>,
        recognition: Option<&[rten::Dimension]>,
    ) -> Result<Self> {
        use rten::Dimension::{Fixed, Symbolic};
        let (
            Some([batch, Fixed(1), Fixed(height), Fixed(width)]),
            Some(
                [
                    recognition_batch,
                    Fixed(1),
                    Fixed(recognition_height),
                    Symbolic(sequence),
                ],
            ),
        ) = (detection, recognition)
        else {
            anyhow::bail!("Bundled OCR model input shapes are invalid");
        };
        let valid_batch = |dimension: &rten::Dimension| {
            matches!(dimension, Fixed(1)) || matches!(dimension,Symbolic(name) if !name.is_empty())
        };
        ensure!(
            valid_batch(batch) && valid_batch(recognition_batch) && !sequence.is_empty(),
            "Bundled OCR model input shapes are invalid"
        );
        // These geometry values belong to the exact verified weights, not a
        // general fallback for arbitrary models. A changed bundle needs review.
        ensure!(
            (*height, *width, *recognition_height) == (800, 600, 64),
            "Bundled OCR model geometry does not match the trusted weights"
        );
        Ok(Self {
            recognition_height: *recognition_height as u32,
        })
    }
}
struct Engine {
    ocr: OcrEngine,
    geometry: ModelGeometry,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    pub available: bool,
    pub language: String,
    pub reason: Option<String>,
}
/// Bounded model verification only, without parsing, capture, or inference.
/// This performs filesystem I/O and must be invoked on a worker, never in a UI draw.
pub fn capability() -> Capability {
    let checked = (|| {
        if let Some(cache) = ENGINE.get() {
            let cached = cache
                .lock()
                .map_err(|_| anyhow::anyhow!("OCR model initialization is unavailable"))?;
            // The process owns a verified in-memory copy even if files change later.
            if cached.is_some() {
                return Ok(());
            }
        }
        let directory = model_directory()?;
        verified_model(&directory, &DETECTION)?;
        verified_model(&directory, &RECOGNITION)?;
        Ok::<_, anyhow::Error>(())
    })();
    Capability {
        available: checked.is_ok(),
        language: "Latin".into(),
        reason: checked.err().map(|error| error.to_string()),
    }
}
fn directory_for_executable(executable: &Path) -> Result<PathBuf> {
    let directory = executable
        .parent()
        .context("OCR executable directory is unavailable")?;
    // Unit-test executables live in profile/deps. Production never searches parents.
    #[cfg(test)]
    let directory = if directory.file_name().is_some_and(|name| name == "deps") {
        directory
            .parent()
            .context("OCR test profile directory is unavailable")?
    } else {
        directory
    };
    Ok(directory.join("assets").join("ocr"))
}
fn model_directory() -> Result<PathBuf> {
    directory_for_executable(
        &std::env::current_exe().context("OCR executable location is unavailable")?,
    )
}
fn verified_model(directory: &Path, specification: &ModelSpec) -> Result<Vec<u8>> {
    let file = File::open(directory.join(specification.name))
        .map_err(|_| anyhow::anyhow!("Bundled OCR model {} is unavailable", specification.name))?;
    ensure!(
        file.metadata().map(|metadata| metadata.len()).ok() == Some(specification.bytes as u64),
        "Bundled OCR model {} has an invalid length",
        specification.name
    );
    let mut bytes = Vec::with_capacity(specification.bytes);
    file.take(specification.bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            anyhow::anyhow!("Bundled OCR model {} could not be read", specification.name)
        })?;
    ensure!(
        bytes.len() == specification.bytes && sha256(&bytes)? == specification.sha256,
        "Bundled OCR model {} failed integrity verification",
        specification.name
    );
    Ok(bytes)
}
fn sha256(bytes: &[u8]) -> Result<String> {
    use windows_sys::Win32::Security::Cryptography::{
        BCRYPT_SHA256_ALGORITHM, BCryptCloseAlgorithmProvider, BCryptHash,
        BCryptOpenAlgorithmProvider,
    };
    let mut algorithm = std::ptr::null_mut();
    let opened = unsafe {
        BCryptOpenAlgorithmProvider(&mut algorithm, BCRYPT_SHA256_ALGORITHM, std::ptr::null(), 0)
    };
    ensure!(opened >= 0, "OCR integrity provider is unavailable");
    let mut digest = [0_u8; 32];
    let result = unsafe {
        BCryptHash(
            algorithm,
            std::ptr::null(),
            0,
            bytes.as_ptr(),
            bytes.len() as u32,
            digest.as_mut_ptr(),
            digest.len() as u32,
        )
    };
    unsafe {
        BCryptCloseAlgorithmProvider(algorithm, 0);
    }
    ensure!(result >= 0, "OCR integrity verification failed");
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

// Only immutable model weights are cached. No frames, text, or actor data enter this cache.
static ENGINE: OnceLock<Mutex<Option<Arc<Engine>>>> = OnceLock::new();
fn engine(cancel: Option<&AtomicBool>) -> Result<Arc<Engine>> {
    initialized(ENGINE.get_or_init(|| Mutex::new(None)), cancel, || {
        let directory = model_directory()?;
        // Parsing consumes these SAME verified buffers; no path-based reopen or mmap.
        let detection = rten::Model::load(verified_model(&directory, &DETECTION)?)
            .map_err(|_| anyhow::anyhow!("Bundled OCR detection model could not be loaded"))?;
        canceled(cancel)?;
        let recognition = rten::Model::load(verified_model(&directory, &RECOGNITION)?)
            .map_err(|_| anyhow::anyhow!("Bundled OCR recognition model could not be loaded"))?;
        canceled(cancel)?;
        let geometry = ModelGeometry::from_models(&detection, &recognition)?;
        let ocr = OcrEngine::new(OcrEngineParams {
            detection_model: Some(detection),
            recognition_model: Some(recognition),
            debug: false,
            ..Default::default()
        })
        .map_err(|_| anyhow::anyhow!("Bundled Latin OCR engine could not be initialized"))?;
        Ok(Engine { ocr, geometry })
    })
}
fn initialized<T>(
    cache: &Mutex<Option<Arc<T>>>,
    cancel: Option<&AtomicBool>,
    initialize: impl FnOnce() -> Result<T>,
) -> Result<Arc<T>> {
    canceled(cancel)?;
    let mut cached = cache
        .lock()
        .map_err(|_| anyhow::anyhow!("OCR model initialization is unavailable"))?;
    canceled(cancel)?;
    if let Some(engine) = cached.as_ref() {
        return Ok(engine.clone());
    }
    // Failed reads/initialization never enter the cache. Correcting a bundle
    // permits a subsequent worker request to retry without restarting the app.
    let engine = Arc::new(initialize()?);
    canceled(cancel)?;
    *cached = Some(engine.clone());
    Ok(engine)
}
fn canceled(cancel: Option<&AtomicBool>) -> Result<()> {
    ensure!(
        cancel.is_none_or(|flag| !flag.load(Ordering::Acquire)),
        "OCR observation was canceled"
    );
    Ok(())
}
pub fn validate_region(region: Option<Region>) -> Result<()> {
    if let Some(region) = region {
        ensure!(
            region.width > 0 && region.height > 0,
            "OCR region must have positive dimensions"
        );
        ensure!(
            region.width <= MAX_OCR_DIMENSION && region.height <= MAX_OCR_DIMENSION,
            "OCR region dimensions must be at most 4096 pixels"
        );
        region
            .x
            .checked_add(region.width)
            .context("OCR region x extent overflows")?;
        region
            .y
            .checked_add(region.height)
            .context("OCR region y extent overflows")?;
        ensure!(
            u64::from(region.width) * u64::from(region.height) <= MAX_OCR_PIXELS,
            "OCR region exceeds the 1 Mi-pixel limit"
        );
    }
    Ok(())
}
#[derive(Clone)]
struct Glyph {
    character: char,
    bounds: [i32; 4],
}
type Line = Vec<Glyph>;

// Adapted from ocrs 0.12.2 detection.rs::find_connected_component_rects,
// commit 2dbc1f840e47d45630ef6060499138bf597a9f65.
// Copyright (c) 2023 Ocrs project contributors. Used under MIT; the full
// notice is in ../notices/ocrs-MIT.txt. Contour mode, epsilon, expansion and
// minimum area retain upstream semantics; only TEXT_PROBABILITY is deliberate.
fn mask_words(
    mask: NdTensorView<f32, 2>,
    dimensions: (u32, u32),
    cancel: Option<&AtomicBool>,
) -> Result<Vec<RotatedRect>> {
    canceled(cancel)?;
    let (width, height) = dimensions;
    validate_region(Some(Region {
        x: 0,
        y: 0,
        width,
        height,
    }))?;
    ensure!(
        mask.shape() == [height as usize, width as usize],
        "OCR detector mask dimensions changed"
    );
    for (index, value) in mask.iter().enumerate() {
        if index.is_multiple_of(4096) {
            canceled(cancel)?;
        }
        ensure!(
            value.is_finite() && (0.0..=1.0).contains(value),
            "OCR detector returned invalid probabilities"
        );
    }
    canceled(cancel)?;
    let binary = mask.map(|probability| *probability > TEXT_PROBABILITY);
    let mut words = Vec::new();
    let contours = find_contours(binary.view(), RetrievalMode::External);
    for polygon in contours.iter() {
        canceled(cancel)?;
        let points: Vec<_> = polygon.iter().map(|point| point.to_f32()).collect();
        let simplified = simplify_polygon(&points, 2.0);
        if let Some(mut rectangle) = min_area_rect(&simplified) {
            rectangle.resize(
                rectangle.width() + 2.0 * 3.0,
                rectangle.height() + 2.0 * 3.0,
            );
            if rectangle.area() >= 100.0 {
                ensure!(
                    words.len() < MAX_OCR_WORDS,
                    "OCR detected more than 256 words"
                );
                words.push(rectangle);
            }
        }
    }
    Ok(words)
}

fn recognize(pixels: &RgbaImage, cancel: Option<&AtomicBool>) -> Result<Vec<Line>> {
    canceled(cancel)?;
    let engine = engine(cancel)?;
    canceled(cancel)?;
    let source = ImageSource::from_bytes(pixels.as_raw(), pixels.dimensions())
        .map_err(|_| anyhow::anyhow!("OCR input dimensions are invalid"))?;
    let input = engine
        .ocr
        .prepare_input(source)
        .map_err(|_| anyhow::anyhow!("OCR input preparation failed"))?;
    canceled(cancel)?;
    let mask = engine
        .ocr
        .detect_text_pixels(&input)
        .map_err(|_| anyhow::anyhow!("OCR text detection failed"))?;
    let words = mask_words(mask.view(), pixels.dimensions(), cancel)?;
    for word in &words {
        ensure!(
            word.corners()
                .iter()
                .all(|point| point.x.is_finite() && point.y.is_finite()),
            "OCR detection returned invalid geometry"
        );
    }
    canceled(cancel)?;
    let lines = engine.ocr.find_text_lines(&input, &words);
    ensure!(
        lines.len() <= MAX_OCR_LINES,
        "OCR detected more than 64 lines"
    );
    let mut result = Vec::with_capacity(lines.len());
    let mut bytes = 0_usize;
    for line in lines {
        canceled(cancel)?;
        // Match the pinned recognizer exactly: its width normalization uses
        // the UNCLIPPED integral bounding rectangle, not an ROI-clipped box.
        let rectangle = bounding_rect(line.iter())
            .context("OCR line has no words")?
            .integral_bounding_rect();
        validate_line_aspect(rectangle, engine.geometry.recognition_height)?;
        let recognized = engine
            .ocr
            .recognize_text(&input, std::slice::from_ref(&line))
            .map_err(|_| anyhow::anyhow!("OCR text recognition failed"))?;
        for line in recognized.into_iter().flatten() {
            // Check every character BEFORE constructing any response strings.
            let line_bytes = line
                .chars()
                .iter()
                .map(|glyph| glyph.char.len_utf8())
                .sum::<usize>();
            bytes = bytes
                .checked_add(line_bytes + usize::from(!result.is_empty()))
                .context("OCR text length overflows")?;
            ensure!(
                bytes <= MAX_TEXT_BYTES,
                "OCR text exceeds the 4096-byte limit"
            );
            let glyphs = line
                .chars()
                .iter()
                .map(|glyph| Glyph {
                    character: glyph.char,
                    bounds: [
                        glyph.rect.left(),
                        glyph.rect.top(),
                        glyph.rect.right(),
                        glyph.rect.bottom(),
                    ],
                })
                .collect();
            result.push(glyphs);
        }
    }
    canceled(cancel)?;
    Ok(result)
}
fn validate_line_aspect(rectangle: Rect<i32>, recognition_height: u32) -> Result<()> {
    let width = i64::from(rectangle.right()) - i64::from(rectangle.left());
    let height = i64::from(rectangle.bottom()) - i64::from(rectangle.top());
    ensure!(
        width > 0 && height > 0 && recognition_height > 0,
        "OCR line has invalid geometry"
    );
    let normalized = width
        .checked_mul(i64::from(recognition_height))
        .context("OCR line width overflows")?;
    ensure!(
        normalized <= i64::from(MAX_RECOGNITION_WIDTH) * height,
        "OCR line exceeds the recognition aspect limit"
    );
    Ok(())
}

pub(crate) fn analyze(
    bounds: Bounds,
    pixels: &RgbaImage,
    region: Option<Region>,
    cancel: Option<&AtomicBool>,
) -> Result<Value> {
    analyze_with(bounds, pixels, region, cancel, recognize)
}
fn analyze_with(
    bounds: Bounds,
    pixels: &RgbaImage,
    region: Option<Region>,
    cancel: Option<&AtomicBool>,
    recognize: impl FnOnce(&RgbaImage, Option<&AtomicBool>) -> Result<Vec<Line>>,
) -> Result<Value> {
    canceled(cancel)?;
    ensure!(
        bounds.valid()
            && i64::from(bounds.right) - i64::from(bounds.left) == i64::from(pixels.width())
            && i64::from(bounds.bottom) - i64::from(bounds.top) == i64::from(pixels.height()),
        "OCR frame bounds do not match its dimensions"
    );
    let image = Image::new(pixels.width(), pixels.height(), pixels.as_raw())?;
    let region = region.unwrap_or(Region {
        x: 0,
        y: 0,
        width: pixels.width(),
        height: pixels.height(),
    });
    validate_region(Some(region))?;
    // Strict crop validation precedes model loading/inference. Oversized frames are not resized.
    let cropped = image.crop(region)?;
    let cropped = RgbaImage::from_raw(region.width, region.height, cropped)
        .context("OCR crop dimensions changed")?;
    canceled(cancel)?;
    let lines = recognize(&cropped, cancel)?;
    canceled(cancel)?;
    format_result(bounds, region, &lines)
}
fn text(glyphs: &[Glyph]) -> String {
    glyphs.iter().map(|glyph| glyph.character).collect()
}
fn box_for(glyphs: &[Glyph], region: Region, bounds: Bounds) -> Result<([u32; 4], [i64; 4])> {
    ensure!(!glyphs.is_empty(), "OCR returned an empty text item");
    let mut rect = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    for glyph in glyphs {
        let [left, top, right, bottom] = glyph.bounds;
        ensure!(
            right > left && bottom > top,
            "OCR returned invalid text geometry"
        );
        rect[0] = rect[0].min(left);
        rect[1] = rect[1].min(top);
        rect[2] = rect[2].max(right);
        rect[3] = rect[3].max(bottom);
    }
    let left = rect[0].clamp(0, region.width as i32) as u32;
    let top = rect[1].clamp(0, region.height as i32) as u32;
    let right = rect[2].clamp(0, region.width as i32) as u32;
    let bottom = rect[3].clamp(0, region.height as i32) as u32;
    ensure!(
        right > left && bottom > top,
        "OCR text geometry lies outside the region"
    );
    let client = [region.x + left, region.y + top, right - left, bottom - top];
    let desktop = [
        i64::from(bounds.left) + i64::from(client[0]),
        i64::from(bounds.top) + i64::from(client[1]),
        i64::from(client[2]),
        i64::from(client[3]),
    ];
    Ok((client, desktop))
}
fn format_result(bounds: Bounds, region: Region, lines: &[Line]) -> Result<Value> {
    ensure!(
        lines.len() <= MAX_OCR_LINES,
        "OCR returned more than 64 lines"
    );
    let mut bytes = lines.len().saturating_sub(1);
    let mut word_count = 0_usize;
    for line in lines {
        ensure!(!line.is_empty(), "OCR returned an empty line");
        for glyph in line {
            ensure!(
                !glyph.character.is_control() || matches!(glyph.character, '\t' | '\r' | '\n'),
                "OCR returned an unsupported control character"
            );
            bytes = bytes
                .checked_add(glyph.character.len_utf8())
                .context("OCR text length overflows")?;
            ensure!(
                bytes <= MAX_TEXT_BYTES,
                "OCR text exceeds the 4096-byte limit"
            );
        }
        word_count += line
            .split(|glyph| glyph.character == ' ')
            .filter(|word| !word.is_empty())
            .count();
        ensure!(
            word_count <= MAX_OCR_WORDS,
            "OCR returned more than 256 words"
        );
    }
    let mut full_text = String::with_capacity(bytes);
    let mut formatted_lines = Vec::with_capacity(lines.len());
    for line in lines {
        let line_text = text(line);
        if !full_text.is_empty() {
            full_text.push('\n');
        }
        full_text.push_str(&line_text);
        let (client_box, desktop_box) = box_for(line, region, bounds)?;
        let mut words = Vec::new();
        for word in line
            .split(|glyph| glyph.character == ' ')
            .filter(|word| !word.is_empty())
        {
            let (client_box, desktop_box) = box_for(word, region, bounds)?;
            words
                .push(json!({"text":text(word),"client_box":client_box,"desktop_box":desktop_box}));
        }
        formatted_lines.push(json!({"text":line_text,"client_box":client_box,"desktop_box":desktop_box,"words":words}));
    }
    Ok(
        json!({"type":"ocr","provider":"ocrs","language":"Latin","text":full_text,"lines":formatted_lines,
        "client_region":region,"desktop_region":[i64::from(bounds.left)+i64::from(region.x),
            i64::from(bounds.top)+i64::from(region.y),i64::from(region.width),i64::from(region.height)],
        "snapshot_only":true}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn trusted_geometry() -> ModelGeometry {
        use rten::Dimension::{Fixed, Symbolic};
        ModelGeometry::from_shapes(
            Some(&[Symbolic("batch".into()), Fixed(1), Fixed(800), Fixed(600)]),
            Some(&[
                Symbolic("batch".into()),
                Fixed(1),
                Fixed(64),
                Symbolic("seq".into()),
            ]),
        )
        .unwrap()
    }
    #[test]
    fn trusted_model_geometry_rejects_missing_dynamic_or_malformed_shapes() {
        use rten::Dimension::{Fixed, Symbolic};
        let detector = vec![Symbolic("batch".into()), Fixed(1), Fixed(800), Fixed(600)];
        let recognizer = vec![
            Symbolic("batch".into()),
            Fixed(1),
            Fixed(64),
            Symbolic("seq".into()),
        ];
        assert_eq!(
            trusted_geometry(),
            ModelGeometry {
                recognition_height: 64
            }
        );
        assert!(ModelGeometry::from_shapes(None, Some(&recognizer)).is_err());
        assert!(ModelGeometry::from_shapes(Some(&detector), None).is_err());
        assert!(ModelGeometry::from_shapes(Some(&detector[..3]), Some(&recognizer)).is_err());
        for (index, value) in [
            (0, Fixed(2)),
            (1, Fixed(3)),
            (2, Symbolic("height".into())),
            (2, Fixed(0)),
            (3, Fixed(usize::MAX)),
            (2, Fixed(801)),
            (0, Symbolic(String::new())),
        ] {
            let mut shape = detector.clone();
            shape[index] = value;
            assert!(ModelGeometry::from_shapes(Some(&shape), Some(&recognizer)).is_err());
        }
        for (index, value) in [
            (1, Fixed(3)),
            (2, Symbolic("height".into())),
            (2, Fixed(50)),
            (2, Fixed(0)),
            (3, Fixed(2400)),
            (3, Symbolic(String::new())),
        ] {
            let mut shape = recognizer.clone();
            shape[index] = value;
            assert!(ModelGeometry::from_shapes(Some(&detector), Some(&shape)).is_err());
        }
    }
    #[test]
    fn detector_mask_rejects_invalid_shape_probability_and_canceled_work() {
        use rten_tensor::NdTensor;
        let mut mask = NdTensor::<f32, 2>::zeros([16, 16]);
        assert!(mask_words(mask.view(), (8, 32), None).is_err());
        for invalid in [f32::NAN, f32::INFINITY, -0.01, 1.01] {
            mask[[2, 3]] = invalid;
            assert!(mask_words(mask.view(), (16, 16), None).is_err());
        }
        mask[[2, 3]] = 0.0;
        assert!(mask_words(mask.view(), (16, 16), Some(&AtomicBool::new(true))).is_err());
    }
    #[test]
    fn neutral_probability_boundary_and_word_budget_are_enforced_before_recognition() {
        use rten_tensor::NdTensor;
        let mut mask = NdTensor::full([16, 16], 0.5_f32);
        assert!(mask_words(mask.view(), (16, 16), None).unwrap().is_empty());
        for y in 4..12 {
            for x in 4..12 {
                mask[[y, x]] = f32::from_bits(0.5_f32.to_bits() + 1);
            }
        }
        assert_eq!(mask_words(mask.view(), (16, 16), None).unwrap().len(), 1);
        let mut crowded = NdTensor::<f32, 2>::zeros([196, 208]);
        let paint = |mask: &mut NdTensor<f32, 2>, index: usize| {
            let x = 4 + (index % 17) * 12;
            let y = 4 + (index / 17) * 12;
            for dy in 0..6 {
                for dx in 0..6 {
                    mask[[y + dy, x + dx]] = 1.0;
                }
            }
        };
        for index in 0..MAX_OCR_WORDS {
            paint(&mut crowded, index);
        }
        assert_eq!(
            mask_words(crowded.view(), (208, 196), None).unwrap().len(),
            MAX_OCR_WORDS
        );
        paint(&mut crowded, MAX_OCR_WORDS);
        assert!(mask_words(crowded.view(), (208, 196), None).is_err());
    }
    #[test]
    fn recognition_width_guard_uses_unclipped_integral_pixel_geometry() {
        assert!(validate_line_aspect(Rect::from_tlhw(0, 0, 2, 75), 64).is_ok());
        assert!(validate_line_aspect(Rect::from_tlhw(0, 0, 2, 76), 64).is_err());
        assert!(validate_line_aspect(Rect::from_tlhw(0, 0, 1, 2), 0).is_err());
        let line = [RotatedRect::from_rect(Rect::from_tlbr(
            0.0, -80.0, 2.0, 20.0,
        ))];
        let rectangle = bounding_rect(line.iter()).unwrap().integral_bounding_rect();
        assert_eq!(rectangle.left(), -80);
        assert!(
            validate_line_aspect(rectangle, 64).is_err(),
            "ROI clipping would hide the excessive normalized line width"
        );
    }
    #[test]
    fn actual_bundled_models_read_clean_captured_line_without_extra_text() {
        // Exact owned target capture from diagnostic003, SHA-256
        // 1a12a6519b056d09e7addae5c4849f81bfbf2e5724051caacecc46a61922e941.
        // This invokes the production analyze/recognize path with no fixture
        // preprocessing or result filtering. Only the assertion normalizes
        // whitespace, so extra tokens and a missing internal word break fail.
        let encoded = include_bytes!("../tests/fixtures/ocr-clean-line.png");
        let pixels = image::load_from_memory_with_format(encoded, image::ImageFormat::Png)
            .unwrap()
            .into_rgba8();
        assert_eq!(pixels.dimensions(), (524, 181));
        let bounds = Bounds {
            left: -30,
            top: 40,
            right: 494,
            bottom: 221,
        };
        let region = Region {
            x: 16,
            y: 16,
            width: 480,
            height: 120,
        };
        let result = analyze(bounds, &pixels, Some(region), None).unwrap();
        let lines = result["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 1, "OCR of the clean captured line: {result}");
        let tokens: Vec<_> = result["text"]
            .as_str()
            .unwrap()
            .split_whitespace()
            .collect();
        assert_eq!(
            tokens,
            ["MUX", "OCR", "7319"],
            "OCR of the clean captured line: {result}"
        );
        assert_eq!(
            lines[0]["text"]
                .as_str()
                .unwrap()
                .split_whitespace()
                .collect::<Vec<_>>(),
            tokens
        );
        assert_eq!(result["client_region"], json!(region));
        assert_eq!(result["desktop_region"], json!([-14, 56, 480, 120]));
        for item in std::iter::once(&lines[0]).chain(lines[0]["words"].as_array().unwrap().iter()) {
            let client: [u32; 4] = serde_json::from_value(item["client_box"].clone()).unwrap();
            let desktop: [i64; 4] = serde_json::from_value(item["desktop_box"].clone()).unwrap();
            assert!(client[2] > 0 && client[3] > 0);
            assert!(client[0] >= region.x && client[1] >= region.y);
            assert!(
                client[0] + client[2] <= region.x + region.width
                    && client[1] + client[3] <= region.y + region.height
            );
            assert_eq!(
                desktop,
                [
                    i64::from(bounds.left) + i64::from(client[0]),
                    i64::from(bounds.top) + i64::from(client[1]),
                    i64::from(client[2]),
                    i64::from(client[3])
                ]
            );
        }
    }
    #[test]
    fn actual_bundled_models_read_larger_than_detector_roi_without_coordinate_rescaling() {
        let original = image::load_from_memory_with_format(
            include_bytes!("../tests/fixtures/ocr-clean-line.png"),
            image::ImageFormat::Png,
        )
        .unwrap()
        .into_rgba8();
        let mut pixels = RgbaImage::from_pixel(900, 900, image::Rgba([255, 255, 255, 255]));
        image::imageops::replace(&mut pixels, &original, 0, 0);
        assert_eq!(
            image::imageops::crop_imm(&pixels, 0, 0, 524, 181).to_image(),
            original
        );
        let bounds = Bounds {
            left: -30,
            top: 40,
            right: 870,
            bottom: 940,
        };
        // This exceeds both detector axes. The upstream detector resizes
        // internally; app capture pixels and returned coordinates remain native.
        let result = analyze(bounds, &pixels, None, None).unwrap();
        let lines = result["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 1, "OCR of larger ROI: {result}");
        assert_eq!(
            result["text"]
                .as_str()
                .unwrap()
                .split_whitespace()
                .collect::<Vec<_>>(),
            ["MUX", "OCR", "7319"],
            "OCR of larger ROI: {result}"
        );
        assert_eq!(
            result["client_region"],
            json!({"x":0,"y":0,"width":900,"height":900})
        );
        for item in std::iter::once(&lines[0]).chain(lines[0]["words"].as_array().unwrap().iter()) {
            let client: [u32; 4] = serde_json::from_value(item["client_box"].clone()).unwrap();
            let desktop: [i64; 4] = serde_json::from_value(item["desktop_box"].clone()).unwrap();
            assert!(client[2] > 0 && client[3] > 0);
            // Text lies in the original captured region, not stretched to
            // detector dimensions or the whole 900x900 canvas.
            assert!(
                client[0] >= 16
                    && client[1] >= 16
                    && client[0] + client[2] <= 496
                    && client[1] + client[3] <= 136,
                "larger ROI returned scaled or unrelated geometry: {result}"
            );
            assert_eq!(
                desktop,
                [
                    i64::from(bounds.left) + i64::from(client[0]),
                    i64::from(bounds.top) + i64::from(client[1]),
                    i64::from(client[2]),
                    i64::from(client[3])
                ]
            );
        }
    }
    #[test]
    fn actual_bundled_models_read_blank_native_canvas_as_empty() {
        let pixels = RgbaImage::from_pixel(600, 800, image::Rgba([255, 255, 255, 255]));
        let result = analyze(
            Bounds {
                left: 0,
                top: 0,
                right: 600,
                bottom: 800,
            },
            &pixels,
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            result["text"], "",
            "blank OCR must not invent text: {result}"
        );
        assert_eq!(
            result["lines"],
            json!([]),
            "blank OCR must not invent lines: {result}"
        );
    }
    #[test]
    fn model_cache_retries_failed_initialization_and_caches_only_success_without_frames() {
        let cache = Mutex::new(None);
        assert!(initialized::<u32>(&cache, None, || anyhow::bail!("missing model")).is_err());
        assert!(cache.lock().unwrap().is_none());
        let model = initialized(&cache, None, || Ok(7_u32)).unwrap();
        assert_eq!(*model, 7);
        let same = initialized(&cache, None, || {
            panic!("successful immutable model cache was not reused")
        })
        .unwrap();
        assert!(Arc::ptr_eq(&model, &same));
        let cancel = AtomicBool::new(true);
        assert!(initialized(&cache, Some(&cancel), || Ok(8)).is_err());
        let fresh = Mutex::new(None);
        let cancel = AtomicBool::new(false);
        assert!(
            initialized(&fresh, Some(&cancel), || {
                cancel.store(true, Ordering::Release);
                Ok(9)
            })
            .is_err()
        );
        assert!(fresh.lock().unwrap().is_none());
    }
    fn glyphs(value: &str) -> Line {
        value
            .chars()
            .map(|character| Glyph {
                character,
                bounds: [-1, 0, 3, 4],
            })
            .collect()
    }
    fn bounds() -> Bounds {
        Bounds {
            left: -10,
            top: 20,
            right: 0,
            bottom: 30,
        }
    }
    fn region() -> Region {
        Region {
            x: 2,
            y: 3,
            width: 5,
            height: 5,
        }
    }
    #[test]
    fn bounded_fake_recognition_reports_actual_roi_and_clipped_client_desktop_boxes() {
        let result = analyze_with(
            bounds(),
            &RgbaImage::new(10, 10),
            Some(region()),
            None,
            |pixels, _| {
                assert_eq!(pixels.dimensions(), (5, 5));
                Ok(vec![glyphs("Ready now"), glyphs("OK")])
            },
        )
        .unwrap();
        assert_eq!(result["text"], "Ready now\nOK");
        assert_eq!(result["client_region"], json!(region()));
        assert_eq!(result["desktop_region"], json!([-8, 23, 5, 5]));
        assert_eq!(result["lines"][0]["client_box"], json!([2, 3, 3, 4]));
        assert_eq!(
            result["lines"][0]["words"][1]["desktop_box"],
            json!([-8, 23, 3, 4])
        );
        assert_eq!(result["snapshot_only"], true);
    }
    #[test]
    fn empty_read_is_empty_and_overflow_controls_and_invalid_geometry_are_errors() {
        assert_eq!(format_result(bounds(), region(), &[]).unwrap()["text"], "");
        for lines in [
            vec![glyphs(&"x".repeat(MAX_TEXT_BYTES + 1))],
            vec![glyphs("private\0value")],
            vec![glyphs("x"); MAX_OCR_LINES + 1],
            vec![glyphs(&"x ".repeat(MAX_OCR_WORDS + 1))],
            vec![vec![Glyph {
                character: 'x',
                bounds: [5, 5, 6, 6],
            }]],
        ] {
            let error = format_result(bounds(), region(), &lines)
                .unwrap_err()
                .to_string();
            assert!(!error.contains("private"));
        }
    }
    #[test]
    fn roi_and_cancel_reject_before_provider_and_full_frame_does_not_downscale() {
        let pixels = RgbaImage::new(1025, 1024);
        let bounds = Bounds {
            left: 0,
            top: 0,
            right: 1025,
            bottom: 1024,
        };
        assert!(
            analyze_with(bounds, &pixels, None, None, |_, _| panic!(
                "oversized OCR invoked provider"
            ))
            .is_err()
        );
        assert!(
            analyze_with(
                bounds,
                &pixels,
                Some(Region {
                    x: 1025,
                    y: 0,
                    width: 1,
                    height: 1
                }),
                None,
                |_, _| panic!("outside OCR invoked provider")
            )
            .is_err()
        );
        let canceled = AtomicBool::new(true);
        assert!(
            analyze_with(
                bounds,
                &pixels,
                Some(region()),
                Some(&canceled),
                |_, _| panic!("canceled OCR invoked provider")
            )
            .is_err()
        );
        assert!(
            validate_region(Some(Region {
                x: u32::MAX,
                y: 0,
                width: 1,
                height: 1
            }))
            .is_err()
        );
        assert!(
            validate_region(Some(Region {
                x: 0,
                y: 0,
                width: 0,
                height: 1
            }))
            .is_err()
        );
        assert!(
            validate_region(Some(Region {
                x: 0,
                y: 0,
                width: 4097,
                height: 1
            }))
            .is_err()
        );
        assert!(
            validate_region(Some(Region {
                x: 0,
                y: 0,
                width: 1,
                height: 4097
            }))
            .is_err()
        );
        assert!(validate_line_aspect(Rect::from_tlhw(0, 0, 1, 4096), 64).is_err());
        assert!(validate_line_aspect(Rect::from_tlhw(0, 0, 2, 75), 64).is_ok());
        assert!(validate_line_aspect(Rect::from_tlhw(0, 0, 0, 2), 64).is_err());
        let cancel = AtomicBool::new(false);
        assert!(
            analyze_with(
                bounds,
                &pixels,
                Some(region()),
                Some(&cancel),
                |_, cancel| {
                    cancel.unwrap().store(true, Ordering::Release);
                    Ok(vec![glyphs("private late text")])
                }
            )
            .is_err()
        );
    }
    #[test]
    fn model_checks_consume_exact_hash_pinned_buffer_and_never_search_source_paths() {
        assert_eq!(
            sha256(b"abc").unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let root = std::env::temp_dir().join(format!(
            "proto-ocr-model-{}-{}",
            std::process::id(),
            crate::gui_bridge::random_token().unwrap()
        ));
        std::fs::create_dir(&root).unwrap();
        let specification = ModelSpec {
            name: "test.rten",
            bytes: 3,
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        };
        assert!(verified_model(&root, &specification).is_err());
        std::fs::write(root.join(specification.name), b"abc").unwrap();
        assert_eq!(verified_model(&root, &specification).unwrap(), b"abc");
        for bytes in [b"abd".as_slice(), b"abcd".as_slice()] {
            std::fs::write(root.join(specification.name), bytes).unwrap();
            assert!(verified_model(&root, &specification).is_err());
        }
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            directory_for_executable(Path::new("C:/app/program.exe")).unwrap(),
            Path::new("C:/app/assets/ocr")
        );
        assert_eq!(
            directory_for_executable(Path::new("C:/app/deps/test.exe")).unwrap(),
            Path::new("C:/app/assets/ocr")
        );
    }
}
