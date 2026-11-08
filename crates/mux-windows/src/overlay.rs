//! Thread-owned, click-through desktop cursor windows. Never moves the system pointer.
use crate::app_cursor::{AppCursorImage, AppCursorObservation};
use crate::cursor_presenter::{DibCanvas, Presenter};
pub use crate::cursor_presenter::{PresentationStatus, RasterBackend};
use crate::system_cursor;
use anyhow::{Context, Result, ensure};
use mux_core::desktop::{BadgePosition, BadgeShape, CursorSettings, CursorShape, LabelPosition};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::HashMap,
    marker::PhantomData,
    rc::Rc,
    sync::{Arc, Mutex, OnceLock},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        HiDpi::*,
        Input::KeyboardAndMouse::{
            MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey, UnregisterHotKey, VK_F11,
        },
        WindowsAndMessaging::*,
    },
};

const MAX_CANVAS_WIDTH: i32 = 1024;
const MAX_CANVAS_HEIGHT: i32 = 1024;
const MAX_LABEL_WIDTH: i32 = 512;
const MAX_LABEL_CHARS: usize = 64;
const THEME_PREVIEW_EDGE: i32 = 64;
const THEME_PREVIEW_PADDING: i32 = 4;
const MAX_THEME_PREVIEWS: usize = 32;

/// Cached Windows stock arrow artwork composited with the configured glow.
/// This is a read-only preview resource and does not inspect or move the pointer.
pub fn theme_cursor_preview(color: [u8; 3]) -> Option<AppCursorImage> {
    static CACHE: OnceLock<Mutex<HashMap<[u8; 3], Option<AppCursorImage>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(cache) = cache.lock()
        && let Some(image) = cache.get(&color)
    {
        return image.clone();
    }
    let image = compose_theme_cursor_preview(color);
    if let Ok(mut cache) = cache.lock() {
        if cache.len() >= MAX_THEME_PREVIEWS {
            cache.clear();
        }
        cache.insert(color, image.clone());
    }
    image
}

fn compose_theme_cursor_preview(color: [u8; 3]) -> Option<AppCursorImage> {
    let source = system_cursor::raster(32512, THEME_PREVIEW_EDGE).image?;
    let padding = THEME_PREVIEW_PADDING as u32;
    let width = source.width.checked_add(padding * 2)?;
    let height = source.height.checked_add(padding * 2)?;
    let mut pixels = vec![0; width as usize * height as usize * 4];
    let alpha: Vec<u8> = source
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| pixel[3])
        .collect();

    // Match the native system-cursor glow compositor, including its transparent
    // padding so the halo follows the arrow silhouette.
    for y in -3..source.height as i32 + 3 {
        for x in -3..source.width as i32 + 3 {
            let glow = system_cursor::glow_alpha(
                source.width as usize,
                source.height as usize,
                &alpha,
                x,
                y,
            );
            if glow == 0 {
                continue;
            }
            let target_x = x + THEME_PREVIEW_PADDING;
            let target_y = y + THEME_PREVIEW_PADDING;
            let offset = ((target_y as u32 * width + target_x as u32) * 4) as usize;
            let destination: [u8; 4] = pixels[offset..offset + 4].try_into().ok()?;
            pixels[offset..offset + 4].copy_from_slice(&system_cursor::source_over(
                destination,
                system_cursor::glow_bgra(color, glow),
            ));
        }
    }
    for y in 0..source.height {
        for x in 0..source.width {
            let source_offset = ((y * source.width + x) * 4) as usize;
            let target_x = x + padding;
            let target_y = y + padding;
            let target_offset = ((target_y * width + target_x) * 4) as usize;
            let destination: [u8; 4] = pixels[target_offset..target_offset + 4].try_into().ok()?;
            let original: [u8; 4] = source.pixels[source_offset..source_offset + 4]
                .try_into()
                .ok()?;
            pixels[target_offset..target_offset + 4]
                .copy_from_slice(&system_cursor::source_over(destination, original));
        }
    }

    Some(AppCursorImage {
        width,
        height,
        hotspot: (
            source.hotspot.0 + THEME_PREVIEW_PADDING,
            source.hotspot.1 + THEME_PREVIEW_PADDING,
        ),
        identity: source.identity
            ^ u64::from(color[0]) << 16
            ^ u64::from(color[1]) << 8
            ^ u64::from(color[2]),
        pixels: Arc::from(pixels),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RenderKey {
    application_kind: u32,
    application_hidden: bool,
    application_image: u64,
    name: String,
    name_truncated: bool,
    color: [u8; 3],
    size_bits: u32,
    shape: CursorShape,
    label: bool,
    label_position: LabelPosition,
    font_size_bits: u32,
    badge: Option<String>,
    badge_shape: BadgeShape,
    badge_position: BadgePosition,
    paused: bool,
    renderer: mux_core::desktop::CursorRenderer,
}

#[derive(Clone)]
struct RenderFrame {
    key: RenderKey,
    recipe: RenderRecipe,
    canvas_size: (i32, i32),
    hotspot: (i32, i32),
    painted_pixels: usize,
    pixel_hash: u64,
    rendered_label: String,
    label_truncated: bool,
    generation: u64,
}

struct PrintSurface {
    canvas: Option<DibCanvas>,
    recipe: Option<RenderRecipe>,
    size: (i32, i32),
}

struct MeasureDc(HDC);

impl MeasureDc {
    fn new() -> Result<Self> {
        unsafe {
            let dc = CreateCompatibleDC(std::ptr::null_mut());
            ensure!(!dc.is_null(), "create cursor measurement context");
            Ok(Self(dc))
        }
    }
}

impl Drop for MeasureDc {
    fn drop(&mut self) {
        unsafe {
            DeleteDC(self.0);
        }
    }
}

struct OwnedGdiObject(HGDIOBJ);

impl Drop for OwnedGdiObject {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                DeleteObject(self.0);
            }
        }
    }
}

struct SelectedObject {
    dc: HDC,
    previous: HGDIOBJ,
}

impl SelectedObject {
    fn new(dc: HDC, object: HGDIOBJ, context: &str) -> Result<Self> {
        let previous = unsafe { SelectObject(dc, object) };
        ensure!(
            !previous.is_null() && previous != (-1isize) as HGDIOBJ,
            "{context}"
        );
        Ok(Self { dc, previous })
    }
}

