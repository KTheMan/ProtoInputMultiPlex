//! Portable magnifier settings, bounded viewport geometry, and nearest-neighbor rendering.
use crate::desktop::Bounds;
use anyhow::{Context, Result, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const MAX_PIXELS: u64 = 16_000_000;
const MIN_VIEW_SIZE: u32 = 32;
const MAX_VIEW_SIZE: u32 = 4096;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Off,
    Lens,
    Docked,
    Fullscreen,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    #[default]
    SelectedTarget,
    Desktop,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DockSide {
    #[default]
    Right,
    Left,
    Top,
    Bottom,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub mode: Mode,
    pub source: Source,
    pub zoom: f64,
    pub width: u32,
    pub height: u32,
    pub border: bool,
    pub border_color: [u8; 3],
    pub auto_refresh: bool,
    pub refresh_hz: u8,
    pub monitor_id: Option<String>,
    pub work_area: bool,
    pub dock: DockSide,
    pub desktop_region: Option<Bounds>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Off,
            source: Source::SelectedTarget,
            zoom: 2.0,
            width: 320,
            height: 240,
            border: true,
            border_color: [96, 165, 250],
            auto_refresh: true,
            refresh_hz: 8,
            monitor_id: None,
            work_area: true,
            dock: DockSide::Right,
            desktop_region: None,
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.zoom.is_finite() && (1.0..=32.0).contains(&self.zoom),
            "magnifier zoom must be finite and between 1 and 32"
        );
        ensure!(
            (MIN_VIEW_SIZE..=MAX_VIEW_SIZE).contains(&self.width)
                && (MIN_VIEW_SIZE..=MAX_VIEW_SIZE).contains(&self.height),
            "magnifier dimensions must each be between 32 and 4096 pixels"
        );
        ensure!(
            pixel_count(self.width, self.height)? <= MAX_PIXELS,
            "magnifier output exceeds the 16 million pixel limit"
        );
        ensure!(
            (1..=30).contains(&self.refresh_hz),
            "magnifier refresh rate must be between 1 and 30 Hz"
        );
        if let Some(id) = &self.monitor_id {
            ensure!(
                !id.trim().is_empty() && id.len() <= 1024,
                "magnifier monitor ID must contain 1 through 1024 bytes"
            );
        }
        if let Some(region) = self.desktop_region {
            ensure!(region.valid_region(), "magnifier desktop region is invalid");
            ensure!(
                pixel_count(bounds_width(region)?, bounds_height(region)?)? <= MAX_PIXELS,
                "magnifier desktop region exceeds the 16 million pixel limit"
            );
        }
        Ok(())
    }
}

/// Places a viewport inside the effective monitor/work-area bounds.
pub fn placement(settings: &Settings, area: Bounds, cursor: (i32, i32)) -> Result<Bounds> {
    settings.validate()?;
    ensure!(area.valid_region(), "magnifier placement area is invalid");
    let area_width = bounds_width(area)?;
    let area_height = bounds_height(area)?;
    let bounds = match settings.mode {
        Mode::Off => anyhow::bail!("magnifier is disabled"),
        Mode::Fullscreen => area,
        Mode::Lens | Mode::Docked => {
            ensure!(
                settings.width <= area_width && settings.height <= area_height,
                "magnifier viewport does not fit inside the selected display area"
            );
            let width = settings.width as i32;
            let height = settings.height as i32;
            let (left, top) = match settings.mode {
                Mode::Lens => {
                    let left = i64::from(cursor.0) + 24;
                    let top = i64::from(cursor.1) + 24;
                    (
                        left.clamp(
                            i64::from(area.left),
                            i64::from(area.right) - i64::from(width),
                        ) as i32,
                        top.clamp(
                            i64::from(area.top),
                            i64::from(area.bottom) - i64::from(height),
                        ) as i32,
                    )
                }
                Mode::Docked => match settings.dock {
                    DockSide::Right => (
                        area.right - width,
                        area.top + ((area_height - settings.height) / 2) as i32,
                    ),
                    DockSide::Left => (
                        area.left,
                        area.top + ((area_height - settings.height) / 2) as i32,
                    ),
                    DockSide::Top => (
                        area.left + ((area_width - settings.width) / 2) as i32,
                        area.top,
                    ),
                    DockSide::Bottom => (
                        area.left + ((area_width - settings.width) / 2) as i32,
                        area.bottom - height,
                    ),
                },
                Mode::Off | Mode::Fullscreen => unreachable!(),
            };
            Bounds {
                left,
                top,
                right: left + width,
                bottom: top + height,
            }
        }
    };
    ensure!(
        bounds.valid_region(),
        "magnifier viewport bounds are invalid"
    );
    ensure!(
        pixel_count(bounds_width(bounds)?, bounds_height(bounds)?)? <= MAX_PIXELS,
        "magnifier viewport exceeds the 16 million pixel limit"
    );
    Ok(bounds)
}

