//! Portable, bounded per-user annotation strokes and deterministic RGBA rendering.
use crate::desktop::Bounds;
use anyhow::{Context, Result, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const MAX_SEGMENTS: usize = 100_000;
pub const MAX_SEGMENTS_PER_STROKE: usize = 8_192;
pub const MAX_EXPORT_PIXELS: u64 = 16_000_000;
const MAX_RENDER_CANDIDATE_PIXELS: u64 = 64_000_000;
const MAX_COORDINATE: i32 = 1_000_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tool {
    #[default]
    Off,
    Brush,
    Eraser,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Activator {
    #[default]
    Primary,
    Secondary,
    Middle,
    X1,
    X2,
}

impl Activator {
    pub const fn down_flag(self) -> u32 {
        match self {
            Self::Primary => 1,
            Self::Secondary => 4,
            Self::Middle => 16,
            Self::X1 => 64,
            Self::X2 => 256,
        }
    }

    pub const fn up_flag(self) -> u32 {
        self.down_flag() * 2
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub tool: Tool,
    pub activator: Activator,
    pub color: [u8; 3],
    pub alpha: u8,
    pub width: u16,
    pub clear_key: Option<u16>,
    pub clear_all: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            tool: Tool::Off,
            activator: Activator::Primary,
            color: [96, 165, 250],
            alpha: 255,
            width: 6,
            clear_key: None,
            clear_all: false,
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=128).contains(&self.width),
            "annotation brush width must be between 1 and 128 pixels"
        );
        if let Some(key) = self.clear_key {
            ensure!(
                (1..=255).contains(&key),
                "annotation clear key must be a virtual key from 1 through 255"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    fn validate(self) -> Result<()> {
        ensure!(
            (-MAX_COORDINATE..=MAX_COORDINATE).contains(&self.x)
                && (-MAX_COORDINATE..=MAX_COORDINATE).contains(&self.y),
            "annotation point is outside the supported coordinate range"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Segment {
    pub from: Point,
    pub to: Point,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Stroke {
    pub owner: usize,
    pub color: [u8; 3],
    pub alpha: u8,
    pub width: u16,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserStatus {
    pub user: usize,
    pub settings: Settings,
    pub strokes: usize,
    pub segments: usize,
    pub open: bool,
}

#[derive(Debug, Clone, Copy)]
enum OpenStroke {
    Brush { stroke_id: u64, last: Point },
    Eraser { last: Point, width: u16 },
}

struct EraseStrokePlan {
    stroke_index: usize,
    segments: Vec<Segment>,
}

struct ErasePlan {
    strokes: Vec<EraseStrokePlan>,
    total_segments: usize,
}

/// A bounded shared annotation document. Each user owns independent strokes and erasure.
#[derive(Clone)]
pub struct Document {
    settings: Vec<Settings>,
    strokes: Vec<(u64, Stroke)>,
    open: Vec<Option<OpenStroke>>,
    next_stroke_id: u64,
    total_segments: usize,
    revision: u64,
}

impl Document {
    pub fn new(users: usize) -> Result<Self> {
        let mut settings = Vec::new();
        settings
            .try_reserve_exact(users)
            .context("reserve annotation user settings")?;
        settings.resize(users, Settings::default());

        let mut open = Vec::new();
        open.try_reserve_exact(users)
            .context("reserve annotation user state")?;
        open.resize(users, None);

        Ok(Self {
            settings,
            strokes: Vec::new(),
            open,
            next_stroke_id: 1,
            total_segments: 0,
            revision: 0,
        })
    }

    pub fn users(&self) -> usize {
        self.settings.len()
    }

    pub fn reserve_user(&mut self) -> Result<()> {
        self.settings
            .try_reserve(1)
            .context("reserve annotation settings")?;
        self.open
            .try_reserve(1)
            .context("reserve annotation lane")?;
        Ok(())
    }

    pub fn append_user(&mut self) -> usize {
        let user = self.users();
        self.settings.push(Settings::default());
        self.open.push(None);
        user
    }

    pub fn settings(&self, user: usize) -> Result<&Settings> {
        self.settings
            .get(user)
            .context("annotation user index is out of range")
    }

    /// Returns whether this user's brush or eraser is currently active.
    pub fn is_open(&self, user: usize) -> Result<bool> {
        self.open
            .get(user)
            .map(Option::is_some)
            .context("annotation user index is out of range")
    }

    pub fn configure(&mut self, user: usize, settings: Settings) -> Result<()> {
        settings.validate()?;
        ensure!(user < self.users(), "annotation user index is out of range");
        self.end(user)?;
        if self.settings[user] != settings {
            self.settings[user] = settings;
            self.bump_revision();
        }
        Ok(())
    }

    pub fn status(&self) -> Vec<UserStatus> {
        self.settings
            .iter()
            .enumerate()
            .map(|(user, settings)| {
                let strokes = self
                    .strokes
                    .iter()
                    .filter(|(_, stroke)| stroke.owner == user && !stroke.segments.is_empty())
                    .count();
                let segments = self
                    .strokes
                    .iter()
                    .filter(|(_, stroke)| stroke.owner == user)
                    .map(|(_, stroke)| stroke.segments.len())
                    .sum();
                UserStatus {
                    user,
                    settings: settings.clone(),
                    strokes,
                    segments,
                    open: self.open[user].is_some(),
                }
            })
            .collect()
    }

    pub fn begin(&mut self, user: usize, point: (i32, i32)) -> Result<()> {
        self.check_user_point(user, point)?;
        let tool = self.settings[user].tool;
        let next_stroke_id = if tool == Tool::Brush {
            ensure!(
                self.total_segments < MAX_SEGMENTS,
                "annotation segment budget is full"
            );
            ensure!(
                self.strokes.len() < MAX_SEGMENTS,
                "annotation stroke budget is full"
            );
            self.strokes
                .try_reserve(1)
                .context("reserve annotation stroke")?;
            Some(
                self.next_stroke_id
                    .checked_add(1)
                    .context("annotation stroke identifier exhausted")?,
            )
        } else {
            None
        };
        self.end(user)?;
        let point = Point::new(point.0, point.1);
        match tool {
            Tool::Off => return Ok(()),
            Tool::Brush => {
                let stroke_id = self.next_stroke_id;
                self.next_stroke_id = next_stroke_id.expect("brush identifier was reserved");
                let settings = &self.settings[user];
                self.strokes.push((
                    stroke_id,
                    Stroke {
                        owner: user,
                        color: settings.color,
                        alpha: settings.alpha,
                        width: settings.width,
                        segments: vec![Segment {
                            from: point,
                            to: point,
                        }],
                    },
                ));
                self.open[user] = Some(OpenStroke::Brush {
                    stroke_id,
                    last: point,
                });
                self.total_segments += 1;
                self.bump_revision();
            }
            Tool::Eraser => {
                let width = self.settings[user].width;
                let plan = self.plan_erase(user, point, point, width)?;
                self.apply_erase(plan);
                self.open[user] = Some(OpenStroke::Eraser { last: point, width });
                self.bump_revision();
            }
        }
        Ok(())
    }

    pub fn sample(&mut self, user: usize, point: (i32, i32)) -> Result<()> {
        self.check_user_point(user, point)?;
        let Some(open) = self.open[user] else {
            return Ok(());
        };
        let point = Point::new(point.0, point.1);
        match open {
            OpenStroke::Brush { stroke_id, last } => {
                if last == point {
                    return Ok(());
                }
                ensure!(
                    self.total_segments < MAX_SEGMENTS,
                    "annotation segment budget is full"
                );
                let (_, stroke) = self
                    .strokes
                    .iter_mut()
                    .find(|(id, _)| *id == stroke_id)
                    .context("open annotation stroke is missing")?;
                ensure!(
                    stroke.segments.len() < MAX_SEGMENTS_PER_STROKE,
                    "annotation stroke segment budget is full"
                );
                stroke
                    .segments
                    .try_reserve(1)
                    .context("reserve annotation segment")?;
                stroke.segments.push(Segment {
                    from: last,
                    to: point,
                });
                self.total_segments += 1;
                self.open[user] = Some(OpenStroke::Brush {
                    stroke_id,
                    last: point,
                });
                self.bump_revision();
            }
            OpenStroke::Eraser { last, width } => {
                let plan = self.plan_erase(user, last, point, width)?;
                let changed = plan.is_some();
                self.apply_erase(plan);
                self.open[user] = Some(OpenStroke::Eraser { last: point, width });
                if changed {
                    self.bump_revision();
                }
            }
        }
        Ok(())
    }

    pub fn end(&mut self, user: usize) -> Result<()> {
        let open = self
            .open
            .get_mut(user)
            .context("annotation user index is out of range")?;
        if open.take().is_some() {
            self.bump_revision();
        }
        Ok(())
    }

    pub fn end_all(&mut self) {
        let mut changed = false;
        for open in &mut self.open {
            changed |= open.take().is_some();
        }
        if changed {
            self.bump_revision();
        }
    }

    pub fn clear(&mut self, user: usize, all: bool) -> Result<()> {
        ensure!(user < self.users(), "annotation user index is out of range");
        if all {
            self.end_all();
            let changed = !self.strokes.is_empty();
            self.strokes.clear();
            self.total_segments = 0;
            if changed {
                self.bump_revision();
            }
        } else {
            self.end(user)?;
            let before = self.strokes.len();
            self.strokes.retain(|(_, stroke)| stroke.owner != user);
            if self.strokes.len() != before {
                self.recount_segments();
                self.bump_revision();
            }
        }
        Ok(())
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Rasterize the document into top-to-bottom straight-alpha RGBA pixels.
    pub fn render(&self, bounds: Bounds) -> Result<Vec<u8>> {
        self.render_owner(None, bounds)
    }

    /// Rasterize only this user's strokes, preserving their original paint order.
    pub fn render_user(&self, user: usize, bounds: Bounds) -> Result<Vec<u8>> {
        self.settings(user)?;
        self.render_owner(Some(user), bounds)
    }

    fn render_owner(&self, owner: Option<usize>, bounds: Bounds) -> Result<Vec<u8>> {
        ensure!(
            bounds.valid_region(),
            "annotation export bounds are invalid"
        );
        let width = i64::from(bounds.right) - i64::from(bounds.left);
        let height = i64::from(bounds.bottom) - i64::from(bounds.top);
        let pixels = u64::try_from(width)
            .context("annotation width is invalid")?
            .checked_mul(u64::try_from(height).context("annotation height is invalid")?)
            .context("annotation export pixel count overflow")?;
        ensure!(
            pixels <= MAX_EXPORT_PIXELS,
            "annotation export exceeds the 16 million pixel limit"
        );
        let byte_len = usize::try_from(
            pixels
                .checked_mul(4)
                .context("annotation export size overflow")?,
        )
        .context("annotation export is too large")?;
        let mut rgba = Vec::new();
        rgba.try_reserve_exact(byte_len)
            .context("reserve annotation export pixels")?;
        rgba.resize(byte_len, 0);

        let mut work = 0_u64;
        for (_, stroke) in &self.strokes {
            if stroke.alpha == 0 || owner.is_some_and(|owner| stroke.owner != owner) {
                continue;
            }
            for segment in &stroke.segments {
                raster_segment(
                    &mut rgba,
                    bounds,
                    *segment,
                    stroke.width,
                    stroke.color,
                    stroke.alpha,
                    &mut work,
                )?;
            }
        }
        Ok(rgba)
    }

    fn check_user_point(&self, user: usize, point: (i32, i32)) -> Result<()> {
        ensure!(user < self.users(), "annotation user index is out of range");
        Point::new(point.0, point.1).validate()
    }

    fn plan_erase(
        &self,
        owner: usize,
        from: Point,
        to: Point,
        width: u16,
    ) -> Result<Option<ErasePlan>> {
        let eraser_radius = f64::from(width) * 0.5;
        let mut replacements = Vec::new();
        let mut total_segments = self.total_segments;
        for (stroke_index, (_, stroke)) in self.strokes.iter().enumerate() {
            if stroke.owner != owner {
                continue;
            }
            let stroke_radius = f64::from(stroke.width) * 0.5;
            let threshold = eraser_radius + stroke_radius;
            let hit_count = stroke
                .segments
                .iter()
                .filter(|segment| {
                    capsule_parameter_interval(**segment, from, to, threshold).is_some()
                })
                .count();
            if hit_count == 0 {
                continue;
            }
            let reserve_count = stroke
                .segments
                .len()
                .checked_add(hit_count)
                .context("annotation eraser split count overflow")?;
            let mut new_segments = Vec::new();
            new_segments
                .try_reserve_exact(reserve_count)
                .context("reserve annotation eraser split plan")?;
            for segment in &stroke.segments {
                if let Some((erase_start, erase_end)) =
                    capsule_parameter_interval(*segment, from, to, threshold)
                {
                    let (parts, count) = subtract_interval(*segment, erase_start, erase_end);
                    new_segments.extend(parts.into_iter().take(count).flatten());
                } else {
                    new_segments.push(*segment);
                }
            }
            ensure!(
                new_segments.len() <= MAX_SEGMENTS_PER_STROKE,
                "annotation stroke segment budget is full after erasing"
            );
            total_segments = total_segments
                .checked_sub(stroke.segments.len())
                .and_then(|count| count.checked_add(new_segments.len()))
                .context("annotation segment count overflow")?;
            ensure!(
                total_segments <= MAX_SEGMENTS,
                "annotation segment budget is full after erasing"
            );
            replacements
                .try_reserve(1)
                .context("reserve annotation eraser replacement")?;
            replacements.push(EraseStrokePlan {
                stroke_index,
                segments: new_segments,
            });
        }
        if replacements.is_empty() {
            Ok(None)
        } else {
            Ok(Some(ErasePlan {
                strokes: replacements,
                total_segments,
            }))
        }
    }

    fn apply_erase(&mut self, plan: Option<ErasePlan>) {
        let Some(plan) = plan else { return };
        for replacement in plan.strokes {
            self.strokes[replacement.stroke_index].1.segments = replacement.segments;
        }
        self.total_segments = plan.total_segments;
    }

    fn recount_segments(&mut self) {
        self.total_segments = self
            .strokes
            .iter()
            .map(|(_, stroke)| stroke.segments.len())
            .sum();
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }
}

fn raster_segment(
    rgba: &mut [u8],
    bounds: Bounds,
    segment: Segment,
    width: u16,
    color: [u8; 3],
    alpha: u8,
    work: &mut u64,
) -> Result<()> {
    const SAMPLES: [f64; 4] = [0.125, 0.375, 0.625, 0.875];
    let radius = f64::from(width) * 0.5;
    let min_x = (f64::from(segment.from.x.min(segment.to.x)) - radius)
        .floor()
        .max(f64::from(bounds.left)) as i32;
    let min_y = (f64::from(segment.from.y.min(segment.to.y)) - radius)
        .floor()
        .max(f64::from(bounds.top)) as i32;
    let max_x = (f64::from(segment.from.x.max(segment.to.x)) + radius)
        .ceil()
        .min(f64::from(bounds.right)) as i32;
    let max_y = (f64::from(segment.from.y.max(segment.to.y)) + radius)
        .ceil()
        .min(f64::from(bounds.bottom)) as i32;
    if min_x >= max_x || min_y >= max_y {
        return Ok(());
    }
    let candidate_pixels = u64::try_from(i64::from(max_x) - i64::from(min_x))
        .context("annotation render width overflow")?
        .checked_mul(
            u64::try_from(i64::from(max_y) - i64::from(min_y))
                .context("annotation render height overflow")?,
        )
        .context("annotation render work overflow")?;
    *work = work
        .checked_add(candidate_pixels)
        .context("annotation render work overflow")?;
    ensure!(
        *work <= MAX_RENDER_CANDIDATE_PIXELS,
        "annotation render exceeds the bounded raster work limit"
    );

    let crop_width = usize::try_from(i64::from(bounds.right) - i64::from(bounds.left))?;
    let dx = f64::from(segment.to.x) - f64::from(segment.from.x);
    let dy = f64::from(segment.to.y) - f64::from(segment.from.y);
    let length_squared = dx * dx + dy * dy;
    let radius_squared = radius * radius;
    for y in min_y..max_y {
        for x in min_x..max_x {
            let mut covered = 0_u32;
            for sy in SAMPLES {
                for sx in SAMPLES {
                    let px = f64::from(x) + sx;
                    let py = f64::from(y) + sy;
                    let t = if length_squared == 0.0 {
                        0.0
                    } else {
                        (((px - f64::from(segment.from.x)) * dx
                            + (py - f64::from(segment.from.y)) * dy)
                            / length_squared)
                            .clamp(0.0, 1.0)
                    };
                    let nearest_x = f64::from(segment.from.x) + t * dx;
                    let nearest_y = f64::from(segment.from.y) + t * dy;
                    let distance_x = px - nearest_x;
                    let distance_y = py - nearest_y;
                    if distance_x * distance_x + distance_y * distance_y <= radius_squared {
                        covered += 1;
                    }
                }
            }
            if covered == 0 {
                continue;
            }
            let coverage_alpha = (u32::from(alpha) * covered + 8) / 16;
            let local_x = usize::try_from(i64::from(x) - i64::from(bounds.left))?;
            let local_y = usize::try_from(i64::from(y) - i64::from(bounds.top))?;
            let offset = (local_y * crop_width + local_x) * 4;
            composite_pixel(rgba, offset, color, coverage_alpha as u8);
        }
    }
    Ok(())
}

fn composite_pixel(rgba: &mut [u8], offset: usize, color: [u8; 3], source_alpha: u8) {
    if source_alpha == 0 {
        return;
    }
    let destination_alpha = u32::from(rgba[offset + 3]);
    let source_alpha = u32::from(source_alpha);
    let inverse_alpha = 255 - source_alpha;
    let out_alpha = source_alpha + (destination_alpha * inverse_alpha + 127) / 255;
    for (channel, source_color) in color.into_iter().enumerate() {
        let premultiplied = u32::from(source_color) * source_alpha
            + (u32::from(rgba[offset + channel]) * destination_alpha * inverse_alpha + 127) / 255;
        rgba[offset + channel] = ((premultiplied + out_alpha / 2) / out_alpha) as u8;
    }
    rgba[offset + 3] = out_alpha as u8;
}

/// Parameter range of a source segment covered by the eraser's round-ended capsule.
fn capsule_parameter_interval(
    source: Segment,
    eraser_from: Point,
    eraser_to: Point,
    radius: f64,
) -> Option<(f64, f64)> {
    let ax = f64::from(source.from.x);
    let ay = f64::from(source.from.y);
    let vx = f64::from(source.to.x) - ax;
    let vy = f64::from(source.to.y) - ay;
    let qx = f64::from(eraser_from.x);
    let qy = f64::from(eraser_from.y);
    let dx = f64::from(eraser_to.x) - qx;
    let dy = f64::from(eraser_to.y) - qy;
    let mut start = f64::INFINITY;
    let mut end = f64::NEG_INFINITY;

    let mut include_interval = |interval: Option<(f64, f64)>| {
        if let Some((left, right)) = interval {
            start = start.min(left);
            end = end.max(right);
        }
    };

    include_interval(line_circle_interval(ax, ay, vx, vy, qx, qy, radius));
    include_interval(line_circle_interval(
        ax,
        ay,
        vx,
        vy,
        f64::from(eraser_to.x),
        f64::from(eraser_to.y),
        radius,
    ));

    let axis_length_squared = dx * dx + dy * dy;
    if axis_length_squared > 0.0 {
        let relative_x = ax - qx;
        let relative_y = ay - qy;
        let projected_start = relative_x * dx + relative_y * dy;
        let projected_velocity = vx * dx + vy * dy;
        let cross_start = dx * relative_y - dy * relative_x;
        let cross_velocity = dx * vy - dy * vx;
        let cross_limit = radius * axis_length_squared.sqrt();
        let rectangle = linear_range_interval(
            projected_start,
            projected_velocity,
            0.0,
            axis_length_squared,
            (-cross_limit, cross_limit, cross_start, cross_velocity),
        );
        include_interval(rectangle);
    }

    if !start.is_finite() || !end.is_finite() || end - start <= 1e-10 {
        return None;
    }
    Some((start.clamp(0.0, 1.0), end.clamp(0.0, 1.0)))
}

fn line_circle_interval(
    ax: f64,
    ay: f64,
    vx: f64,
    vy: f64,
    cx: f64,
    cy: f64,
    radius: f64,
) -> Option<(f64, f64)> {
    let rx = ax - cx;
    let ry = ay - cy;
    let a = vx * vx + vy * vy;
    let c = rx * rx + ry * ry - radius * radius;
    if a == 0.0 {
        return (c <= 0.0).then_some((0.0, 1.0));
    }
    let b = 2.0 * (rx * vx + ry * vy);
    let discriminant = b * b - 4.0 * a * c;
    if discriminant < 0.0 {
        return None;
    }
    let root = discriminant.sqrt();
    let left = ((-b - root) / (2.0 * a)).max(0.0);
    let right = ((-b + root) / (2.0 * a)).min(1.0);
    (right > left).then_some((left, right))
}

fn linear_range_interval(
    projection_start: f64,
    projection_velocity: f64,
    projection_lower: f64,
    projection_upper: f64,
    cross: (f64, f64, f64, f64),
) -> Option<(f64, f64)> {
    let mut interval = (0.0, 1.0);
    if !clip_linear_range(
        &mut interval,
        projection_start,
        projection_velocity,
        projection_lower,
        projection_upper,
    ) {
        return None;
    }
    let (cross_lower, cross_upper, cross_start, cross_velocity) = cross;
    if !clip_linear_range(
        &mut interval,
        cross_start,
        cross_velocity,
        cross_lower,
        cross_upper,
    ) {
        return None;
    }
    (interval.1 > interval.0).then_some(interval)
}

fn clip_linear_range(
    interval: &mut (f64, f64),
    start: f64,
    velocity: f64,
    lower: f64,
    upper: f64,
) -> bool {
    if velocity == 0.0 {
        return (lower..=upper).contains(&start);
    }
    let first = (lower - start) / velocity;
    let second = (upper - start) / velocity;
    interval.0 = interval.0.max(first.min(second));
    interval.1 = interval.1.min(first.max(second));
    interval.1 > interval.0
}

fn subtract_interval(
    segment: Segment,
    erase_start: f64,
    erase_end: f64,
) -> ([Option<Segment>; 2], usize) {
    let mut parts = [None, None];
    let mut count = 0;
    if erase_start > 1e-10 {
        push_nonzero_part(&mut parts, &mut count, segment, 0.0, erase_start);
    }
    if erase_end < 1.0 - 1e-10 {
        push_nonzero_part(&mut parts, &mut count, segment, erase_end, 1.0);
    }
    (parts, count)
}

fn push_nonzero_part(
    parts: &mut [Option<Segment>; 2],
    count: &mut usize,
    segment: Segment,
    start: f64,
    end: f64,
) {
    let from = point_at(segment, start);
    let to = point_at(segment, end);
    if from != to {
        parts[*count] = Some(Segment { from, to });
        *count += 1;
    }
}

fn point_at(segment: Segment, t: f64) -> Point {
    let x = f64::from(segment.from.x) + (f64::from(segment.to.x) - f64::from(segment.from.x)) * t;
    let y = f64::from(segment.from.y) + (f64::from(segment.to.y) - f64::from(segment.from.y)) * t;
    Point::new(x.round() as i32, y.round() as i32)
}

#[cfg(test)]
mod tests {
    use super::{
        Activator, Document, MAX_SEGMENTS_PER_STROKE, Point, Segment, Settings, Stroke, Tool,
    };
    use crate::desktop::Bounds;

    fn brush(color: [u8; 3]) -> Settings {
        Settings {
            tool: Tool::Brush,
            color,
            width: 3,
            ..Settings::default()
        }
    }

    #[test]
    fn user_export_clips_to_bounds_preserves_alpha_and_original_paint_order() {
        let bounds = Bounds {
            left: -2,
            top: -2,
            right: 6,
            bottom: 4,
        };
        let mut doc = Document::new(2).unwrap();
        doc.configure(
            0,
            Settings {
                alpha: 128,
                width: 2,
                ..brush([255, 0, 0])
            },
        )
        .unwrap();
        doc.configure(
            1,
            Settings {
                width: 2,
                ..brush([0, 0, 255])
            },
        )
        .unwrap();
        doc.begin(0, (-4, 0)).unwrap();
        doc.sample(0, (3, 0)).unwrap();
        doc.end(0).unwrap();
        doc.begin(1, (2, 0)).unwrap();
        doc.sample(1, (5, 0)).unwrap();
        doc.end(1).unwrap();
        let first = doc.render_user(0, bounds).unwrap();
        let second = doc.render_user(1, bounds).unwrap();
        let all = doc.render(bounds).unwrap();
        let pixel = |rgba: &[u8], x: i32, y: i32| {
            let offset = (((y - bounds.top) * 8 + x - bounds.left) * 4) as usize;
            <[u8; 4]>::try_from(&rgba[offset..offset + 4]).unwrap()
        };
        assert_eq!(first.len(), 8 * 6 * 4);
        assert_eq!(pixel(&first, -2, 0), [255, 0, 0, 128]);
        assert_eq!(pixel(&second, -2, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(&first, 5, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(&first, 0, 3), [0, 0, 0, 0]);
        assert_eq!(pixel(&all, 2, 0), [0, 0, 255, 255]);

        doc.configure(
            0,
            Settings {
                width: 2,
                ..brush([0, 255, 0])
            },
        )
        .unwrap();
        doc.begin(0, (1, 0)).unwrap();
        doc.sample(0, (3, 0)).unwrap();
        doc.end(0).unwrap();
        assert_eq!(
            pixel(&doc.render_user(0, bounds).unwrap(), 2, 0),
            [0, 255, 0, 255]
        );
        assert_eq!(pixel(&doc.render(bounds).unwrap(), 2, 0), [0, 255, 0, 255]);
        assert!(doc.render_user(2, bounds).is_err());
    }

    #[test]
    fn cloned_document_is_an_independent_request_time_ink_snapshot() {
        let bounds = Bounds {
            left: 0,
            top: 0,
            right: 12,
            bottom: 6,
        };
        let mut live = Document::new(1).unwrap();
        live.configure(0, brush([24, 80, 160])).unwrap();
        live.begin(0, (2, 2)).unwrap();
        live.sample(0, (5, 2)).unwrap();
        let expected = live.render_user(0, bounds).unwrap();
        let snapshot = live.clone();
        let revision = snapshot.revision();

        live.sample(0, (10, 2)).unwrap();
        live.clear(0, true).unwrap();
        live.configure(0, brush([160, 80, 24])).unwrap();
        live.begin(0, (9, 4)).unwrap();
        assert_ne!(live.revision(), revision);
        assert_eq!(snapshot.revision(), revision);
        assert_eq!(snapshot.render_user(0, bounds).unwrap(), expected);
        assert_ne!(live.render_user(0, bounds).unwrap(), expected);
    }

    #[test]
    fn activators_cover_five_raw_button_pairs() {
        let buttons = [
            Activator::Primary,
            Activator::Secondary,
            Activator::Middle,
            Activator::X1,
            Activator::X2,
        ];
        assert_eq!(buttons.map(Activator::down_flag), [1, 4, 16, 64, 256]);
        assert_eq!(buttons.map(Activator::up_flag), [2, 8, 32, 128, 512]);
    }

    #[test]
    fn rendering_emits_real_alpha_pixels_and_clips_negative_origin() {
        let mut doc = Document::new(1).unwrap();
        let settings = Settings {
            alpha: 128,
            ..brush([20, 80, 240])
        };
        doc.configure(0, settings).unwrap();
        doc.begin(0, (-4, -3)).unwrap();
        doc.sample(0, (2, -3)).unwrap();
        doc.end(0).unwrap();

        let rgba = doc
            .render(Bounds {
                left: -6,
                top: -5,
                right: 5,
                bottom: 3,
            })
            .unwrap();
        assert_eq!(rgba.len(), 11 * 8 * 4);
        assert!(rgba.as_chunks::<4>().0.iter().any(|pixel| pixel[3] > 0));
        assert!(
            rgba.as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| (1..255).contains(&pixel[3]))
        );
        assert!(
            rgba.as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[3] == 0 || pixel[2] > 0)
        );
    }

    #[test]
    fn eraser_and_clear_are_scoped_to_the_owner() {
        let mut doc = Document::new(2).unwrap();
        doc.configure(0, brush([255, 0, 0])).unwrap();
        doc.configure(1, brush([0, 0, 255])).unwrap();
        for user in 0..2 {
            doc.begin(user, (2, 4)).unwrap();
            doc.sample(user, (14, 4)).unwrap();
            doc.end(user).unwrap();
        }

        doc.configure(
            0,
            Settings {
                tool: Tool::Eraser,
                width: 5,
                ..Settings::default()
            },
        )
        .unwrap();
        doc.begin(0, (7, 4)).unwrap();
        doc.end(0).unwrap();
        let rgba = doc
            .render(Bounds {
                left: 0,
                top: 0,
                right: 18,
                bottom: 10,
            })
            .unwrap();
        assert!(rgba.as_chunks::<4>().0.iter().any(|p| p[3] > 0 && p[2] > 0));
        assert_eq!(doc.status()[0].segments, 3);
        assert!(doc.status()[1].segments > 0);

        doc.clear(0, false).unwrap();
        assert_eq!(doc.status()[1].segments, 2);
        doc.clear(0, true).unwrap();
        assert!(doc.status().iter().all(|s| s.segments == 0 && !s.open));
    }

    #[test]
    fn eraser_splits_only_the_owner_segment_and_preserves_both_sides() {
        let mut doc = Document::new(2).unwrap();
        doc.configure(0, brush([255, 0, 0])).unwrap();
        doc.configure(1, brush([0, 0, 255])).unwrap();
        for (user, y) in [(0, 10), (1, 30)] {
            doc.begin(user, (5, y)).unwrap();
            doc.sample(user, (95, y)).unwrap();
            doc.end(user).unwrap();
        }
        let user1_segments = doc.status()[1].segments;
        doc.configure(
            0,
            Settings {
                tool: Tool::Eraser,
                width: 4,
                ..Settings::default()
            },
        )
        .unwrap();
        doc.begin(0, (50, 10)).unwrap();
        doc.end(0).unwrap();

        // Brush begin records a dot as well as the long segment, which the eraser splits in two.
        assert_eq!(doc.status()[0].segments, 3);
        assert_eq!(doc.status()[1].segments, user1_segments);
        let rgba = doc
            .render(Bounds {
                left: 0,
                top: 0,
                right: 100,
                bottom: 40,
            })
            .unwrap();
        let pixel = |x: usize, y: usize| {
            let offset = (y * 100 + x) * 4;
            [
                rgba[offset],
                rgba[offset + 1],
                rgba[offset + 2],
                rgba[offset + 3],
            ]
        };
        assert!(pixel(20, 10)[3] > 0 && pixel(20, 10)[0] > 0);
        assert_eq!(pixel(50, 10)[3], 0);
        assert!(pixel(80, 10)[3] > 0 && pixel(80, 10)[0] > 0);
        assert!(pixel(50, 30)[3] > 0 && pixel(50, 30)[2] > 0);
    }

    #[test]
    fn eraser_split_budget_failure_is_atomic() {
        let mut doc = Document::new(1).unwrap();
        let mut segments = Vec::with_capacity(MAX_SEGMENTS_PER_STROKE);
        segments.push(Segment {
            from: Point::new(0, 0),
            to: Point::new(100, 0),
        });
        for index in 1..MAX_SEGMENTS_PER_STROKE {
            let y = index as i32 + 100;
            segments.push(Segment {
                from: Point::new(0, y),
                to: Point::new(0, y),
            });
        }
        doc.strokes.push((
            1,
            Stroke {
                owner: 0,
                color: [255, 0, 0],
                alpha: 255,
                width: 2,
                segments,
            },
        ));
        doc.total_segments = MAX_SEGMENTS_PER_STROKE;
        doc.settings[0] = Settings {
            tool: Tool::Eraser,
            width: 2,
            ..Settings::default()
        };
        let before_revision = doc.revision();
        let before_status = doc.status();
        let before_segments = doc.strokes[0].1.segments.clone();

        assert!(doc.begin(0, (50, 0)).is_err());
        assert_eq!(doc.revision(), before_revision);
        assert_eq!(doc.status(), before_status);
        assert_eq!(doc.strokes[0].1.segments, before_segments);
        assert!(!doc.is_open(0).unwrap());
    }

    #[test]
    fn invalid_configuration_and_export_bounds_do_not_mutate_document() {
        let mut doc = Document::new(1).unwrap();
        doc.configure(0, brush([5, 6, 7])).unwrap();
        let revision = doc.revision();
        assert!(
            doc.configure(
                0,
                Settings {
                    width: 0,
                    ..Settings::default()
                }
            )
            .is_err()
        );
        assert_eq!(doc.revision(), revision);
        assert_eq!(doc.settings(0).unwrap(), &brush([5, 6, 7]));
        assert!(
            doc.render(Bounds {
                left: -1_000_000,
                top: -1_000_000,
                right: 1_000_000,
                bottom: 1_000_000,
            })
            .is_err()
        );
        assert_eq!(doc.revision(), revision);
    }
}