impl Drop for SelectedObject {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Rect {
    pub(crate) left: i32,
    pub(crate) top: i32,
    pub(crate) right: i32,
    pub(crate) bottom: i32,
}

impl Rect {
    pub(crate) fn gpu(self) -> windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F {
        windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F {
            left: self.left as f32,
            top: self.top as f32,
            right: self.right as f32,
            bottom: self.bottom as f32,
        }
    }
    fn width(self) -> i32 {
        self.right - self.left
    }
    fn height(self) -> i32 {
        self.bottom - self.top
    }
    fn translated(self, x: i32, y: i32) -> Self {
        Self {
            left: self.left + x,
            top: self.top + y,
            right: self.right + x,
            bottom: self.bottom + y,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Layout {
    pub(crate) size: (i32, i32),
    pub(crate) hotspot: (i32, i32),
    pub(crate) name: Option<Rect>,
    pub(crate) badge: Option<Rect>,
    pub(crate) text: Option<Rect>,
    pub(crate) rendered_label: String,
    pub(crate) label_truncated: bool,
}

#[derive(Clone)]
pub(crate) struct RenderRecipe {
    pub(crate) settings: CursorSettings,
    pub(crate) layout: Layout,
    pub(crate) paused: bool,
    pub(crate) application: Option<AppCursorObservation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OverlayStatus {
    #[serde(default)]
    pub presentation: PresentationStatus,
    #[serde(default)]
    pub application_cursor: Option<AppCursorObservation>,
    pub hwnd: usize,
    pub x: i32,
    pub y: i32,
    pub visible: bool,
    pub no_activate: bool,
    pub click_through: bool,
    pub painted_pixels: usize,
    #[serde(default)]
    pub shape: CursorShape,
    #[serde(default)]
    pub system_cursor_fallback: Option<String>,
    #[serde(default)]
    pub badge: Option<String>,
    #[serde(default)]
    pub render_origin: (i32, i32),
    #[serde(default)]
    pub hotspot: (i32, i32),
    #[serde(default)]
    pub pixel_hash: u64,
    #[serde(default)]
    pub pixel_telemetry_source: Option<String>,
    #[serde(default)]
    pub canvas_size: (i32, i32),
    #[serde(default)]
    pub idle_hidden: bool,
    #[serde(default)]
    pub rendered_label: String,
    #[serde(default)]
    pub label_truncated: bool,
    #[serde(default)]
    pub paint_generation: u64,
}
pub struct Overlay {
    hwnd: HWND,
    presenter: Presenter,
    print_surface: Box<RefCell<PrintSurface>>,
    _thread: PhantomData<Rc<()>>,
    pub status: OverlayStatus,
    frame: Option<RenderFrame>,
    paint_generation: u64,
    #[cfg(feature = "cursor-presentation-evidence")]
    evidence: Option<crate::cursor_evidence::Recorder>,
}
unsafe extern "system" fn window_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match m {
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_PRINT | WM_PRINTCLIENT => unsafe {
            let pointer = GetWindowLongPtrW(h, GWLP_USERDATA);
            if pointer == 0 || w == 0 {
                return 0;
            }
            let surface = &*(pointer as *const RefCell<PrintSurface>);
            let Ok(mut surface) = surface.try_borrow_mut() else {
                return 0;
            };
            // Compatibility printing only: never call the presenter or route
            // input from this reentrant window procedure.
            if surface.canvas.is_none() {
                let Some(recipe) = surface.recipe.as_ref() else {
                    return 0;
                };
                let Ok(canvas) = rasterize_gdi(recipe) else {
                    return 0;
                };
                surface.canvas = Some(canvas);
            }
            let Some(canvas) = surface.canvas.as_ref() else {
                return 0;
            };
            if BitBlt(
                w as HDC,
                0,
                0,
                surface.size.0,
                surface.size.1,
                canvas.dc,
                0,
                0,
                SRCCOPY,
            ) != 0
            {
                1
            } else {
                0
            }
        },
        WM_NCDESTROY => unsafe {
            SetWindowLongPtrW(h, GWLP_USERDATA, 0);
            DefWindowProcW(h, m, w, l)
        },
        _ => unsafe { DefWindowProcW(h, m, w, l) },
    }
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn font(size: f32) -> Result<OwnedGdiObject> {
    unsafe {
        let face = wide("Segoe UI");
        let handle = CreateFontW(
            -(size.round() as i32),
            0,
            0,
            0,
            FW_NORMAL as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            ANTIALIASED_QUALITY as u32,
            (DEFAULT_PITCH | FF_DONTCARE) as u32,
            face.as_ptr(),
        );
        ensure!(!handle.is_null(), "create cursor label font");
        Ok(OwnedGdiObject(handle))
    }
}

fn measure(dc: HDC, text: &str) -> Result<(i32, i32)> {
    if text.is_empty() {
        return Ok((0, 0));
    }
    unsafe {
        let text = wide(text);
        let mut size = SIZE { cx: 0, cy: 0 };
        ensure!(
            GetTextExtentPoint32W(dc, text.as_ptr(), text.len() as i32 - 1, &mut size) != 0,
            "measure cursor label"
        );
        Ok((size.cx, size.cy))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BoundedName {
    text: String,
    truncated: bool,
}

fn bounded_name(name: &str) -> BoundedName {
    let mut text = String::new();
    let mut truncated = false;
    for (index, character) in name.chars().take(MAX_LABEL_CHARS + 1).enumerate() {
        if index == MAX_LABEL_CHARS {
            truncated = true;
            break;
        }
        if character.is_control() {
            truncated = true;
        } else {
            text.push(character);
        }
    }
    if truncated {
        text = text.chars().take(MAX_LABEL_CHARS - 1).collect();
    }
    BoundedName { text, truncated }
}

fn fit_label(
    dc: HDC,
    name: &str,
    source_truncated: bool,
    max_width: i32,
) -> Result<(String, bool, (i32, i32))> {
    let max_width = max_width.clamp(1, MAX_LABEL_WIDTH);
    let mut characters: Vec<char> = name.chars().collect();
    let mut truncated = source_truncated;
    let mut rendered: String = characters.iter().collect();
    let mut extent = measure(dc, &rendered)?;
    if extent.0 <= max_width && !truncated {
        return Ok((rendered, false, extent));
    }
    truncated = true;
    // The ellipsis is part of the rendered label's scalar limit.
    characters.truncate(MAX_LABEL_CHARS - 1);
    while !characters.is_empty() {
        let mut candidate: String = characters.iter().collect();
        candidate.push('…');
        extent = measure(dc, &candidate)?;
        if extent.0 <= max_width {
            return Ok((candidate, truncated, extent));
        }
        characters.pop();
    }
    rendered = "…".into();
    extent = measure(dc, &rendered)?;
    if extent.0 > max_width {
        return Ok((String::new(), true, (0, 0)));
    }
    Ok((rendered, truncated, extent))
}

fn badge_extent(shape: BadgeShape, text: (i32, i32)) -> (i32, i32) {
    match shape {
        BadgeShape::Circle => {
            let diameter = text.0.max(text.1).saturating_add(12).clamp(24, 512);
            (diameter, diameter)
        }
        BadgeShape::Rounded | BadgeShape::Box => (
            text.0.saturating_add(12).clamp(20, 512),
            text.1.saturating_add(8).clamp(20, 512),
        ),
    }
}

fn include_rect(bounds: &mut Rect, rect: Rect) {
    bounds.left = bounds.left.min(rect.left);
    bounds.top = bounds.top.min(rect.top);
    bounds.right = bounds.right.max(rect.right);
    bounds.bottom = bounds.bottom.max(rect.bottom);
}

pub(crate) fn image_geometry(image: &AppCursorImage, size: i32) -> (i32, i32, i32, i32) {
    let scale = f64::from(size) / f64::from(image.width.max(image.height).max(1));
    let width = (f64::from(image.width) * scale).round().max(1.) as i32;
    let height = (f64::from(image.height) * scale).round().max(1.) as i32;
    let hx = (f64::from(image.hotspot.0) * scale).round() as i32;
    let hy = (f64::from(image.hotspot.1) * scale).round() as i32;
    (width, height, hx.min(width - 1), hy.min(height - 1))
}

fn layout_application(
    settings: &CursorSettings,
    name: &BoundedName,
    dc: HDC,
    app: Option<&AppCursorObservation>,
) -> Result<Layout> {
    let size = settings.size.round() as i32;
    let glyph = if let Some(image) = app.and_then(|a| a.image.as_ref()) {
        let (w, h, x, y) = image_geometry(image, size);
        let halo = if settings.shape == CursorShape::System {
            6
        } else {
            0
        };
        Rect {
            left: -x - halo,
            top: -y - halo,
            right: w - x + halo,
            bottom: h - y + halo,
        }
    } else if app.is_some_and(|a| a.visible == Some(true) && a.kind != 0) {
        let kind = app.unwrap().kind;
        let (x, y) = stock_hotspot(kind, size);
        Rect {
            left: -x - 2,
            top: -y - 2,
            right: size - x + 3,
            bottom: size - y + 3,
        }
    } else if matches!(settings.shape, CursorShape::Arrow | CursorShape::System) {
        Rect {
            left: 0,
            top: 0,
            // The legacy arrow's label starts one cursor-size to the right.
            right: size,
            bottom: ((f64::from(size) * 1.14).ceil() as i32).max(1),
        }
    } else {
        Rect {
            left: -(size / 2),
            top: -(size / 2),
            right: size - size / 2,
            bottom: size - size / 2,
        }
    };
    let badge_text = settings.badge.as_deref().unwrap_or_default();
    let badge_text_extent = measure(dc, badge_text)?;
    let badge_size = badge_extent(settings.badge_shape, badge_text_extent);
    let inline_badge = settings.badge.is_some() && settings.badge_position == BadgePosition::Inline;
    // Reserve room for the glyph and badge before fitting the name. This also
    // covers opposite-side labels and badges, so any valid setting fits the
    // native canvas bound instead of failing during a worker tick.
    let badge_width = if settings.badge.is_some() {
        badge_size.0.saturating_add(4)
    } else {
        0
    };
    let label_budget = MAX_CANVAS_WIDTH
        .saturating_sub(glyph.width())
        .saturating_sub(badge_width)
        .saturating_sub(8)
        .saturating_sub(4)
        .clamp(1, MAX_LABEL_WIDTH);
    let (rendered_label, label_truncated, name_extent) = if settings.label {
        fit_label(dc, &name.text, name.truncated, label_budget)?
    } else {
        (String::new(), false, (0, 0))
    };
    let inline_badge_only = inline_badge && !settings.label;
    let has_label = settings.label && !rendered_label.is_empty();
    let label_block_width = name_extent.0
        + if inline_badge && (has_label || inline_badge_only) {
            (if has_label { 4 } else { 0 }) + badge_size.0
        } else {
            0
        };
    let label_block_height =
        name_extent
            .1
            .max(if inline_badge && (has_label || inline_badge_only) {
                badge_size.1
            } else {
                0
            });
    let mut name_rect = None;
    let mut badge_rect = None;
    let mut badge_text_rect = None;
    if label_block_width > 0 && label_block_height > 0 {
        let gap = 8;
        let left = match settings.label_position {
            LabelPosition::Right => glyph.right + gap,
            LabelPosition::Left => glyph.left - gap - label_block_width,
            LabelPosition::Above | LabelPosition::Below => {
                (glyph.left + glyph.right - label_block_width) / 2
            }
        };
        let side_label_top = if app.is_some() {
            (glyph.top + glyph.bottom - label_block_height) / 2
        } else if matches!(settings.shape, CursorShape::Arrow | CursorShape::System) {
            8
        } else {
            -(label_block_height / 2)
        };
        let top = match settings.label_position {
            LabelPosition::Right | LabelPosition::Left => side_label_top,
            LabelPosition::Above => glyph.top - gap - label_block_height,
            LabelPosition::Below => glyph.bottom + gap,
        };
        if has_label {
            name_rect = Some(Rect {
                left,
                top: top + (label_block_height - name_extent.1) / 2,
                right: left + name_extent.0,
                bottom: top + (label_block_height - name_extent.1) / 2 + name_extent.1,
            });
        }
        if inline_badge && (has_label || inline_badge_only) {
            let x = left + name_extent.0 + if has_label { 4 } else { 0 };
            let y = top + (label_block_height - badge_size.1) / 2;
            badge_rect = Some(Rect {
                left: x,
                top: y,
                right: x + badge_size.0,
                bottom: y + badge_size.1,
            });
        }
    }
    if settings.badge.is_some() && !inline_badge {
        let gap = 4;
        let rect = match settings.badge_position {
            BadgePosition::Inline => unreachable!(),
            BadgePosition::TopLeft => Rect {
                left: glyph.left - badge_size.0 - gap,
                top: glyph.top - badge_size.1 - gap,
                right: glyph.left - gap,
                bottom: glyph.top - gap,
            },
            BadgePosition::TopRight => Rect {
                left: glyph.right + gap,
                top: glyph.top - badge_size.1 - gap,
                right: glyph.right + gap + badge_size.0,
                bottom: glyph.top - gap,
            },
            BadgePosition::BottomLeft => Rect {
                left: glyph.left - badge_size.0 - gap,
                top: glyph.bottom + gap,
                right: glyph.left - gap,
                bottom: glyph.bottom + gap + badge_size.1,
            },
            BadgePosition::BottomRight => Rect {
                left: glyph.right + gap,
                top: glyph.bottom + gap,
                right: glyph.right + gap + badge_size.0,
                bottom: glyph.bottom + gap + badge_size.1,
            },
        };
        badge_rect = Some(rect);
    }
    if let Some(rect) = badge_rect {
        badge_text_rect = Some(Rect {
            left: rect.left + (rect.width() - badge_text_extent.0) / 2,
            top: rect.top + (rect.height() - badge_text_extent.1) / 2,
            right: rect.left + (rect.width() + badge_text_extent.0) / 2,
            bottom: rect.top + (rect.height() + badge_text_extent.1) / 2,
        });
    }
    let mut bounds = glyph;
    if let Some(rect) = name_rect {
        include_rect(&mut bounds, rect);
    }
    if let Some(rect) = badge_rect {
        include_rect(&mut bounds, rect);
    }
    let margin = 4;
    // Keep the cursor anchor at the old origin for arrows and at the center for
    // centered shapes. Only shift content when it extends left or above it.
    let translate_x = bounds.left.saturating_neg().max(0);
    let translate_y = bounds.top.saturating_neg().max(0);
    let width = bounds
        .right
        .checked_add(translate_x)
        .and_then(|value| value.checked_add(margin))
        .context("cursor canvas width overflow")?;
    let height = bounds
        .bottom
        .checked_add(translate_y)
        .and_then(|value| value.checked_add(margin))
        .context("cursor canvas height overflow")?;
    ensure!(
        (1..=MAX_CANVAS_WIDTH).contains(&width)
            && (1..=MAX_CANVAS_HEIGHT).contains(&height)
            && usize::try_from(width)
                .unwrap_or(usize::MAX)
                .saturating_mul(usize::try_from(height).unwrap_or(usize::MAX))
                <= 1024 * 1024,
        "cursor canvas exceeds the 1024×1024 pixel bound"
    );
    let translate = |rect: Option<Rect>| rect.map(|rect| rect.translated(translate_x, translate_y));
    Ok(Layout {
        size: (width, height),
        hotspot: (translate_x, translate_y),
        name: translate(name_rect),
        badge: translate(badge_rect),
        text: translate(badge_text_rect),
        rendered_label,
        label_truncated,
    })
}

pub(crate) fn stock_hotspot(kind: u32, size: i32) -> (i32, i32) {
    match kind {
        32512 | 32650 | 32651 => (0, 0),
        32649 => (size * 3 / 10, 0),
        _ => (size / 2, size / 2),
    }
}

// Original stock-state illustrations; the target sends identifiers, never OS
// cursor handles. These stay readable in each user's own color.
fn draw_stock(dc: HDC, kind: u32, size: i32, hx: i32, hy: i32) {
    let (hot_x, hot_y) = stock_hotspot(kind, size);
    let origin = (hx - hot_x, hy - hot_y);
    let point = |x: f64, y: f64| POINT {
        x: origin.0 + (x * f64::from(size)) as i32,
        y: origin.1 + (y * f64::from(size)) as i32,
    };
    let line = |points: &[(f64, f64)]| unsafe {
        let points: Vec<_> = points.iter().map(|&(x, y)| point(x, y)).collect();
        Polyline(dc, points.as_ptr(), points.len() as i32);
    };
    let polygon = |points: &[(f64, f64)]| unsafe {
        let points: Vec<_> = points.iter().map(|&(x, y)| point(x, y)).collect();
        Polygon(dc, points.as_ptr(), points.len() as i32);
    };
    unsafe {
        match kind {
            32512 | 32650 | 32651 => {
                polygon(&[
                    (0., 0.),
                    (0., 0.9),
                    (0.23, 0.67),
                    (0.46, 1.),
                    (0.6, 0.9),
                    (0.39, 0.56),
                    (0.8, 0.56),
                ]);
                if kind == 32650 {
                    let old = SelectObject(dc, GetStockObject(NULL_BRUSH));
                    let a = point(0.55, 0.1);
                    let b = point(1., 0.55);
                    Ellipse(dc, a.x, a.y, b.x, b.y);
                    SelectObject(dc, old);
                    line(&[(0.77, 0.15), (0.77, 0.32), (0.9, 0.32)]);
                } else if kind == 32651 {
                    line(&[
                        (0.65, 0.1),
                        (0.9, 0.1),
                        (1., 0.22),
                        (0.8, 0.35),
                        (0.8, 0.43),
                    ]);
                    line(&[(0.8, 0.52), (0.8, 0.57)]);
                }
            }
            32513 => {
                line(&[(0.3, 0.), (0.7, 0.)]);
                line(&[(0.5, 0.), (0.5, 1.)]);
                line(&[(0.3, 1.), (0.7, 1.)]);
            }
            32514 => {
                let old = SelectObject(dc, GetStockObject(NULL_BRUSH));
                let a = point(0.05, 0.05);
                let b = point(0.95, 0.95);
                Ellipse(dc, a.x, a.y, b.x, b.y);
                SelectObject(dc, old);
                line(&[(0.5, 0.18), (0.5, 0.5), (0.75, 0.65)]);
            }
            32515 => {
                line(&[(0., 0.5), (1., 0.5)]);
                line(&[(0.5, 0.), (0.5, 1.)]);
            }
            32516 => polygon(&[
                (0.5, 0.),
                (0.05, 0.45),
                (0.33, 0.45),
                (0.33, 1.),
                (0.67, 1.),
                (0.67, 0.45),
                (0.95, 0.45),
            ]),
            32649 => polygon(&[
                (0.3, 0.),
                (0.4, 0.),
                (0.4, 0.4),
                (0.52, 0.27),
                (0.62, 0.4),
                (0.72, 0.32),
                (0.88, 0.47),
                (0.92, 0.72),
                (0.75, 1.),
                (0.35, 1.),
                (0.05, 0.62),
                (0.13, 0.49),
                (0.3, 0.65),
            ]),
            32648 => {
                let old = SelectObject(dc, GetStockObject(NULL_BRUSH));
                let a = point(0.05, 0.05);
                let b = point(0.95, 0.95);
                Ellipse(dc, a.x, a.y, b.x, b.y);
                SelectObject(dc, old);
                line(&[(0.18, 0.82), (0.82, 0.18)]);
            }
            32642 | 32643 => {
                let flip = kind == 32643;
                let p = |x: f64, y: f64| if flip { (x, 1. - y) } else { (x, y) };
                line(&[p(0.05, 0.05), p(0.95, 0.95)]);
                line(&[p(0.05, 0.4), p(0.05, 0.05), p(0.4, 0.05)]);
                line(&[p(0.6, 0.95), p(0.95, 0.95), p(0.95, 0.6)]);
            }
            32644..=32646 => {
                if kind != 32645 {
                    line(&[(0., 0.5), (1., 0.5)]);
                    line(&[(0.23, 0.28), (0., 0.5), (0.23, 0.72)]);
                    line(&[(0.77, 0.28), (1., 0.5), (0.77, 0.72)]);
                }
                if kind != 32644 {
                    line(&[(0.5, 0.), (0.5, 1.)]);
                    line(&[(0.28, 0.23), (0.5, 0.), (0.72, 0.23)]);
                    line(&[(0.28, 0.77), (0.5, 1.), (0.72, 0.77)]);
                }
            }
            _ => {}
        }
    }
}

fn blend_image(
    canvas: &mut [u8],
    canvas_size: (i32, i32),
    anchor: (i32, i32),
    size: i32,
    image: &AppCursorImage,
) {
    if image.width == 0
        || image.height == 0
        || image.width > 256
        || image.height > 256
        || image.pixels.len() != image.width as usize * image.height as usize * 4
    {
        return;
    }
    let (width, height, hx, hy) = image_geometry(image, size);
    let left = anchor.0 - hx;
    let top = anchor.1 - hy;
    for y in 0..height {
        for x in 0..width {
            let dx = left + x;
            let dy = top + y;
            if dx < 0 || dy < 0 || dx >= canvas_size.0 || dy >= canvas_size.1 {
                continue;
            }
            let sx = x as usize * image.width as usize / width as usize;
            let sy = y as usize * image.height as usize / height as usize;
            let source = &image.pixels[(sy * image.width as usize + sx) * 4..][..4];
            let destination = &mut canvas[((dy * canvas_size.0 + dx) as usize) * 4..][..4];
            let result = system_cursor::source_over(
                [
                    destination[0],
                    destination[1],
                    destination[2],
                    destination[3],
                ],
                [source[0], source[1], source[2], source[3]],
            );
            destination.copy_from_slice(&result);
        }
    }
}

fn blend_system_glow(
    canvas: &mut [u8],
    canvas_size: (i32, i32),
    anchor: (i32, i32),
    size: i32,
    image: &AppCursorImage,
    color: [u8; 3],
) {
    if image.width == 0
        || image.height == 0
        || image.width > 256
        || image.height > 256
        || image.pixels.len() != image.width as usize * image.height as usize * 4
    {
        return;
    }
    let (width, height, hotspot_x, hotspot_y) = image_geometry(image, size);
    let left = anchor.0 - hotspot_x;
    let top = anchor.1 - hotspot_y;
    let mut alpha = Vec::with_capacity(width as usize * height as usize);
    for y in 0..height {
        for x in 0..width {
            let source_x = x as usize * image.width as usize / width as usize;
            let source_y = y as usize * image.height as usize / height as usize;
            alpha.push(image.pixels[(source_y * image.width as usize + source_x) * 4 + 3]);
        }
    }
    // Include transparent pixels inside the bitmap bounds so the halo follows the
    // cursor silhouette instead of the rectangular image edges.
    for y in -3..height + 3 {
        for x in -3..width + 3 {
            let glow_alpha =
                system_cursor::glow_alpha(width as usize, height as usize, &alpha, x, y);
            if glow_alpha == 0 {
                continue;
            }
            let target_x = left + x;
            let target_y = top + y;
            if target_x < 0
                || target_y < 0
                || target_x >= canvas_size.0
                || target_y >= canvas_size.1
            {
                continue;
            }
            let source = system_cursor::glow_bgra(color, glow_alpha);
            let destination =
                &mut canvas[((target_y * canvas_size.0 + target_x) as usize) * 4..][..4];
            let result = system_cursor::source_over(
                [
                    destination[0],
                    destination[1],
                    destination[2],
                    destination[3],
                ],
                source,
            );
            destination.copy_from_slice(&result);
        }
    }
}

fn render_key(name: &BoundedName, settings: &CursorSettings, paused: bool) -> RenderKey {
    RenderKey {
        application_kind: 0,
        application_hidden: false,
        application_image: 0,
        name: name.text.clone(),
        name_truncated: name.truncated,
        color: settings.color,
        size_bits: settings.size.to_bits(),
        shape: settings.shape,
        label: settings.label,
        label_position: settings.label_position,
        font_size_bits: settings.label_font_size.to_bits(),
        badge: settings.badge.clone(),
        badge_shape: settings.badge_shape,
        badge_position: settings.badge_position,
        paused,
        renderer: settings.renderer,
    }
}
impl Overlay {
    pub fn new(name: &str) -> Result<Self> {
        unsafe {
            let class = wide("ProtoInputMultiPlex.DesktopCursor");
            let instance = GetModuleHandleW(std::ptr::null());
            let wc = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassW(&wc); // The class may already belong to an earlier cursor on this thread.
            let title = wide(&format!("ProtoInputMultiPlex cursor — {name}"));
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                class.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                0,
                0,
                220,
                84,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            ensure!(
                !hwnd.is_null(),
                "create desktop cursor: {}",
                std::io::Error::last_os_error()
            );
            let print_surface = Box::new(RefCell::new(PrintSurface {
                canvas: None,
                recipe: None,
                size: (0, 0),
            }));
            // The 32-bit binding aliases SetWindowLongW (i32); x64 uses isize.
            let surface_pointer = (&*print_surface as *const RefCell<PrintSurface>) as _;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, surface_pointer);
            if GetWindowLongPtrW(hwnd, GWLP_USERDATA) != surface_pointer {
                DestroyWindow(hwnd);
                anyhow::bail!("attach cursor print surface to its window");
            }
            #[cfg(feature = "cursor-presentation-evidence")]
            let evidence = crate::cursor_evidence::Recorder::from_environment(hwnd);
            #[cfg(feature = "cursor-presentation-evidence")]
            let presenter = if evidence.is_some()
                && std::env::var("PROTOINPUTMULTIPLEX_GPU_CURSOR_TEST_FORCE_GDI").as_deref()
                    == Ok(name)
            {
                Presenter::forced_gdi(hwnd)
            } else {
                Presenter::new(hwnd)
            };
            #[cfg(not(feature = "cursor-presentation-evidence"))]
            let presenter = Presenter::new(hwnd);
            let presentation = presenter.status();
            Ok(Self {
                hwnd,
                presenter,
                print_surface,
                _thread: PhantomData,
                status: OverlayStatus {
                    presentation,
                    application_cursor: None,
                    hwnd: hwnd as usize,
                    x: 0,
                    y: 0,
                    visible: false,
                    no_activate: true,
                    click_through: true,
                    painted_pixels: 0,
                    shape: CursorShape::Arrow,
                    system_cursor_fallback: None,
                    badge: None,
                    render_origin: (0, 0),
                    hotspot: (0, 0),
                    pixel_hash: 0,
                    pixel_telemetry_source: None,
                    canvas_size: (0, 0),
                    idle_hidden: false,
                    rendered_label: String::new(),
                    label_truncated: false,
                    paint_generation: 0,
                },
                frame: None,
                paint_generation: 0,
                #[cfg(feature = "cursor-presentation-evidence")]
                evidence,
            })
        }
    }
    pub fn draw(
        &mut self,
        x: i32,
        y: i32,
        name: &str,
        settings: &CursorSettings,
        paused: bool,
    ) -> Result<()> {
        self.draw_application((x, y), name, settings, paused, None)
    }

    pub fn draw_application(
        &mut self,
        position: (i32, i32),
        name: &str,
        settings: &CursorSettings,
        paused: bool,
        application: Option<&AppCursorObservation>,
    ) -> Result<()> {
        let (x, y) = position;
        settings.validate()?;
        if self.presenter.set_renderer(self.hwnd, settings.renderer) {
            self.status.presentation = self.presenter.status();
            self.frame = None;
            let mut surface = self.print_surface.borrow_mut();
            surface.canvas = None;
            surface.recipe = None;
        }
        let bounded_name = bounded_name(name);
        let app = application.filter(|a| a.visible.is_some());
        let follow_mode = system_cursor::follow_mode(
            settings.follow_application,
            application.and_then(|observation| observation.visible),
        );
        let mut system_fallback = None;
        let system_image = if settings.shape == CursorShape::System {
            match follow_mode {
                system_cursor::FollowMode::ApplicationHidden => None,
                system_cursor::FollowMode::ApplicationVisible => {
                    let observation = app.expect("visible app cursor observation");
                    if let Some(image) = observation.image.as_ref() {
                        Some(image.clone())
                    } else if system_cursor::stock_kind(observation.kind).is_some() {
                        let result =
                            system_cursor::raster(observation.kind, settings.size.round() as i32);
                        system_fallback = result.note.map(str::to_owned);
                        result.image
                    } else {
                        let result = system_cursor::raster(32512, settings.size.round() as i32);
                        let mut reason = "Application cursor kind has no supported Windows resource mapping; showing the arrow fallback.".to_owned();
                        if let Some(note) = result.note {
                            reason.push(' ');
                            reason.push_str(note);
                        }
                        system_fallback = Some(reason);
                        result.image
                    }
                }
                system_cursor::FollowMode::ConfiguredFallback => {
                    let result = system_cursor::raster(32512, settings.size.round() as i32);
                    system_fallback = result.note.map(str::to_owned);
                    result.image
                }
            }
        } else {
            None
        };
        let synthesized_app = match follow_mode {
            system_cursor::FollowMode::ApplicationHidden
                if settings.shape == CursorShape::System =>
            {
                Some(AppCursorObservation {
                    kind: app.map_or(0, |observation| observation.kind),
                    visible: Some(false),
                    image: None,
                    ..AppCursorObservation::default()
                })
            }
            system_cursor::FollowMode::ApplicationVisible
                if settings.shape == CursorShape::System =>
            {
                Some(AppCursorObservation {
                    kind: app.map_or(32512, |observation| {
                        if observation.image.is_some() {
                            observation.kind
                        } else {
                            system_cursor::stock_kind(observation.kind).unwrap_or(32512)
                        }
                    }),
                    visible: Some(true),
                    image: system_image.clone(),
                    ..AppCursorObservation::default()
                })
            }
            system_cursor::FollowMode::ConfiguredFallback
                if settings.shape == CursorShape::System =>
            {
                Some(AppCursorObservation {
                    kind: 32512,
                    visible: Some(true),
                    image: system_image.clone(),
                    ..AppCursorObservation::default()
                })
            }
            _ => None,
        };
        let render_app = if settings.shape == CursorShape::System {
            synthesized_app.as_ref()
        } else {
            app
        };
        let mut key = render_key(&bounded_name, settings, paused);
        if let Some(app) = render_app {
            key.application_kind = app.kind;
            key.application_hidden = app.visible == Some(false);
            key.application_image = app.image.as_ref().map_or(0, |image| image.identity);
        }
        self.status.application_cursor = application.cloned();
        self.status.system_cursor_fallback = system_fallback.clone();
        self.status.x = x;
        self.status.y = y;
        self.status.shape = settings.shape;
        self.status.badge = settings.badge.clone();

        if !settings.visible {
            if self.frame.as_ref().is_some_and(|frame| frame.key != key) {
                self.frame = None;
                let previous_surface = {
                    let mut surface = self.print_surface.borrow_mut();
                    surface.size = (0, 0);
                    surface.recipe = None;
                    surface.canvas.take()
                };
                drop(previous_surface);
            }
            unsafe {
                ShowWindow(self.hwnd, SW_HIDE);
                if let Some(frame) = self.frame.as_ref().cloned() {
                    let origin = (
                        x.saturating_sub(frame.hotspot.0),
                        y.saturating_sub(frame.hotspot.1),
                    );
                    ensure!(
                        SetWindowPos(
                            self.hwnd,
                            std::ptr::null_mut(),
                            origin.0,
                            origin.1,
                            0,
                            0,
                            SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER,
                        ) != 0,
                        "position hidden cursor"
                    );
                    self.apply_frame_status(&frame, origin, false);
                } else {
                    ensure!(
                        SetWindowPos(
                            self.hwnd,
                            std::ptr::null_mut(),
                            x,
                            y,
                            0,
                            0,
                            SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER,
                        ) != 0,
                        "position hidden cursor without a cached frame"
                    );
                    self.status.visible = false;
                    self.status.painted_pixels = 0;
                    self.status.pixel_hash = 0;
                    self.status.pixel_telemetry_source = Some("not_rendered".into());
                    self.status.canvas_size = (0, 0);
                    self.status.rendered_label.clear();
                    self.status.label_truncated = false;
                    self.status.render_origin = (x, y);
                    self.status.hotspot = (0, 0);
                }
            }
            return Ok(());
        }

        if let Some(mut frame) = self
            .frame
            .as_ref()
            .filter(|frame| frame.key == key)
            .cloned()
        {
            let origin = (
                x.saturating_sub(frame.hotspot.0),
                y.saturating_sub(frame.hotspot.1),
            );
            if !self.presenter.reuse_gpu(frame.generation) {
                let mut surface = self.print_surface.borrow_mut();
                if surface.canvas.is_none() {
                    surface.canvas = Some(rasterize_gdi(&frame.recipe)?);
                }
                let canvas = surface
                    .canvas
                    .as_ref()
                    .context("missing cached GDI cursor")?;
                self.presenter.present(
                    self.hwnd,
                    canvas,
                    frame.canvas_size,
                    origin,
                    frame.generation,
                )?;
                (frame.painted_pixels, frame.pixel_hash) = canvas_metrics(canvas);
                self.frame = Some(frame.clone());
            }
            self.status.presentation = self.presenter.status();
            unsafe {
                ensure!(
                    SetWindowPos(
                        self.hwnd,
                        HWND_TOPMOST,
                        origin.0,
                        origin.1,
                        frame.canvas_size.0,
                        frame.canvas_size.1,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    ) != 0,
                    "position cached cursor"
                );
                self.apply_frame_status(&frame, origin, IsWindowVisible(self.hwnd) != 0);
            }
            #[cfg(feature = "cursor-presentation-evidence")]
            self.record_evidence();
            return Ok(());
        }

        let measure_dc = MeasureDc::new()?;
        let font = font(settings.label_font_size)?;
        let layout = {
            let _selected = SelectedObject::new(measure_dc.0, font.0, "select cursor label font")?;
            layout_application(settings, &bounded_name, measure_dc.0, render_app)?
        };
        let origin = (
            x.saturating_sub(layout.hotspot.0),
            y.saturating_sub(layout.hotspot.1),
        );
        let generation = self.paint_generation.saturating_add(1);
        let recipe = RenderRecipe {
            settings: settings.clone(),
            layout: layout.clone(),
            paused,
            application: render_app.cloned(),
        };
        #[cfg(feature = "cursor-presentation-evidence")]
        if let Some(evidence) = &mut self.evidence {
            self.presenter.set_readback_enabled(evidence.prepare(
                generation,
                layout.size,
                self.presenter.status().backend != "gdi",
            ));
        }
        let mut canvas = None;
        let (painted_pixels, pixel_hash) = if self
            .presenter
            .rasterize(self.hwnd, &recipe, origin, generation)
        {
            (0, 0) // No CPU render/readback just to manufacture pixel telemetry.
        } else {
            let rendered = rasterize_gdi(&recipe)?;
            self.presenter
                .present(self.hwnd, &rendered, layout.size, origin, generation)?;
            let metrics = canvas_metrics(&rendered);
            canvas = Some(rendered);
            metrics
        };
        self.status.presentation = self.presenter.status();
        unsafe {
            ensure!(
                SetWindowPos(
                    self.hwnd,
                    HWND_TOPMOST,
                    origin.0,
                    origin.1,
                    layout.size.0,
                    layout.size.1,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW
                ) != 0,
                "position desktop cursor"
            );
        }
        self.paint_generation = generation;
        let frame = RenderFrame {
            key,
            recipe: recipe.clone(),
            canvas_size: layout.size,
            hotspot: layout.hotspot,
            painted_pixels,
            pixel_hash,
            rendered_label: layout.rendered_label,
            label_truncated: layout.label_truncated,
            generation,
        };
        self.apply_frame_status(&frame, origin, unsafe { IsWindowVisible(self.hwnd) != 0 });
        self.frame = Some(frame);
        let mut surface = self.print_surface.borrow_mut();
        surface.size = layout.size;
        surface.canvas = canvas;
        surface.recipe = Some(recipe);
        drop(surface);
        #[cfg(feature = "cursor-presentation-evidence")]
        self.record_evidence();
        Ok(())
    }

    #[cfg(feature = "cursor-presentation-evidence")]
    fn record_evidence(&mut self) {
        if let Some(evidence) = &mut self.evidence {
            // The owned window procedure only reads this retained surface for
            // WM_PRINT. A shared borrow is reentrant-safe; it never calls draw
            // or mutates this canvas, and all presenter work stays on the owner.
            let surface = self.print_surface.borrow();
            evidence.record(&self.presenter, surface.canvas.as_ref(), &self.status);
        }
    }

    fn apply_frame_status(&mut self, frame: &RenderFrame, origin: (i32, i32), visible: bool) {
        self.status.visible = visible;
        self.status.painted_pixels = frame.painted_pixels;
        self.status.pixel_hash = frame.pixel_hash;
        self.status.pixel_telemetry_source = Some(
            if self.presenter.status().raster_backend
                == crate::cursor_presenter::RasterBackend::Direct2D
            {
                "unavailable_gpu"
            } else {
                "cpu_gdi"
            }
            .into(),
        );
        self.status.canvas_size = frame.canvas_size;
        self.status.render_origin = origin;
        self.status.hotspot = frame.hotspot;
        self.status.rendered_label.clone_from(&frame.rendered_label);
        self.status.label_truncated = frame.label_truncated;
        self.status.paint_generation = frame.generation;
    }
}
fn canvas_metrics(canvas: &DibCanvas) -> (usize, u64) {
    let pixels = unsafe {
        std::slice::from_raw_parts(
            canvas.bits as *const u32,
            (canvas.size.0 * canvas.size.1) as usize,
        )
    };
    crate::cursor_telemetry::pixel_telemetry(pixels)
}

fn rasterize_gdi(recipe: &RenderRecipe) -> Result<DibCanvas> {
    let settings = &recipe.settings;
    let layout = &recipe.layout;
    let paused = recipe.paused;
    let render_app = recipe.application.as_ref();
    let font = font(settings.label_font_size)?;
    unsafe {
        let canvas = DibCanvas::new(layout.size.0, layout.size.1)?;
        let _canvas_font = SelectedObject::new(canvas.dc, font.0, "select cursor drawing font")?;
        let pixels_len = usize::try_from(layout.size.0)
            .context("negative cursor canvas width")?
            .checked_mul(usize::try_from(layout.size.1).context("negative cursor canvas height")?)
            .context("cursor canvas allocation overflow")?;
        std::ptr::write_bytes(canvas.bits, 0, pixels_len * 4);

        let rgb = if paused {
            [148, 163, 184]
        } else {
            settings.color
        };
        let color =
            (u32::from(rgb[0]) | (u32::from(rgb[1]) << 8) | (u32::from(rgb[2]) << 16)).max(1);
        let pen_color = if render_app.map_or(
            matches!(settings.shape, CursorShape::Arrow | CursorShape::System),
            |a| matches!(a.kind, 32512 | 32649 | 32650 | 32651),
        ) {
            0x00FFFFFF
        } else {
            color
        };
        let brush = OwnedGdiObject(CreateSolidBrush(color) as HGDIOBJ);
        ensure!(!brush.0.is_null(), "create cursor brush");
        let pen = OwnedGdiObject(CreatePen(PS_SOLID, 2, pen_color) as HGDIOBJ);
        ensure!(!pen.0.is_null(), "create cursor pen");
        let _selected_brush = SelectedObject::new(canvas.dc, brush.0, "select cursor brush")?;
        let _selected_pen = SelectedObject::new(canvas.dc, pen.0, "select cursor pen")?;
        let size = settings.size.round() as i32;
        let (hx, hy) = layout.hotspot;
        if let Some(app) = render_app {
            if app.visible == Some(true) && app.image.is_none() {
                draw_stock(canvas.dc, app.kind, size, hx, hy);
            }
        } else {
            match settings.shape {
                CursorShape::Arrow | CursorShape::System => {
                    let points = [
                        (1., 1.),
                        (1., f64::from(size)),
                        (f64::from(size) * 0.28, f64::from(size) * 0.72),
                        (f64::from(size) * 0.52, f64::from(size) * 1.12),
                        (f64::from(size) * 0.7, f64::from(size) * 1.02),
                        (f64::from(size) * 0.47, f64::from(size) * 0.62),
                        (f64::from(size) * 0.83, f64::from(size) * 0.62),
                    ]
                    .map(|(px, py)| POINT {
                        x: hx + px as i32,
                        y: hy + py as i32,
                    });
                    Polygon(canvas.dc, points.as_ptr(), points.len() as i32);
                }
                CursorShape::Crosshair => {
                    let half = size / 2;
                    MoveToEx(canvas.dc, hx - half, hy, std::ptr::null_mut());
                    LineTo(canvas.dc, hx + half + 1, hy);
                    MoveToEx(canvas.dc, hx, hy - half, std::ptr::null_mut());
                    LineTo(canvas.dc, hx, hy + half + 1);
                    Ellipse(canvas.dc, hx - 2, hy - 2, hx + 3, hy + 3);
                }
                CursorShape::IBeam => {
                    let half = size / 2;
                    let bar_half_width = (size / 2).min(7);
                    MoveToEx(canvas.dc, hx, hy - half, std::ptr::null_mut());
                    LineTo(canvas.dc, hx, hy + half + 1);
                    MoveToEx(
                        canvas.dc,
                        hx - bar_half_width,
                        hy - half,
                        std::ptr::null_mut(),
                    );
                    LineTo(canvas.dc, hx + bar_half_width + 1, hy - half);
                    MoveToEx(
                        canvas.dc,
                        hx - bar_half_width,
                        hy + half,
                        std::ptr::null_mut(),
                    );
                    LineTo(canvas.dc, hx + bar_half_width + 1, hy + half);
                }
                CursorShape::Ring => {
                    let half = size / 2;
                    let no_brush = GetStockObject(NULL_BRUSH);
                    let _no_brush =
                        SelectedObject::new(canvas.dc, no_brush, "select cursor ring brush")?;
                    Ellipse(
                        canvas.dc,
                        hx - half,
                        hy - half,
                        hx + half + 1,
                        hy + half + 1,
                    );
                }
            }
        }
        drop(_selected_pen);
        drop(_selected_brush);

        SetBkMode(canvas.dc, TRANSPARENT as i32);
        SetTextColor(canvas.dc, color);
        if let Some(rect) = layout.name {
            let text = wide(&layout.rendered_label);
            TextOutW(
                canvas.dc,
                rect.left,
                rect.top,
                text.as_ptr(),
                text.len() as i32 - 1,
            );
        }
        if let (Some(rect), Some(text_rect), Some(badge_text)) =
            (layout.badge, layout.text, settings.badge.as_deref())
        {
            let badge_brush = OwnedGdiObject(CreateSolidBrush(color) as HGDIOBJ);
            let badge_pen = OwnedGdiObject(CreatePen(PS_SOLID, 1, 0x00FFFFFF) as HGDIOBJ);
            ensure!(
                !badge_brush.0.is_null() && !badge_pen.0.is_null(),
                "create cursor badge drawing objects"
            );
            let _badge_brush = SelectedObject::new(canvas.dc, badge_brush.0, "select badge brush")?;
            let _badge_pen = SelectedObject::new(canvas.dc, badge_pen.0, "select badge pen")?;
            match settings.badge_shape {
                BadgeShape::Rounded => RoundRect(
                    canvas.dc,
                    rect.left,
                    rect.top,
                    rect.right,
                    rect.bottom,
                    8,
                    8,
                ),
                BadgeShape::Box => {
                    Rectangle(canvas.dc, rect.left, rect.top, rect.right, rect.bottom)
                }
                BadgeShape::Circle => {
                    Ellipse(canvas.dc, rect.left, rect.top, rect.right, rect.bottom)
                }
            };
            drop(_badge_pen);
            drop(_badge_brush);
            let text_color = if rgb.iter().map(|c| u32::from(*c)).sum::<u32>() > 3 * 127 {
                0x00101010
            } else {
                0x00FFFFFF
            };
            SetTextColor(canvas.dc, text_color);
            let text = wide(badge_text);
            TextOutW(
                canvas.dc,
                text_rect.left,
                text_rect.top,
                text.as_ptr(),
                text.len() as i32 - 1,
            );
        }
        drop(_canvas_font);
        GdiFlush();
        // GDI writes RGB with an unused alpha byte. Make that existing artwork
        // opaque, then composite target-provided premultiplied pixels separately.
        let bytes = std::slice::from_raw_parts_mut(canvas.bits as *mut u8, pixels_len * 4);
        for pixel in bytes.as_chunks_mut::<4>().0 {
            pixel[3] = if pixel[..3].iter().any(|c| *c != 0) {
                255
            } else {
                0
            };
        }
        if let Some(image) = render_app
            .filter(|a| a.visible == Some(true))
            .and_then(|a| a.image.as_ref())
        {
            if settings.shape == CursorShape::System {
                blend_system_glow(
                    bytes,
                    layout.size,
                    layout.hotspot,
                    settings.size.round() as i32,
                    image,
                    rgb,
                );
            }
            blend_image(
                bytes,
                layout.size,
                layout.hotspot,
                settings.size.round() as i32,
                image,
            );
        }
        Ok(canvas)
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        self.presenter.shutdown();
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            DestroyWindow(self.hwnd);
        }
    }
}
pub struct DesktopGuard {
    owns_hotkey: bool,
}
impl DesktopGuard {
    pub fn start() -> Result<Self> {
        Self::create(true)
    }
    pub fn cursor_thread() -> Result<Self> {
        Self::create(false)
    }
    fn create(hotkey: bool) -> Result<Self> {
        unsafe {
            ensure!(
                !SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).is_null(),
                "set desktop cursor DPI context"
            );
            if hotkey {
                ensure!(
                    RegisterHotKey(
                        std::ptr::null_mut(),
                        1,
                        MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_NOREPEAT,
                        VK_F11 as u32
                    ) != 0,
                    "Cannot register Ctrl+Alt+Shift+F11 emergency stop: {}",
                    std::io::Error::last_os_error()
                );
            }
            Ok(Self {
                owns_hotkey: hotkey,
            })
        }
    }
    pub fn pump(&self) -> bool {
        unsafe {
            let mut message: MSG = std::mem::zeroed();
            let mut stop = false;
            while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                if message.message == WM_HOTKEY {
                    stop = true;
                }
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            stop
        }
    }
}
impl Drop for DesktopGuard {
    fn drop(&mut self) {
        unsafe {
            if self.owns_hotkey {
                UnregisterHotKey(std::ptr::null_mut(), 1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cpu_settings() -> CursorSettings {
        CursorSettings {
            renderer: mux_core::desktop::CursorRenderer::Gdi,
            ..Default::default()
        }
    }

    #[test]
    fn legacy_overlay_status_gets_default_presentation_telemetry() {
        let status: OverlayStatus = serde_json::from_value(serde_json::json!({
            "hwnd": 0, "x": 12, "y": 34, "visible": false,
            "no_activate": true, "click_through": true, "painted_pixels": 0
        }))
        .unwrap();
        assert_eq!(status.presentation.backend, "pending");
        assert_eq!(status.presentation.gpu_uploads, 0);
        assert_eq!(status.presentation.gpu_presents, 0);
        assert_eq!(status.presentation.composition_commits, 0);
        assert!(status.presentation.adapter.is_none());
        assert!(status.presentation.fallback_reason.is_none());
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(value["presentation"]["backend"], "pending");
        assert!(value.get("application_cursor").is_some());
    }

    #[test]
    fn historical_gpu_upload_status_does_not_claim_gpu_rasterization() {
        let status: PresentationStatus = serde_json::from_value(serde_json::json!({
            "backend":"direct_composition_d3d11", "adapter":"historical",
            "fallback_reason":null, "gpu_uploads":7, "gpu_presents":7,
            "composition_commits":7
        }))
        .unwrap();
        assert_eq!(status.gpu_uploads, 7);
        assert_eq!(status.raster_backend, RasterBackend::Unknown);
        assert_eq!((status.gpu_draws, status.asset_uploads), (0, 0));
    }

    #[test]
    fn overlay_lifecycle_and_emergency_message() {
        let guard = DesktopGuard::start().unwrap();
        let mut overlay = Overlay::new("Virtual test cursor").unwrap();
        overlay
            .draw(64, 80, "Virtual test cursor", &cpu_settings(), false)
            .unwrap();
        assert!(
            overlay.status.visible && overlay.status.click_through && overlay.status.no_activate
        );
        assert!(overlay.status.painted_pixels > 20);
        let arrow_hash = overlay.status.pixel_hash;
        assert_eq!(overlay.status.render_origin, (64, 80));
        assert_eq!(overlay.status.hotspot, (0, 0));
        let mut hashes = std::collections::HashSet::from([arrow_hash]);
        for shape in [
            CursorShape::Crosshair,
            CursorShape::IBeam,
            CursorShape::Ring,
        ] {
            overlay
                .draw(
                    64,
                    80,
                    "Virtual test cursor",
                    &CursorSettings {
                        shape,
                        badge: Some("U1".into()),
                        ..cpu_settings()
                    },
                    false,
                )
                .unwrap();
            assert_eq!(overlay.status.shape, shape);
            assert_eq!(overlay.status.badge.as_deref(), Some("U1"));
            assert_eq!(overlay.status.hotspot, (14, 14));
            assert_eq!(overlay.status.render_origin, (50, 66));
            let mut rect: RECT = unsafe { std::mem::zeroed() };
            assert_ne!(unsafe { GetWindowRect(overlay.hwnd, &mut rect) }, 0);
            assert_eq!((rect.left, rect.top), (50, 66));
            assert_eq!(
                (
                    rect.left + overlay.status.hotspot.0,
                    rect.top + overlay.status.hotspot.1
                ),
                (64, 80)
            );
            assert!(hashes.insert(overlay.status.pixel_hash));
        }
        overlay
            .draw(64, 80, "Virtual test cursor", &cpu_settings(), false)
            .unwrap();
        let hwnd = overlay.hwnd;
        unsafe {
            let mut rect: RECT = std::mem::zeroed();
            assert_ne!(GetWindowRect(hwnd, &mut rect), 0);
            assert_eq!((rect.left, rect.top), (64, 80));
            let thread = windows_sys::Win32::System::Threading::GetCurrentThreadId();
            assert_ne!(PostThreadMessageW(thread, WM_HOTKEY, 1, 0), 0);
        }
        assert!(guard.pump());
        overlay
            .draw(
                64,
                80,
                "Virtual test cursor",
                &CursorSettings {
                    visible: false,
                    shape: CursorShape::Ring,
                    badge: Some("U1".into()),
                    ..cpu_settings()
                },
                false,
            )
            .unwrap();
        assert!(!overlay.status.visible);
        assert_eq!(overlay.status.x, 64);
        assert_eq!(overlay.status.y, 80);
        assert_eq!(overlay.status.shape, CursorShape::Ring);
        assert_eq!(overlay.status.badge.as_deref(), Some("U1"));
        drop(overlay);
        assert_eq!(unsafe { IsWindow(hwnd) }, 0);
    }

    #[test]
    fn measured_overlay_stays_bounded_and_reuses_pixels_when_moving() {
        let mut overlay = Overlay::new("Worst-case virtual cursor").unwrap();
        let settings = CursorSettings {
            size: 256.0,
            shape: CursorShape::Ring,
            label_position: LabelPosition::Left,
            label_font_size: 48.0,
            badge: Some("WWWWWWWW".into()),
            badge_shape: BadgeShape::Circle,
            badge_position: BadgePosition::TopRight,
            ..cpu_settings()
        };
        let long_name = "W".repeat(MAX_LABEL_CHARS);
        overlay
            .draw(100, 120, &long_name, &settings, false)
            .unwrap();
        assert!(overlay.status.visible);
        assert!(overlay.status.label_truncated);
        assert!(overlay.status.rendered_label.chars().count() <= MAX_LABEL_CHARS);
        assert!(overlay.status.canvas_size.0 <= MAX_CANVAS_WIDTH);
        assert!(overlay.status.canvas_size.1 <= MAX_CANVAS_HEIGHT);
        assert!(overlay.status.painted_pixels > 0);
        let first_generation = overlay.status.paint_generation;
        let first_hash = overlay.status.pixel_hash;
        assert_eq!(first_generation, 1);

        let (surface_size, expected_pixels) = {
            let surface = overlay.print_surface.borrow();
            let canvas = surface.canvas.as_ref().expect("retained cursor surface");
            let len = usize::try_from(surface.size.0 * surface.size.1).unwrap();
            let pixels =
                unsafe { std::slice::from_raw_parts(canvas.bits as *const u32, len).to_vec() };
            (surface.size, pixels)
        };
        let print_target = DibCanvas::new(surface_size.0, surface_size.1).unwrap();
        unsafe {
            std::ptr::write_bytes(
                print_target.bits,
                0,
                expected_pixels.len() * std::mem::size_of::<u32>(),
            );
        }
        // This checks retained CPU print compatibility, not compositor/GPU capture.
        assert_ne!(
            unsafe {
                SendMessageW(
                    overlay.hwnd,
                    WM_PRINTCLIENT,
                    print_target.dc as WPARAM,
                    (PRF_CLIENT | PRF_ERASEBKGND) as LPARAM,
                )
            },
            0
        );
        unsafe { GdiFlush() };
        let captured_pixels = unsafe {
            std::slice::from_raw_parts(print_target.bits as *const u32, expected_pixels.len())
        };
        // The print target's alpha can differ; compare its RGB copy.
        assert!(
            captured_pixels
                .iter()
                .zip(&expected_pixels)
                .all(|(captured, expected)| { captured & 0x00ff_ffff == expected & 0x00ff_ffff })
        );

        overlay
            .draw(105, 124, &long_name, &settings, false)
            .unwrap();
        assert_eq!(overlay.status.paint_generation, first_generation);
        assert_eq!(overlay.status.pixel_hash, first_hash);
        assert_eq!(
            (
                overlay.status.render_origin.0 + overlay.status.hotspot.0,
                overlay.status.render_origin.1 + overlay.status.hotspot.1,
            ),
            (105, 124)
        );

        let hidden = CursorSettings {
            visible: false,
            ..settings.clone()
        };
        overlay.draw(105, 124, &long_name, &hidden, false).unwrap();
        assert!(!overlay.status.visible);
        assert_eq!(overlay.status.paint_generation, first_generation);
        overlay
            .draw(105, 124, &long_name, &settings, false)
            .unwrap();
        assert!(overlay.status.visible);
        assert_eq!(overlay.status.paint_generation, first_generation);

        let thin_name = "i".repeat(MAX_LABEL_CHARS + 1);
        let thin_font = CursorSettings {
            label_font_size: 8.0,
            ..cpu_settings()
        };
        overlay
            .draw(105, 124, &thin_name, &thin_font, false)
            .unwrap();
        assert!(overlay.status.label_truncated);
        assert_eq!(
            overlay.status.rendered_label.chars().count(),
            MAX_LABEL_CHARS
        );

        let prefix = "n".repeat(MAX_LABEL_CHARS + 1);
        let suffix_a = "a".repeat(64 * 1024);
        let suffix_b = "b".repeat(64 * 1024);
        let long_a = format!("{prefix}{suffix_a}");
        let long_b = format!("{prefix}{suffix_b}");
        let ordinary = cpu_settings();
        overlay.draw(105, 124, &long_a, &ordinary, false).unwrap();
        let bounded_generation = overlay.status.paint_generation;
        overlay.draw(105, 124, &long_b, &ordinary, false).unwrap();
        assert_eq!(overlay.status.paint_generation, bounded_generation);

        let hidden_without_frame = CursorSettings {
            visible: false,
            size: 40.0,
            ..cpu_settings()
        };
        overlay
            .draw(
                321,
                456,
                "Changed while hidden",
                &hidden_without_frame,
                false,
            )
            .unwrap();
        let mut rect: RECT = unsafe { std::mem::zeroed() };
        assert_ne!(unsafe { GetWindowRect(overlay.hwnd, &mut rect) }, 0);
        assert_eq!((rect.left, rect.top), (321, 456));
        assert_eq!(overlay.status.render_origin, (321, 456));
        assert!(!overlay.status.visible);
    }

    #[test]
    fn application_image_preserves_alpha_hotspot_and_cached_metadata() {
        let mut overlay = Overlay::new("Application image test").unwrap();
        let mut pixels = [32, 64, 96, 128].repeat(16);
        pixels[..4].copy_from_slice(&[0, 0, 0, 255]);
        let mut app = AppCursorObservation {
            state: "Following application cursor".into(),
            capability: true,
            sequence: 1,
            kind: 2,
            visible: Some(true),
            image: Some(AppCursorImage {
                width: 4,
                height: 4,
                hotspot: (1, 2),
                pixels: pixels.into(),
                identity: 123,
            }),
            ..Default::default()
        };
        let settings = CursorSettings {
            size: 32.,
            ..cpu_settings()
        };
        overlay
            .draw_application((100, 120), "Agent", &settings, false, Some(&app))
            .unwrap();
        assert_eq!(overlay.status.hotspot, (8, 16));
        assert_eq!(overlay.status.render_origin, (92, 104));
        {
            let surface = overlay.print_surface.borrow();
            let canvas = surface.canvas.as_ref().unwrap();
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    canvas.bits as *const u8,
                    surface.size.0 as usize * surface.size.1 as usize * 4,
                )
            };
            assert_eq!(
                &bytes[..4],
                &[0, 0, 0, 255],
                "opaque black must not become transparent"
            );
            assert_eq!(
                &bytes[8 * 4..8 * 4 + 4],
                &[32, 64, 96, 128],
                "custom premultiplied alpha must survive GDI artwork conversion"
            );
        }
        let generation = overlay.status.paint_generation;
        app.sequence = 2;
        overlay
            .draw_application((104, 125), "Agent", &settings, false, Some(&app))
            .unwrap();
        assert_eq!(overlay.status.paint_generation, generation);
        assert_eq!(
            overlay.status.application_cursor.as_ref().unwrap().sequence,
            2
        );
        app.visible = Some(false);
        app.kind = 0;
        app.image = None;
        overlay
            .draw_application((104, 125), "Agent", &settings, false, Some(&app))
            .unwrap();
        assert_eq!(overlay.status.rendered_label, "Agent");
        assert!(
            overlay.status.painted_pixels > 0,
            "hiding the app image preserves the user label"
        );
    }
}

#[cfg(test)]
#[path = "overlay_native_acceptance.rs"]
mod native_acceptance;