/// Computes a source ROI that contains every pixel the zoomed viewport samples.
/// The returned bounds are clipped to the supplied source rectangle; rendering edge-clamps
/// when the logical cursor lies outside that source.
pub fn capture_region(
    settings: &Settings,
    source_bounds: Bounds,
    cursor: (i32, i32),
    output_bounds: Bounds,
) -> Result<Bounds> {
    settings.validate()?;
    ensure!(
        source_bounds.valid_region(),
        "magnifier source bounds are invalid"
    );
    ensure!(
        output_bounds.valid_region(),
        "magnifier output bounds are invalid"
    );
    let out_width = bounds_width(output_bounds)?;
    let out_height = bounds_height(output_bounds)?;
    ensure!(
        pixel_count(out_width, out_height)? <= MAX_PIXELS,
        "magnifier output exceeds the 16 million pixel limit"
    );
    // Include one extra source pixel for floor-based nearest-neighbor sampling at the
    // half-pixel edges when a small high-zoom viewport is centered on an integer cursor.
    let roi_width = ((f64::from(out_width) / settings.zoom).ceil() as u32 + 1).max(1);
    let roi_height = ((f64::from(out_height) / settings.zoom).ceil() as u32 + 1).max(1);
    let mut desired = Bounds {
        left: (i64::from(cursor.0) - i64::from(roi_width) / 2).clamp(-1_000_000, 1_000_000) as i32,
        top: (i64::from(cursor.1) - i64::from(roi_height) / 2).clamp(-1_000_000, 1_000_000) as i32,
        right: 0,
        bottom: 0,
    };
    desired.right = (i64::from(desired.left) + i64::from(roi_width)).min(1_000_000) as i32;
    desired.bottom = (i64::from(desired.top) + i64::from(roi_height)).min(1_000_000) as i32;
    let intersection = source_bounds.intersection(desired);
    if let Some(region) = intersection {
        ensure!(
            pixel_count(bounds_width(region)?, bounds_height(region)?)? <= MAX_PIXELS,
            "magnifier capture region exceeds the 16 million pixel limit"
        );
        return Ok(region);
    }
    // The cursor is outside a selected target: capture its nearest edge pixel so the
    // renderer can show a stable, edge-clamped view instead of requesting a huge frame.
    let x = cursor.0.clamp(source_bounds.left, source_bounds.right - 1);
    let y = cursor.1.clamp(source_bounds.top, source_bounds.bottom - 1);
    Ok(Bounds {
        left: x,
        top: y,
        right: x + 1,
        bottom: y + 1,
    })
}

