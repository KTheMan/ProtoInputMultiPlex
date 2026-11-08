//! Portable pixel inspection and bounded exhaustive image matching.
//!
//! Frames and templates are tightly packed RGBA8. Matching compares RGB bytes
//! only; alpha is ignored and does not mask a pixel from the score.
use anyhow::{Context, Result, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const MAX_IMAGE_PIXELS: usize = 16_777_216;
pub const MAX_TEMPLATE_WIDTH: u32 = 128;
pub const MAX_TEMPLATE_HEIGHT: u32 = 128;
pub const MAX_MATCH_OPERATIONS: u64 = 64_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Match {
    pub top_left: [u32; 2],
    pub size: [u32; 2],
    /// Geometric center in capture-relative pixel-boundary coordinates.
    pub center: [f64; 2],
    /// RGB mean-absolute-error similarity in [0, 1]; alpha is ignored.
    pub score: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct Image<'a> {
    width: u32,
    height: u32,
    rgba: &'a [u8],
}

impl<'a> Image<'a> {
    pub fn new(width: u32, height: u32, rgba: &'a [u8]) -> Result<Self> {
        ensure!(width > 0 && height > 0, "image dimensions must be positive");
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .context("image pixel count overflow")?;
        ensure!(
            pixels <= MAX_IMAGE_PIXELS as u64,
            "image exceeds the 16 Mi-pixel limit"
        );
        let expected = pixels.checked_mul(4).context("image byte count overflow")?;
        let expected =
            usize::try_from(expected).context("image byte count exceeds address space")?;
        ensure!(
            rgba.len() == expected,
            "RGBA byte length does not match image dimensions"
        );
        Ok(Self {
            width,
            height,
            rgba,
        })
    }

    pub const fn width(&self) -> u32 {
        self.width
    }

    pub const fn height(&self) -> u32 {
        self.height
    }

    pub fn pixel(&self, x: u32, y: u32) -> Result<[u8; 4]> {
        ensure!(
            x < self.width && y < self.height,
            "pixel coordinate is outside image"
        );
        let offset = ((u64::from(y) * u64::from(self.width) + u64::from(x)) * 4) as usize;
        Ok(self.rgba[offset..offset + 4]
            .try_into()
            .expect("validated RGBA pixel slice"))
    }

    pub fn crop(&self, region: Region) -> Result<Vec<u8>> {
        self.validate_region(region)?;
        let bytes = u64::from(region.width)
            .checked_mul(u64::from(region.height))
            .and_then(|pixels| pixels.checked_mul(4))
            .context("crop byte count overflow")?;
        let bytes = usize::try_from(bytes).context("crop byte count exceeds address space")?;
        let mut crop = Vec::new();
        crop.try_reserve_exact(bytes)
            .context("unable to allocate crop buffer")?;
        let source_stride = self.width as usize * 4;
        let row_bytes = region.width as usize * 4;
        for y in region.y..region.y + region.height {
            let start = y as usize * source_stride + region.x as usize * 4;
            crop.extend_from_slice(&self.rgba[start..start + row_bytes]);
        }
        Ok(crop)
    }

    pub fn find(
        &self,
        template: &Image<'_>,
        region: Option<Region>,
        min_score: f64,
    ) -> Result<Option<Match>> {
        ensure!(
            min_score.is_finite() && (0.0..=1.0).contains(&min_score),
            "minimum match score must be finite and in [0, 1]"
        );
        ensure!(
            template.width <= MAX_TEMPLATE_WIDTH && template.height <= MAX_TEMPLATE_HEIGHT,
            "template exceeds the 128x128 limit"
        );
        let search = region.unwrap_or(Region {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
        });
        self.validate_region(search)?;
        if template.width > search.width || template.height > search.height {
            return Ok(None);
        }

        let columns = search
            .width
            .checked_sub(template.width)
            .and_then(|value| value.checked_add(1))
            .context("match candidate width overflow")?;
        let rows = search
            .height
            .checked_sub(template.height)
            .and_then(|value| value.checked_add(1))
            .context("match candidate height overflow")?;
        let candidate_count = u64::from(columns)
            .checked_mul(u64::from(rows))
            .context("match candidate count overflow")?;
        let template_pixels = u64::from(template.width)
            .checked_mul(u64::from(template.height))
            .context("template pixel count overflow")?;
        let operations = candidate_count
            .checked_mul(template_pixels)
            .context("match operation count overflow")?;
        ensure!(
            operations <= MAX_MATCH_OPERATIONS,
            "image match exceeds the 64,000,000 candidate-pixel budget"
        );

        let max_error = template_pixels * 3 * 255;
        let mut best: Option<Match> = None;
        for offset_y in 0..rows {
            for offset_x in 0..columns {
                let x = search.x + offset_x;
                let y = search.y + offset_y;
                let error = self.rgb_error(template, x, y);
                let score = 1.0 - error as f64 / max_error as f64;
                if best.as_ref().is_none_or(|current| score > current.score) {
                    best = Some(Match {
                        top_left: [x, y],
                        size: [template.width, template.height],
                        center: [
                            f64::from(x) + f64::from(template.width) / 2.0,
                            f64::from(y) + f64::from(template.height) / 2.0,
                        ],
                        score,
                    });
                }
            }
        }
        Ok(best.filter(|result| result.score >= min_score))
    }

    fn validate_region(&self, region: Region) -> Result<()> {
        ensure!(
            region.width > 0 && region.height > 0,
            "region dimensions must be positive"
        );
        let right = region
            .x
            .checked_add(region.width)
            .context("region horizontal bound overflow")?;
        let bottom = region
            .y
            .checked_add(region.height)
            .context("region vertical bound overflow")?;
        ensure!(
            right <= self.width && bottom <= self.height,
            "region is outside image"
        );
        Ok(())
    }

    fn rgb_error(&self, template: &Image<'_>, x: u32, y: u32) -> u64 {
        let mut error = 0_u64;
        for template_y in 0..template.height {
            for template_x in 0..template.width {
                let frame_offset = (((y + template_y) as usize * self.width as usize)
                    + (x + template_x) as usize)
                    * 4;
                let template_offset =
                    (template_y as usize * template.width as usize + template_x as usize) * 4;
                for channel in 0..3 {
                    error += u64::from(
                        self.rgba[frame_offset + channel]
                            .abs_diff(template.rgba[template_offset + channel]),
                    );
                }
            }
        }
        error
    }
}

/// Free-function form for callers that prefer to keep the frame explicit.
pub fn find(
    image: &Image<'_>,
    template: &Image<'_>,
    region: Option<Region>,
    min_score: f64,
) -> Result<Option<Match>> {
    image.find(template, region, min_score)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(width: u32, height: u32, pixels: &[[u8; 4]]) -> Vec<u8> {
        assert_eq!(pixels.len(), width as usize * height as usize);
        pixels.iter().flatten().copied().collect()
    }

    #[test]
    fn exact_match_region_last_edge_and_geometric_center() {
        let frame_bytes = rgba(
            4,
            3,
            &[
                [0, 0, 0, 0],
                [0, 0, 0, 0],
                [0, 0, 0, 0],
                [0, 0, 0, 0],
                [0, 0, 0, 0],
                [0, 0, 0, 0],
                [10, 20, 30, 0],
                [40, 50, 60, 0],
                [0, 0, 0, 0],
                [0, 0, 0, 0],
                [70, 80, 90, 0],
                [100, 110, 120, 0],
            ],
        );
        let needle_bytes = rgba(
            2,
            2,
            &[
                [10, 20, 30, 255],
                [40, 50, 60, 255],
                [70, 80, 90, 255],
                [100, 110, 120, 255],
            ],
        );
        let frame = Image::new(4, 3, &frame_bytes).unwrap();
        let needle = Image::new(2, 2, &needle_bytes).unwrap();
        let found = frame.find(&needle, None, 1.0).unwrap().unwrap();
        assert_eq!(found.top_left, [2, 1], "last legal row/column is included");
        assert_eq!(found.size, [2, 2]);
        assert_eq!(found.center, [3.0, 2.0]);
        assert_eq!(found.score, 1.0);
        let region = Region {
            x: 2,
            y: 1,
            width: 2,
            height: 2,
        };
        assert_eq!(frame.find(&needle, Some(region), 1.0).unwrap(), Some(found));
        assert_eq!(
            find(&frame, &needle, Some(region), 1.0)
                .unwrap()
                .unwrap()
                .top_left,
            [2, 1]
        );
    }

    #[test]
    fn exhaustive_best_match_threshold_ties_and_alpha_ignored() {
        let frame_bytes = rgba(3, 1, &[[10, 10, 10, 0], [20, 20, 20, 0], [10, 10, 10, 255]]);
        let needle_bytes = rgba(1, 1, &[[10, 10, 10, 255]]);
        let frame = Image::new(3, 1, &frame_bytes).unwrap();
        let needle = Image::new(1, 1, &needle_bytes).unwrap();
        let best = frame.find(&needle, None, 1.0).unwrap().unwrap();
        assert_eq!(
            best.top_left,
            [0, 0],
            "equal best scores keep first row-major result"
        );
        assert_eq!(best.score, 1.0, "alpha is ignored without masking RGB");
        assert!(
            frame
                .find(
                    &needle,
                    Some(Region {
                        x: 1,
                        y: 0,
                        width: 1,
                        height: 1
                    }),
                    0.97
                )
                .unwrap()
                .is_none()
        );
        let approximate = frame
            .find(
                &needle,
                Some(Region {
                    x: 1,
                    y: 0,
                    width: 1,
                    height: 1,
                }),
                0.95,
            )
            .unwrap()
            .unwrap();
        assert_eq!(approximate.top_left, [1, 0]);
        assert!((approximate.score - (1.0 - 30.0 / 765.0)).abs() < 1e-12);
    }

    #[test]
    fn exact_zero_region_crop_and_pixel_bounds() {
        let bytes = rgba(
            2,
            2,
            &[
                [1, 2, 3, 4],
                [5, 6, 7, 8],
                [9, 10, 11, 12],
                [13, 14, 15, 16],
            ],
        );
        let image = Image::new(2, 2, &bytes).unwrap();
        assert_eq!(image.pixel(1, 1).unwrap(), [13, 14, 15, 16]);
        assert!(image.pixel(2, 0).is_err());
        assert_eq!(
            image
                .crop(Region {
                    x: 1,
                    y: 0,
                    width: 1,
                    height: 2
                })
                .unwrap(),
            [5, 6, 7, 8, 13, 14, 15, 16]
        );
        assert!(
            image
                .crop(Region {
                    x: 1,
                    y: 1,
                    width: 2,
                    height: 1
                })
                .is_err()
        );
        assert!(
            image
                .crop(Region {
                    x: u32::MAX,
                    y: 0,
                    width: 1,
                    height: 1
                })
                .is_err()
        );
        assert!(
            image
                .find(
                    &image,
                    Some(Region {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 2
                    }),
                    0.0
                )
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn image_template_score_and_operation_limits_are_explicit() {
        assert!(Image::new(0, 1, &[]).is_err());
        assert!(Image::new(1, 1, &[1, 2, 3]).is_err());
        assert!(Image::new(u32::MAX, u32::MAX, &[]).is_err());
        let image_bytes = [0_u8; 4];
        let image = Image::new(1, 1, &image_bytes).unwrap();
        for invalid in [f64::NAN, f64::INFINITY, -0.01, 1.01] {
            assert!(image.find(&image, None, invalid).is_err());
        }
        let large_needle_bytes = vec![0_u8; 129 * 128 * 4];
        let large_needle = Image::new(129, 128, &large_needle_bytes).unwrap();
        assert!(image.find(&large_needle, None, 0.0).is_err());

        let frame_bytes = vec![0_u8; 1000 * 1000 * 4];
        let frame = Image::new(1000, 1000, &frame_bytes).unwrap();
        let template_bytes = vec![0_u8; 128 * 128 * 4];
        let template = Image::new(128, 128, &template_bytes).unwrap();
        assert!(
            frame.find(&template, None, 0.0).is_err(),
            "budget rejects before scanning"
        );
        assert_eq!(MAX_MATCH_OPERATIONS, 64_000_000);
    }

    #[test]
    fn image_dimensions_and_region_serde_are_strict() {
        let pixels = vec![0_u8; MAX_IMAGE_PIXELS * 4];
        assert!(Image::new(4096, 4096, &pixels).is_ok());
        assert!(Image::new(4097, 4096, &pixels).is_err());
        assert!(
            serde_json::from_str::<Region>(r#"{"x":0,"y":0,"width":1,"height":1,"extra":1}"#)
                .is_err()
        );
        let schema = schemars::schema_for!(Region);
        assert!(serde_json::to_string(&schema).unwrap().contains("width"));
    }
}
