//! Read-only rasterization of shared Windows cursor resources for per-user overlays.
//! LoadCursor returns shared handles; these are never destroyed or installed.
use crate::app_cursor::AppCursorImage;
use crate::cursor_presenter::DibCanvas;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};
use windows_sys::Win32::{Graphics::Gdi::*, UI::WindowsAndMessaging::*};

const MAX_EDGE: i32 = 256;
const MAX_CACHE: usize = 32;
const INVERSION_NOTE: &str =
    "Windows cursor inverting pixels rendered solid to preserve the Windows glyph geometry.";
const VECTOR_NOTE: &str = "Windows cursor pixels could not be represented as visible RGBA; showing the compatible vector fallback.";

#[derive(Clone)]
pub(crate) struct RasterResult {
    pub image: Option<AppCursorImage>,
    pub note: Option<&'static str>,
}

type CursorCache = HashMap<(u32, i32), RasterResult>;
static CACHE: OnceLock<Mutex<CursorCache>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FollowMode {
    ApplicationVisible,
    ApplicationHidden,
    ConfiguredFallback,
}

pub(crate) fn follow_mode(follow: bool, visible: Option<bool>) -> FollowMode {
    if !follow {
        FollowMode::ConfiguredFallback
    } else {
        match visible {
            Some(true) => FollowMode::ApplicationVisible,
            Some(false) => FollowMode::ApplicationHidden,
            None => FollowMode::ConfiguredFallback,
        }
    }
}

/// Observer-supported stock IDs; unknown app kinds use the configured IDC_ARROW fallback.
pub(crate) fn stock_kind(kind: u32) -> Option<u32> {
    matches!(kind, 32512..=32516 | 32642..=32646 | 32648..=32651).then_some(kind)
}

pub(crate) fn raster(kind: u32, edge: i32) -> RasterResult {
    let stock = stock_kind(kind).unwrap_or(32512);
    let edge = edge.clamp(12, MAX_EDGE);
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    match cache.lock() {
        Ok(cache) => {
            if let Some(result) = cache.get(&(stock, edge)).cloned() {
                return result;
            }
        }
        Err(_) => {
            return RasterResult {
                image: None,
                note: Some(VECTOR_NOTE),
            };
        }
    }
    let result = raster_uncached(stock, edge).unwrap_or(RasterResult {
        image: None,
        note: Some(VECTOR_NOTE),
    });
    if let Ok(mut cache) = cache.lock() {
        if cache.len() >= MAX_CACHE {
            cache.clear();
        }
        cache.insert((stock, edge), result.clone());
    }
    result
}

struct OwnedBitmap(HBITMAP);
impl Drop for OwnedBitmap {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                DeleteObject(self.0 as HGDIOBJ);
            }
        }
    }
}

fn bitmap_dimensions(bitmap: HBITMAP) -> Option<(i32, i32)> {
    if bitmap.is_null() {
        return None;
    }
    unsafe {
        let mut info: BITMAP = std::mem::zeroed();
        let read = GetObjectW(
            bitmap as HGDIOBJ,
            std::mem::size_of::<BITMAP>() as i32,
            (&mut info as *mut BITMAP).cast(),
        );
        if read == 0 {
            return None;
        }
        Some((info.bmWidth.checked_abs()?, info.bmHeight.checked_abs()?))
    }
}