/// Renders a cropped nearest-neighbor zoom as straight-alpha RGBA with an optional 2px border.
pub fn render(
    source: &[u8],
    source_bounds: Bounds,
    cursor: (i32, i32),
    output_bounds: Bounds,
    settings: &Settings,
) -> Result<Vec<u8>> {
    settings.validate()?;
    ensure!(settings.mode != Mode::Off, "magnifier is disabled");
    ensure!(
        source_bounds.valid_region(),
        "magnifier source bounds are invalid"
    );
    ensure!(
        output_bounds.valid_region(),
        "magnifier output bounds are invalid"
    );
    let source_width = bounds_width(source_bounds)?;
    let source_height = bounds_height(source_bounds)?;
    ensure!(
        pixel_count(source_width, source_height)? <= MAX_PIXELS,
        "magnifier source exceeds the 16 million pixel limit"
    );
    let source_bytes = byte_count(source_width, source_height)?;
    ensure!(
        source.len() == source_bytes,
        "magnifier source buffer length does not match its bounds"
    );
    let output_width = bounds_width(output_bounds)?;
    let output_height = bounds_height(output_bounds)?;
    let output_bytes = byte_count(output_width, output_height)?;
    ensure!(
        pixel_count(output_width, output_height)? <= MAX_PIXELS,
        "magnifier output exceeds the 16 million pixel limit"
    );
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(output_bytes)
        .context("reserve magnifier output pixels")?;
    rgba.resize(output_bytes, 0);

    let half_width = f64::from(output_width) * 0.5;
    let half_height = f64::from(output_height) * 0.5;
    let min_source_x = f64::from(source_bounds.left) + 0.5;
    let max_source_x = f64::from(source_bounds.right) - 0.5;
    let min_source_y = f64::from(source_bounds.top) + 0.5;
    let max_source_y = f64::from(source_bounds.bottom) - 0.5;
    for y in 0..output_height {
        for x in 0..output_width {
            let border = settings.border
                && (x < 2 || y < 2 || x + 2 >= output_width || y + 2 >= output_height);
            let offset = ((y as usize) * (output_width as usize) + x as usize) * 4;
            if border {
                rgba[offset..offset + 4].copy_from_slice(&[
                    settings.border_color[0],
                    settings.border_color[1],
                    settings.border_color[2],
                    255,
                ]);
                continue;
            }
            let source_x = (f64::from(cursor.0)
                + (f64::from(x) + 0.5 - half_width) / settings.zoom)
                .clamp(min_source_x, max_source_x);
            let source_y = (f64::from(cursor.1)
                + (f64::from(y) + 0.5 - half_height) / settings.zoom)
                .clamp(min_source_y, max_source_y);
            let sx = (source_x.floor() as i64 - i64::from(source_bounds.left)) as usize;
            let sy = (source_y.floor() as i64 - i64::from(source_bounds.top)) as usize;
            let source_offset = (sy * source_width as usize + sx) * 4;
            rgba[offset..offset + 4].copy_from_slice(&source[source_offset..source_offset + 4]);
        }
    }
    Ok(rgba)
}

fn bounds_width(bounds: Bounds) -> Result<u32> {
    u32::try_from(i64::from(bounds.right) - i64::from(bounds.left))
        .context("magnifier bounds width is invalid")
}

fn bounds_height(bounds: Bounds) -> Result<u32> {
    u32::try_from(i64::from(bounds.bottom) - i64::from(bounds.top))
        .context("magnifier bounds height is invalid")
}

fn pixel_count(width: u32, height: u32) -> Result<u64> {
    u64::from(width)
        .checked_mul(u64::from(height))
        .context("magnifier pixel count overflow")
}

fn byte_count(width: u32, height: u32) -> Result<usize> {
    usize::try_from(
        pixel_count(width, height)?
            .checked_mul(4)
            .context("magnifier byte count overflow")?,
    )
    .context("magnifier image is too large")
}

#[cfg(test)]
mod tests {
    use super::{DockSide, Mode, Settings, capture_region, placement, render};
    use crate::desktop::Bounds;

    fn source(width: usize, height: usize) -> Vec<u8> {
        let mut pixels = vec![0; width * height * 4];
        for y in 0..height {
            for x in 0..width {
                let offset = (y * width + x) * 4;
                pixels[offset..offset + 4].copy_from_slice(&[
                    (x % 251) as u8,
                    (y % 251) as u8,
                    if x < width / 2 { 32 } else { 220 },
                    255,
                ]);
            }
        }
        pixels
    }