fn raster_uncached(kind: u32, edge: i32) -> Option<RasterResult> {
    unsafe {
        let resource = kind as usize as *const u16;
        let cursor = LoadCursorW(std::ptr::null_mut(), resource);
        if cursor.is_null() {
            return None;
        }
        let mut icon: ICONINFO = std::mem::zeroed();
        if GetIconInfo(cursor, &mut icon) == 0 {
            return None;
        }
        // GetIconInfo allocates these bitmaps even though the cursor handle is shared.
        let mask = OwnedBitmap(icon.hbmMask);
        let color = OwnedBitmap(icon.hbmColor);
        let native = if color.0.is_null() {
            let (width, doubled_height) = bitmap_dimensions(mask.0)?;
            if doubled_height < 2 || doubled_height % 2 != 0 {
                return None;
            }
            (width, doubled_height / 2)
        } else {
            bitmap_dimensions(color.0)?
        };
        let (native_w, native_h) = native;
        if !(1..=MAX_EDGE).contains(&native_w) || !(1..=MAX_EDGE).contains(&native_h) {
            return None;
        }
        drop(color);
        drop(mask);

        // Draw at the resource's original dimensions: GDI scaling can erase thin
        // monochrome strokes before we have reconstructed their coverage.
        let (width, height) = native;
        let black = DibCanvas::new(width, height).ok()?;
        let white = DibCanvas::new(width, height).ok()?;
        let count = usize::try_from(width.checked_mul(height)?).ok()?;
        std::ptr::write_bytes(black.bits, 0, count * 4);
        let white_bytes = std::slice::from_raw_parts_mut(white.bits.cast::<u8>(), count * 4);
        for pixel in white_bytes.as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&[255, 255, 255, 255]);
        }
        for canvas in [&black, &white] {
            if DrawIconEx(
                canvas.dc,
                0,
                0,
                cursor,
                width,
                height,
                0,
                std::ptr::null_mut(),
                DI_NORMAL,
            ) == 0
            {
                return None;
            }
        }
        GdiFlush();
        let black_pixels = std::slice::from_raw_parts(black.bits.cast::<u8>(), count * 4);
        let white_pixels = std::slice::from_raw_parts(white.bits.cast::<u8>(), count * 4);
        let (bgra, note) = reconstruct_raster(black_pixels, white_pixels)?;
        let image = scale_image(
            kind,
            edge,
            (native_w as u32, native_h as u32),
            (icon.xHotspot, icon.yHotspot),
            &bgra,
        )?;
        Some(RasterResult {
            image: Some(image),
            note,
        })
    }
}

/// Only stock resources use this conversion; application-provided RGBA is untouched.
fn reconstruct_raster(black: &[u8], white: &[u8]) -> Option<(Vec<u8>, Option<&'static str>)> {
    if black.is_empty()
        || black.len() > (MAX_EDGE * MAX_EDGE * 4) as usize
        || black.len() != white.len()
        || !black.len().is_multiple_of(4)
    {
        return None;
    }
    let mut bgra = Vec::with_capacity(black.len());
    let mut normalized_inversion = false;
    for (black, white) in black
        .as_chunks::<4>()
        .0
        .iter()
        .zip(white.as_chunks::<4>().0)
    {
        let black = [black[0], black[1], black[2]];
        let white = [white[0], white[1], white[2]];
        let pixel = if black == [255; 3] && white == [0; 3] {
            // Exact destination inversion has no source-over equivalent. Preserve
            // the glyph's footprint as solid white, with an explicit cached note.
            normalized_inversion = true;
            [255; 4]
        } else {
            reconstruct_bgra(black, white)?
        };
        bgra.extend_from_slice(&pixel);
    }
    bgra.as_chunks::<4>()
        .0
        .iter()
        .any(|pixel| pixel[3] != 0)
        .then_some((bgra, normalized_inversion.then_some(INVERSION_NOTE)))
}

fn scale_image(
    kind: u32,
    edge: i32,
    native: (u32, u32),
    hotspot: (u32, u32),
    bgra: &[u8],
) -> Option<AppCursorImage> {
    let (source_w, source_h) = native;
    if !(1..=MAX_EDGE as u32).contains(&source_w)
        || !(1..=MAX_EDGE as u32).contains(&source_h)
        || bgra.len() != (source_w * source_h * 4) as usize
    {
        return None;
    }
    let edge = edge.clamp(12, MAX_EDGE);
    let scale = f64::from(edge) / f64::from(source_w.max(source_h));
    let width = (f64::from(source_w) * scale).round().max(1.0) as u32;
    let height = (f64::from(source_h) * scale).round().max(1.0) as u32;
    let pixels = area_scale(bgra, native, (width, height));
    if !pixels.as_chunks::<4>().0.iter().any(|pixel| pixel[3] != 0) {
        return None;
    }
    Some(AppCursorImage {
        width,
        height,
        hotspot: (
            (f64::from(hotspot.0) * f64::from(width) / f64::from(source_w))
                .round()
                .clamp(0.0, f64::from(width - 1)) as i32,
            (f64::from(hotspot.1) * f64::from(height) / f64::from(source_h))
                .round()
                .clamp(0.0, f64::from(height - 1)) as i32,
        ),
        identity: image_hash(&pixels, kind, edge),
        pixels: Arc::from(pixels),
    })
}

/// Exact box coverage in integer coordinates, averaging premultiplied channels.
/// With both edges <= 256, the number of source contributions is bounded by
/// (source_w + target_w) * (source_h + target_h), independent of mouse motion.
fn area_scale(bgra: &[u8], source: (u32, u32), target: (u32, u32)) -> Vec<u8> {
    let (sw, sh) = source;
    let (tw, th) = target;
    let mut pixels = vec![0; (tw * th * 4) as usize];
    let area = u64::from(sw) * u64::from(sh);
    for y in 0..th {
        let top = y * sh;
        let bottom = (y + 1) * sh;
        for x in 0..tw {
            let left = x * sw;
            let right = (x + 1) * sw;
            let mut total = [0u64; 4];
            for sy in top / th..bottom.div_ceil(th) {
                let overlap_y = bottom.min((sy + 1) * th) - top.max(sy * th);
                for sx in left / tw..right.div_ceil(tw) {
                    let overlap_x = right.min((sx + 1) * tw) - left.max(sx * tw);
                    let weight = u64::from(overlap_x) * u64::from(overlap_y);
                    let offset = ((sy * sw + sx) * 4) as usize;
                    for channel in 0..4 {
                        total[channel] += u64::from(bgra[offset + channel]) * weight;
                    }
                }
            }
            let offset = ((y * tw + x) * 4) as usize;
            for channel in 0..4 {
                pixels[offset + channel] = ((total[channel] + area / 2) / area) as u8;
            }
        }
    }
    pixels
}

fn image_hash(bytes: &[u8], kind: u32, edge: i32) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in kind
        .to_le_bytes()
        .into_iter()
        .chain(edge.to_le_bytes())
        .chain(bytes.iter().copied())
    {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    hash
}

fn reconstruct_bgra(black: [u8; 3], white: [u8; 3]) -> Option<[u8; 4]> {
    let difference = [
        i16::from(white[0]) - i16::from(black[0]),
        i16::from(white[1]) - i16::from(black[1]),
        i16::from(white[2]) - i16::from(black[2]),
    ];
    if difference.iter().any(|value| !(0..=255).contains(value))
        || (difference[0] - difference[1]).abs() > 1
        || (difference[1] - difference[2]).abs() > 1
    {
        return None;
    }
    let alpha = (255 - difference[0]).clamp(0, 255) as u8;
    Some([
        black[0].min(alpha),
        black[1].min(alpha),
        black[2].min(alpha),
        alpha,
    ])
}

pub(crate) fn glow_alpha(width: usize, height: usize, alpha: &[u8], x: i32, y: i32) -> u8 {
    if width == 0 || height == 0 || alpha.len() != width.saturating_mul(height) {
        return 0;
    }
    const RADIUS: i32 = 3;
    let mut glow = 0;
    for source_y in (y - RADIUS).max(0)..=(y + RADIUS).min(height as i32 - 1) {
        for source_x in (x - RADIUS).max(0)..=(x + RADIUS).min(width as i32 - 1) {
            let distance = (x - source_x).abs().max((y - source_y).abs());
            let source_alpha = alpha[source_y as usize * width + source_x as usize];
            let strength = (u16::from(source_alpha) * (100 - distance as u16 * 25) / 100) as u8;
            glow = glow.max(strength);
        }
    }
    glow
}