    #[test]
    fn zoom_two_and_four_sample_distinct_known_source_pixels() {
        let source = source(32, 32);
        let source_bounds = Bounds {
            left: -16,
            top: -16,
            right: 16,
            bottom: 16,
        };
        for zoom in [2.0, 4.0] {
            let settings = Settings {
                mode: Mode::Lens,
                zoom,
                width: 32,
                height: 32,
                border: false,
                ..Settings::default()
            };
            let output = render(
                &source,
                source_bounds,
                (0, 0),
                Bounds {
                    left: 0,
                    top: 0,
                    right: 32,
                    bottom: 32,
                },
                &settings,
            )
            .unwrap();
            let center = ((16 * 32 + 16) * 4) as usize;
            assert_eq!(&output[center..center + 4], &[16, 16, 220, 255]);
            assert_eq!(output[0], if zoom == 2.0 { 8 } else { 12 });
            assert_ne!(output, source);
            let sampled_span = output
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| pixel[0])
                .collect::<std::collections::BTreeSet<_>>()
                .len();
            if zoom == 4.0 {
                assert!(sampled_span < 16);
            } else {
                assert!(sampled_span >= 16);
            }
        }
    }

    #[test]
    fn placement_clamps_negative_origins_and_supports_dock_and_fullscreen() {
        let area = Bounds {
            left: -1920,
            top: -200,
            right: 0,
            bottom: 880,
        };
        let lens = placement(
            &Settings {
                mode: Mode::Lens,
                width: 320,
                height: 240,
                ..Settings::default()
            },
            area,
            (-1900, -180),
        )
        .unwrap();
        assert!(area.contains(lens.left, lens.top));
        assert!(lens.right <= area.right && lens.bottom <= area.bottom);
        let docked = placement(
            &Settings {
                mode: Mode::Docked,
                dock: DockSide::Right,
                width: 320,
                height: 240,
                ..Settings::default()
            },
            area,
            (-500, 200),
        )
        .unwrap();
        assert_eq!(docked.right, area.right);
        assert_eq!(
            placement(
                &Settings {
                    mode: Mode::Fullscreen,
                    ..Settings::default()
                },
                area,
                (0, 0)
            )
            .unwrap(),
            area
        );
    }

    #[test]
    fn capture_region_clips_to_source_and_output_has_border_pixels() {
        let source_bounds = Bounds {
            left: -10,
            top: -10,
            right: 10,
            bottom: 10,
        };
        let settings = Settings {
            mode: Mode::Lens,
            zoom: 2.0,
            width: 32,
            height: 32,
            ..Settings::default()
        };
        assert_eq!(
            capture_region(
                &settings,
                source_bounds,
                (100, 100),
                Bounds {
                    left: 0,
                    top: 0,
                    right: 32,
                    bottom: 32
                }
            )
            .unwrap(),
            Bounds {
                left: 9,
                top: 9,
                right: 10,
                bottom: 10
            }
        );
        let pixels = source(20, 20);
        let rendered = render(
            &pixels,
            source_bounds,
            (0, 0),
            Bounds {
                left: 0,
                top: 0,
                right: 32,
                bottom: 32,
            },
            &settings,
        )
        .unwrap();
        assert_eq!(&rendered[..4], &[96, 165, 250, 255]);
        assert_ne!(rendered, vec![0; rendered.len()]);
    }

    #[test]
    fn invalid_zoom_region_and_image_sizes_are_rejected() {
        assert!(
            Settings {
                zoom: f64::NAN,
                ..Settings::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Settings {
                zoom: f64::INFINITY,
                ..Settings::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Settings {
                width: 5000,
                ..Settings::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Settings {
                desktop_region: Some(Bounds {
                    left: 2,
                    top: 0,
                    right: 1,
                    bottom: 1
                }),
                ..Settings::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            render(
                &[0; 15],
                Bounds {
                    left: 0,
                    top: 0,
                    right: 2,
                    bottom: 2
                },
                (0, 0),
                Bounds {
                    left: 0,
                    top: 0,
                    right: 2,
                    bottom: 2
                },
                &Settings {
                    mode: Mode::Lens,
                    ..Settings::default()
                }
            )
            .is_err()
        );
    }
}