/// Convert UI RGB into premultiplied BGRA for the existing DIB compositor.
pub(crate) fn glow_bgra(color: [u8; 3], alpha: u8) -> [u8; 4] {
    let a = u16::from(alpha);
    [
        (u16::from(color[2]) * a / 255) as u8,
        (u16::from(color[1]) * a / 255) as u8,
        (u16::from(color[0]) * a / 255) as u8,
        alpha,
    ]
}

pub(crate) fn source_over(destination: [u8; 4], source: [u8; 4]) -> [u8; 4] {
    let inverse = 255 - u16::from(source[3]);
    std::array::from_fn(|channel| {
        (u16::from(source[channel]) + (u16::from(destination[channel]) * inverse + 127) / 255)
            .min(255) as u8
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_over_reconstruction_preserves_alpha_and_rejects_unrepresentable_pixels() {
        assert_eq!(
            reconstruct_bgra([0, 0, 0], [255, 255, 255]),
            Some([0, 0, 0, 0])
        );
        assert_eq!(reconstruct_bgra([0, 0, 0], [0, 0, 0]), Some([0, 0, 0, 255]));
        assert_eq!(
            reconstruct_bgra([255, 255, 255], [255, 255, 255]),
            Some([255, 255, 255, 255])
        );
        assert_eq!(
            reconstruct_bgra([0, 0, 0], [127, 127, 127]),
            Some([0, 0, 0, 128])
        );
        assert_eq!(reconstruct_bgra([255, 255, 255], [0, 0, 0]), None);
        assert_eq!(reconstruct_bgra([20, 20, 20], [30, 40, 50]), None);
        assert_eq!(reconstruct_bgra([20, 20, 20], [10, 10, 10]), None);
        assert_eq!(
            reconstruct_bgra([0, 0, 0], [127, 128, 127]),
            Some([0, 0, 0, 128])
        );
        assert_eq!(
            reconstruct_bgra([128, 129, 128], [255, 255, 255]),
            Some([128, 128, 128, 128])
        );
        assert_eq!(
            reconstruct_bgra([10, 40, 80], [137, 167, 207]),
            Some([10, 40, 80, 128])
        );
    }

    #[test]
    fn empty_stock_raster_is_rejected_but_opaque_black_is_visible() {
        assert!(reconstruct_raster(&[], &[]).is_none());
        assert!(reconstruct_raster(&[0, 0, 0, 0], &[255, 255, 255, 255]).is_none());
        let (opaque, note) = reconstruct_raster(&[0; 4], &[0; 4]).unwrap();
        assert_eq!(opaque, [0, 0, 0, 255]);
        assert_eq!(note, None);
        assert!(reconstruct_raster(&[0; 3], &[0; 3]).is_none());
        assert!(reconstruct_raster(&[0; 4], &[0; 8]).is_none());
    }

    #[test]
    fn exact_inversion_preserves_glyph_footprint_and_records_conversion_note() {
        let black = [0, 0, 0, 0, 255, 255, 255, 0, 0, 0, 0, 0];
        let white = [255, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0];
        let (pixels, note) = reconstruct_raster(&black, &white).unwrap();
        assert_eq!(pixels, [0, 0, 0, 0, 255, 255, 255, 255, 0, 0, 0, 255]);
        assert!(note.unwrap().contains("inverting pixels rendered solid"));
        // A different destination-dependent operation must reject the whole raster.
        assert!(reconstruct_raster(&[255, 0, 255, 0], &[0, 255, 0, 0]).is_none());
    }

    #[test]
    fn one_pixel_ibeam_strokes_survive_minification_to_24_and_12() {
        let mut pixels = vec![0; 48 * 48 * 4];
        for y in 4..44 {
            for x in 0..48 {
                if x == 23 || ((y == 4 || y == 43) && (19..=27).contains(&x)) {
                    let offset = (y * 48 + x) * 4;
                    pixels[offset..offset + 4].copy_from_slice(&[255; 4]);
                }
            }
        }
        for edge in [24, 12] {
            let image = scale_image(32513, edge, (48, 48), (23, 23), &pixels).unwrap();
            assert_eq!((image.width, image.height), (edge as u32, edge as u32));
            let rows_with_coverage = image
                .pixels
                .chunks_exact(edge as usize * 4)
                .filter(|row| row.as_chunks::<4>().0.iter().any(|pixel| pixel[3] > 0))
                .count();
            assert!(rows_with_coverage >= (edge * 3 / 4) as usize);
            assert!(
                image
                    .pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|pixel| pixel[0] == pixel[3])
            );
        }
    }

    #[test]
    fn area_scaling_preserves_premultiplied_color_and_scales_hotspot() {
        let mut pixels = vec![0; 24 * 24 * 4];
        pixels[0..4].copy_from_slice(&[0, 0, 128, 128]);
        pixels[4..8].copy_from_slice(&[128, 0, 0, 128]);
        let image = scale_image(32512, 12, (24, 24), (8, 16), &pixels).unwrap();
        assert_eq!(&image.pixels[0..4], &[32, 0, 32, 64]);
        assert_eq!(image.hotspot, (4, 8));
        assert!(
            image
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[..3].iter().all(|channel| *channel <= pixel[3]))
        );
    }

    #[test]
    fn raster_scaling_bounds_dimensions_allocation_and_hotspot() {
        let pixels = [40, 80, 120, 255].repeat(16 * 8);
        let small = scale_image(32512, i32::MIN, (16, 8), (u32::MAX, u32::MAX), &pixels).unwrap();
        assert_eq!((small.width, small.height, small.hotspot), (12, 6, (11, 5)));
        let large = scale_image(32512, i32::MAX, (16, 8), (0, 0), &pixels).unwrap();
        assert_eq!((large.width, large.height), (256, 128));
        assert_eq!(large.pixels.len(), 256 * 128 * 4);
        assert_eq!(&large.pixels[0..4], &[40, 80, 120, 255]);
        assert!(scale_image(32512, 12, (0, 8), (0, 0), &[]).is_none());
        assert!(scale_image(32512, 12, (257, 8), (0, 0), &[]).is_none());
        assert!(scale_image(32512, 12, (16, 8), (0, 0), &[0; 4]).is_none());
        assert!(scale_image(32512, 12, (16, 8), (0, 0), &[0; 16 * 8 * 4]).is_none());
    }

    #[test]
    fn glow_follows_alpha_into_transparent_image_padding() {
        let mut alpha = vec![0; 7 * 7];
        alpha[3 * 7 + 3] = 255;
        assert!(glow_alpha(7, 7, &alpha, 4, 3) > 0);
        assert!(glow_alpha(7, 7, &alpha, 0, 0) > 0);
        assert_eq!(glow_alpha(7, 7, &alpha, -4, 0), 0);
    }

    #[test]
    fn pure_red_and_blue_remain_correct_in_bgra_glow() {
        assert_eq!(glow_bgra([255, 0, 0], 128), [0, 0, 128, 128]);
        assert_eq!(glow_bgra([0, 0, 255], 128), [128, 0, 0, 128]);
    }

    #[test]
    fn opaque_original_pixels_replace_the_glow_exactly() {
        let original = [15, 64, 210, 255];
        assert_eq!(source_over([200, 20, 4, 128], original), original);
    }

    #[test]
    fn hidden_followed_cursor_is_not_the_configured_arrow_fallback() {
        assert_eq!(
            follow_mode(true, Some(false)),
            FollowMode::ApplicationHidden
        );
        assert_eq!(
            follow_mode(true, Some(true)),
            FollowMode::ApplicationVisible
        );
        assert_eq!(follow_mode(true, None), FollowMode::ConfiguredFallback);
        assert_eq!(
            follow_mode(false, Some(false)),
            FollowMode::ConfiguredFallback
        );
    }
}
