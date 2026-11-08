//! Pixel-space cursor policy, independent from Windows and the native backend.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MotionCurve {
    #[default]
    Linear,
    Gentle,
    Strong,
}

/// Presentation preference; Auto uses hardware GPU rasterization with a lazy GDI fallback.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CursorRenderer {
    #[default]
    Auto,
    Gdi,
}

impl MotionCurve {
    /// A radial per-packet gain curve. The response rises smoothly and saturates at a preset cap.
    pub fn gain(self, dx: i32, dy: i32) -> f64 {
        let magnitude = f64::from(dx).hypot(f64::from(dy));
        let (threshold, strength, span) = match self {
            Self::Linear => return 1.0,
            Self::Gentle => (3.0, 0.75, 18.0),
            Self::Strong => (2.0, 1.75, 8.0),
        };
        let excess = (magnitude - threshold).max(0.0);
        1.0 + strength * excess / (excess + span)
    }
}

/// Optional packet-count and elapsed-time triggers for combining rapid mouse motion.
/// When both are configured, native publication flushes on whichever trigger fires first.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(default, deny_unknown_fields)]
pub struct MotionCoalescingSettings {
    /// Flush after this many motion packets (`Some(2..=64)`). `None` disables the trigger.
    pub buffer_events: Option<u8>,
    /// Flush after this many milliseconds (`Some(1..=16)`). `None` disables the trigger.
    pub interval_ms: Option<u8>,
}

impl MotionCoalescingSettings {
    pub fn validate(&self) -> Result<()> {
        if let Some(events) = self.buffer_events {
            ensure!(
                (2..=64).contains(&events),
                "motion coalescing buffer must be 2-64 events"
            );
        }
        if let Some(interval_ms) = self.interval_ms {
            ensure!(
                (1..=16).contains(&interval_ms),
                "motion coalescing interval must be 1-16 milliseconds"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Primary,
    Secondary,
    Middle,
    X1,
    X2,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum HorizontalWheelPolicy {
    #[default]
    Ignore,
    ConvertToVertical,
    Preserve,
}

/// Converts vertical wheel input to horizontal only under the selected source-button condition.
/// This is separate from `HorizontalWheelPolicy`, which governs packets already on the H axis.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum HorizontalWheelEmulation {
    #[default]
    Never,
    Always,
    WhilePrimary,
    WhileSecondary,
    WhileMiddle,
    WhileBack,
    WhileForward,
}

impl HorizontalWheelEmulation {
    fn active(self, source_buttons: u8) -> bool {
        match self {
            Self::Never => false,
            Self::Always => true,
            Self::WhilePrimary => source_buttons & 1 != 0,
            Self::WhileSecondary => source_buttons & (1 << 1) != 0,
            Self::WhileMiddle => source_buttons & (1 << 2) != 0,
            Self::WhileBack => source_buttons & (1 << 3) != 0,
            Self::WhileForward => source_buttons & (1 << 4) != 0,
        }
    }
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CursorShape {
    #[default]
    Arrow,
    Crosshair,
    IBeam,
    Ring,
    /// Windows stock cursor pixels with a user-colored glow beneath.
    System,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LabelPosition {
    #[default]
    Right,
    Left,
    Above,
    Below,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum BadgeShape {
    #[default]
    Rounded,
    Box,
    Circle,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum BadgePosition {
    #[default]
    Inline,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum BoundsMode {
    /// Uses the legacy `lock_window` setting for existing profiles.
    #[default]
    Legacy,
    Desktop,
    Window,
    Monitor,
    Region,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct CursorSettings {
    /// Virtual keyboard source and recipient policy for this user.
    pub keyboard: crate::keyboard::KeyboardSettings,
    pub color: [u8; 3],
    pub size: f32,
    pub shape: CursorShape,
    /// Follow the selected application's reported cursor; configured shape is fallback.
    pub follow_application: bool,
    pub visible: bool,
    pub label: bool,
    pub label_position: LabelPosition,
    pub label_font_size: f32,
    pub badge: Option<String>,
    pub badge_shape: BadgeShape,
    pub badge_position: BadgePosition,
    pub idle_hide_ms: Option<u64>,
    pub speed_x: f32,
    pub speed_y: f32,
    /// Original radial gain per input packet: linear, gentle or strong. Not Windows ballistics.
    pub motion_curve: MotionCurve,
    pub renderer: CursorRenderer,
    pub motion_coalescing: MotionCoalescingSettings,
    pub invert_x: bool,
    pub invert_y: bool,
    /// Source order: primary, secondary, middle, back, forward. Null disables a source.
    /// Active destinations must be unique; x1/x2 are back/forward outputs.
    pub button_mapping: [Option<MouseButton>; 5],
    pub swap_buttons: bool,
    pub wheel_scale: f32,
    pub invert_wheel: bool,
    pub horizontal_wheel: HorizontalWheelPolicy,
    pub horizontal_wheel_emulation: HorizontalWheelEmulation,
    pub horizontal_wheel_scale: f32,
    pub invert_horizontal_wheel: bool,
    pub lock_window: bool,
    pub bounds_mode: BoundsMode,
    pub monitor_id: Option<String>,
    pub monitor_work_area: bool,
    pub region: Option<Bounds>,
}
impl Default for CursorSettings {
    fn default() -> Self {
        Self {
            keyboard: crate::keyboard::KeyboardSettings::default(),
            color: [45, 212, 191],
            size: 28.,
            shape: CursorShape::Arrow,
            follow_application: false,
            visible: true,
            label: true,
            label_position: LabelPosition::Right,
            label_font_size: 14.,
            badge: None,
            badge_shape: BadgeShape::Rounded,
            badge_position: BadgePosition::Inline,
            idle_hide_ms: None,
            speed_x: 1.,
            speed_y: 1.,
            motion_curve: MotionCurve::Linear,
            renderer: CursorRenderer::Auto,
            motion_coalescing: MotionCoalescingSettings::default(),
            invert_x: false,
            invert_y: false,
            button_mapping: [
                Some(MouseButton::Primary),
                Some(MouseButton::Secondary),
                Some(MouseButton::Middle),
                Some(MouseButton::X1),
                Some(MouseButton::X2),
            ],
            swap_buttons: false,
            wheel_scale: 1.,
            invert_wheel: false,
            horizontal_wheel: HorizontalWheelPolicy::Ignore,
            horizontal_wheel_emulation: HorizontalWheelEmulation::Never,
            horizontal_wheel_scale: 1.,
            invert_horizontal_wheel: false,
            lock_window: true,
            bounds_mode: BoundsMode::Legacy,
            monitor_id: None,
            monitor_work_area: false,
            region: None,
        }
    }
}
impl CursorSettings {
    /// New virtual users start with focused-control delivery; legacy JSON keeps clicked delivery.
    pub fn for_new_virtual_user() -> Self {
        Self {
            keyboard: crate::keyboard::KeyboardSettings::recommended(),
            ..Self::default()
        }
    }

    pub fn validate(&self) -> Result<()> {
        self.motion_coalescing.validate()?;
        ensure!(
            (12. ..=256.).contains(&self.size),
            "cursor size must be 12–256 pixels"
        );
        ensure!(
            (8. ..=48.).contains(&self.label_font_size),
            "cursor label font size must be 8–48 pixels"
        );
        if let Some(idle_hide_ms) = self.idle_hide_ms {
            ensure!(
                (250..=600_000).contains(&idle_hide_ms),
                "cursor idle-hide timeout must be 250–600000 milliseconds"
            );
        }
        if let Some(badge) = &self.badge {
            ensure!(
                !badge.is_empty()
                    && badge.chars().count() <= 8
                    && badge.chars().all(|character| !character.is_control()),
                "cursor badge must contain 1–8 visible characters without controls"
            );
        }
        ensure!(
            [
                self.speed_x,
                self.speed_y,
                self.wheel_scale,
                self.horizontal_wheel_scale,
            ]
            .iter()
            .all(|v| (0.1..=8.).contains(v)),
            "input multipliers must be finite and between 0.1 and 8"
        );
        let mut outputs = HashSet::new();
        ensure!(
            self.button_mapping
                .iter()
                .flatten()
                .all(|button| outputs.insert(*button)),
            "mapped mouse buttons must have unique outputs"
        );
        if let Some(monitor_id) = &self.monitor_id {
            ensure!(
                !monitor_id.trim().is_empty() && monitor_id.len() <= 1024,
                "monitor identity must contain 1–1024 bytes"
            );
        }
        if let Some(region) = self.region {
            ensure!(
                region.valid_region(),
                "region bounds are invalid or too large"
            );
        }
        match self.bounds_mode {
            BoundsMode::Monitor => ensure!(
                self.monitor_id.is_some(),
                "monitor bounds require a monitor identity"
            ),
            BoundsMode::Region => {
                ensure!(self.region.is_some(), "region bounds require a rectangle")
            }
            _ => (),
        }
        Ok(())
    }

    /// Returns whether the overlay should hide because this virtual user has been idle.
    /// The caller combines this with the saved `visible` preference; an active mouse hold
    /// keeps the overlay visible until release.
    pub fn idle_hidden(&self, elapsed_ms: u64, mouse_held: bool) -> bool {
        !mouse_held
            && self
                .idle_hide_ms
                .is_some_and(|timeout| elapsed_ms >= timeout)
    }

    /// Extended overlay rendering is available only through automatic/concurrent desktop routes.
    pub fn requires_extended_overlay(&self) -> bool {
        self.size > 64.
            || self.renderer != CursorRenderer::Auto
            || self.label_position != LabelPosition::Right
            || self.label_font_size != 14.
            || self.badge_shape != BadgeShape::Rounded
            || self.badge_position != BadgePosition::Inline
            || self.shape == CursorShape::System
            || self.idle_hide_ms.is_some()
    }

    pub fn resolve_bounds(
        &self,
        desktop: Bounds,
        window: Option<Bounds>,
        monitor: Option<Bounds>,
    ) -> Result<Bounds> {
        resolve_bounds(self, desktop, window, monitor)
    }

    /// Applies the configured mouse-button map, then the legacy primary/secondary swap.
    /// `None` means the source is disabled, or the input flags are not button flags.
    pub fn transform_button_flags(&self, flags: u32) -> Option<u32> {
        const SOURCE_FLAGS: [(u32, u32); 5] = [
            (0x001, 0x002),
            (0x004, 0x008),
            (0x010, 0x020),
            (0x040, 0x080),
            (0x100, 0x200),
        ];
        let button_mask = SOURCE_FLAGS
            .iter()
            .fold(0, |mask, (down, up)| mask | down | up);
        if flags == 0 || flags & !button_mask != 0 {
            return None;
        }
        let mut result = 0;
        for (index, (down, up)) in SOURCE_FLAGS.iter().copied().enumerate() {
            let source_bits = flags & (down | up);
            if source_bits == 0 {
                continue;
            }
            let Some(output) = self.button_mapping[index] else {
                continue;
            };
            let (output_down, output_up) = button_flags(output);
            if source_bits & down != 0 {
                result |= output_down;
            }
            if source_bits & up != 0 {
                result |= output_up;
            }
        }
        if self.swap_buttons {
            let left = result & (0x001 | 0x002);
            let right = result & (0x004 | 0x008);
            result &= !(0x001 | 0x002 | 0x004 | 0x008);
            result |= (left << 2) | (right >> 2);
        }
        (result != 0).then_some(result)
    }

    /// Scales a motion packet while carrying fractional output into later packets.
    pub fn transform_motion(&self, dx: i32, dy: i32, remainder: &mut [f64; 2]) -> (i32, i32) {
        let gain = self.motion_curve.gain(dx, dy);
        let mut output = [0; 2];
        for (index, (delta, speed, invert)) in [
            (dx, self.speed_x, self.invert_x),
            (dy, self.speed_y, self.invert_y),
        ]
        .into_iter()
        .enumerate()
        {
            let amount =
                f64::from(delta) * f64::from(speed) * gain * if invert { -1.0 } else { 1.0 }
                    + remainder[index];
            output[index] = amount.trunc() as i32;
            remainder[index] = amount.fract();
        }
        (output[0], output[1])
    }

    /// Applies vertical scaling or the selected horizontal-wheel policy.
    /// The returned packet is `None` when horizontal input is configured to be ignored.
    pub fn transform_wheel_packet(
        &self,
        mut packet: crate::emulated::Packet,
    ) -> Option<crate::emulated::Packet> {
        let (scale, invert) = match packet.flags {
            0x400 => (self.wheel_scale, self.invert_wheel),
            0x800 => match self.horizontal_wheel {
                HorizontalWheelPolicy::Ignore => return None,
                HorizontalWheelPolicy::ConvertToVertical => {
                    packet.flags = 0x400;
                    (self.horizontal_wheel_scale, self.invert_horizontal_wheel)
                }
                HorizontalWheelPolicy::Preserve => {
                    (self.horizontal_wheel_scale, self.invert_horizontal_wheel)
                }
            },
            _ => return Some(packet),
        };
        let delta = f32::from(packet.data as u16 as i16) * scale * if invert { -1.0 } else { 1.0 };
        packet.data = (delta.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16) as u16 as u32;
        Some(packet)
    }

    /// Converts vertical input under the selected held-button condition, then applies the
    /// independent horizontal scale and direction. Existing horizontal packets retain the
    /// `horizontal_wheel` policy handled by `transform_wheel_packet`.
    pub fn transform_wheel_packet_with_source_buttons(
        &self,
        mut packet: crate::emulated::Packet,
        source_buttons: u8,
    ) -> Option<crate::emulated::Packet> {
        if packet.flags == 0x400 && self.horizontal_wheel_emulation.active(source_buttons) {
            packet.flags = 0x800;
            let delta = f32::from(packet.data as u16 as i16)
                * self.horizontal_wheel_scale
                * if self.invert_horizontal_wheel {
                    -1.0
                } else {
                    1.0
                };
            packet.data =
                (delta.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16) as u16 as u32;
            return Some(packet);
        }
        self.transform_wheel_packet(packet)
    }
}

fn button_flags(button: MouseButton) -> (u32, u32) {
    match button {
        MouseButton::Primary => (0x001, 0x002),
        MouseButton::Secondary => (0x004, 0x008),
        MouseButton::Middle => (0x010, 0x020),
        MouseButton::X1 => (0x040, 0x080),
        MouseButton::X2 => (0x100, 0x200),
    }
}
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct Bounds {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}
impl Bounds {
    pub fn contains(self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
    pub fn valid(self) -> bool {
        self.right > self.left && self.bottom > self.top
    }

    pub fn intersection(self, other: Self) -> Option<Self> {
        let intersection = Self {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        };
        intersection.valid().then_some(intersection)
    }

    pub fn valid_region(self) -> bool {
        const LIMIT: i32 = 1_000_000;
        (-LIMIT..=LIMIT).contains(&self.left)
            && (-LIMIT..=LIMIT).contains(&self.top)
            && (-LIMIT..=LIMIT).contains(&self.right)
            && (-LIMIT..=LIMIT).contains(&self.bottom)
            && i64::from(self.right) - i64::from(self.left) > 0
            && i64::from(self.bottom) - i64::from(self.top) > 0
    }
}

/// Resolves portable cursor confinement against physical desktop-coordinate bounds.
/// A window lock without a focused window falls back to the desktop until the first click.
pub fn resolve_bounds(
    settings: &CursorSettings,
    desktop: Bounds,
    window: Option<Bounds>,
    monitor: Option<Bounds>,
) -> Result<Bounds> {
    ensure!(desktop.valid(), "desktop bounds are invalid");
    let resolved = match settings.bounds_mode {
        BoundsMode::Legacy => {
            if settings.lock_window {
                match window {
                    Some(bounds) => {
                        ensure!(bounds.valid(), "focused window bounds are invalid");
                        bounds
                    }
                    None => desktop,
                }
            } else {
                desktop
            }
        }
        BoundsMode::Desktop => desktop,
        BoundsMode::Window => match window {
            Some(bounds) => {
                ensure!(bounds.valid(), "focused window bounds are invalid");
                bounds
            }
            None => desktop,
        },
        BoundsMode::Monitor => {
            ensure!(
                settings
                    .monitor_id
                    .as_ref()
                    .is_some_and(|id| { !id.trim().is_empty() && id.len() <= 1024 }),
                "monitor identity is missing or invalid"
            );
            let bounds = monitor.context("selected monitor is unavailable")?;
            ensure!(bounds.valid(), "selected monitor bounds are invalid");
            bounds
        }
        BoundsMode::Region => {
            let region = settings.region.context("selected region is missing")?;
            ensure!(region.valid_region(), "selected region bounds are invalid");
            let clip = if settings.monitor_id.is_some() {
                let monitor = monitor.context("selected monitor is unavailable")?;
                ensure!(monitor.valid(), "selected monitor bounds are invalid");
                monitor
            } else {
                desktop
            };
            region
                .intersection(clip)
                .and_then(|bounds| bounds.intersection(desktop))
                .context("selected region does not overlap its display bounds")?
        }
    };
    ensure!(resolved.valid(), "resolved cursor bounds are invalid");
    Ok(resolved)
}
#[derive(Debug, Clone)]
pub struct Cursor {
    pub x: i32,
    pub y: i32,
    remainder: [f64; 2],
}
impl Cursor {
    pub fn new(x: i32, y: i32) -> Self {
        Self {
            x,
            y,
            remainder: [0.; 2],
        }
    }
    pub fn motion(
        &mut self,
        dx: i32,
        dy: i32,
        settings: &CursorSettings,
        bounds: Bounds,
    ) -> (i32, i32) {
        let old = (self.x, self.y);
        let transformed = settings.transform_motion(dx, dy, &mut self.remainder);
        self.x = self
            .x
            .saturating_add(transformed.0)
            .clamp(bounds.left, bounds.right - 1);
        self.y = self
            .y
            .saturating_add(transformed.1)
            .clamp(bounds.top, bounds.bottom - 1);
        if self.x == bounds.left || self.x == bounds.right - 1 {
            self.remainder[0] = 0.;
        }
        if self.y == bounds.top || self.y == bounds.bottom - 1 {
            self.remainder[1] = 0.;
        }
        (self.x - old.0, self.y - old.1)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_migration_and_profile_round_trip_preserve_other_cursor_policy() {
        let old = r#"{"size":32.0,"speed_x":1.5,"badge":"A"}"#;
        let legacy: CursorSettings = serde_json::from_str(old).unwrap();
        assert_eq!(legacy.renderer, CursorRenderer::Auto);
        assert_eq!((legacy.size, legacy.speed_x), (32., 1.5));
        assert_eq!(legacy.badge.as_deref(), Some("A"));
        let mut selected = legacy.clone();
        selected.renderer = CursorRenderer::Gdi;
        selected.validate().unwrap();
        assert!(selected.requires_extended_overlay());
        let exported = serde_json::to_value(&selected).unwrap();
        assert_eq!(exported["renderer"], "gdi");
        let imported: CursorSettings = serde_json::from_value(exported).unwrap();
        assert_eq!(
            serde_json::to_value(imported).unwrap(),
            serde_json::to_value(selected).unwrap()
        );
        assert!(
            serde_json::from_str::<CursorSettings>(r#"{"renderer":"unavailable_gpu"}"#).is_err()
        );
    }

    #[test]
    fn independent_subpixel_motion_negative_origin_and_bounds() {
        let b = Bounds {
            left: -1920,
            top: -100,
            right: 1920,
            bottom: 1080,
        };
        let s = CursorSettings {
            speed_x: 0.5,
            ..Default::default()
        };
        let mut a = Cursor::new(-100, 20);
        let b_cursor = Cursor::new(300, 20);
        assert_eq!(a.motion(1, 0, &s, b), (0, 0));
        assert_eq!(a.motion(1, 0, &s, b), (1, 0));
        a.motion(-32000, 32000, &s, b);
        assert_eq!((a.x, a.y), (-1920, 1079));
        assert_eq!((b_cursor.x, b_cursor.y), (300, 20));
        assert_eq!(a.motion(2, -1, &s, b), (1, -1));
    }
    #[test]
    fn reject_invalid_settings() {
        assert!(
            CursorSettings {
                speed_x: f32::NAN,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            CursorSettings {
                size: 0.,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn motion_coalescing_defaults_are_backward_compatible_and_ranges_are_bounded() {
        let defaults = CursorSettings::default();
        assert_eq!(
            defaults.motion_coalescing,
            MotionCoalescingSettings::default()
        );
        assert_eq!(defaults.motion_coalescing.buffer_events, None);
        assert_eq!(defaults.motion_coalescing.interval_ms, None);

        let old: CursorSettings = serde_json::from_str(
            r#"{"color":[45,212,191],"size":28.0,"visible":true,"label":true,"speed_x":1.0,"speed_y":1.0,"invert_x":false,"invert_y":false,"swap_buttons":false,"wheel_scale":1.0,"invert_wheel":false,"lock_window":true}"#,
        )
        .unwrap();
        assert_eq!(old.motion_coalescing, MotionCoalescingSettings::default());
        assert!(old.validate().is_ok());

        for settings in [
            MotionCoalescingSettings {
                buffer_events: Some(2),
                interval_ms: Some(1),
            },
            MotionCoalescingSettings {
                buffer_events: Some(64),
                interval_ms: Some(16),
            },
            MotionCoalescingSettings {
                buffer_events: Some(8),
                interval_ms: None,
            },
            MotionCoalescingSettings {
                buffer_events: None,
                interval_ms: Some(4),
            },
        ] {
            assert!(settings.validate().is_ok());
        }
        for settings in [
            MotionCoalescingSettings {
                buffer_events: Some(1),
                interval_ms: None,
            },
            MotionCoalescingSettings {
                buffer_events: Some(65),
                interval_ms: None,
            },
            MotionCoalescingSettings {
                buffer_events: None,
                interval_ms: Some(0),
            },
            MotionCoalescingSettings {
                buffer_events: None,
                interval_ms: Some(17),
            },
        ] {
            assert!(settings.validate().is_err());
        }
    }

    #[test]
    fn radial_curves_preserve_small_motion_and_bound_large_gain() {
        assert_eq!(MotionCurve::Linear.gain(100, -50), 1.0);
        assert_eq!(MotionCurve::Gentle.gain(1, 1), 1.0);
        assert_eq!(MotionCurve::Strong.gain(0, 0), 1.0);
        let gentle = MotionCurve::Gentle.gain(30_000, 30_000);
        let strong = MotionCurve::Strong.gain(30_000, 30_000);
        assert!(gentle > 1.0 && gentle < 1.75);
        assert!(strong > gentle && strong < 2.75);

        let mut remainder = [0.0; 2];
        let settings = CursorSettings {
            speed_x: 0.5,
            ..Default::default()
        };
        assert_eq!(settings.transform_motion(1, 0, &mut remainder), (0, 0));
        assert_eq!(settings.transform_motion(1, 0, &mut remainder), (1, 0));
    }

    #[test]
    fn mapped_button_flags_preserve_edges_disable_sources_and_apply_legacy_swap() {
        let defaults = CursorSettings::default();
        assert_eq!(defaults.transform_button_flags(0x001), Some(0x001));
        assert_eq!(defaults.transform_button_flags(0x002), Some(0x002));
        assert_eq!(defaults.transform_button_flags(0x040), Some(0x040));

        let mapped = CursorSettings {
            button_mapping: [
                Some(MouseButton::X1),
                None,
                Some(MouseButton::Middle),
                Some(MouseButton::Secondary),
                Some(MouseButton::Primary),
            ],
            ..Default::default()
        };
        assert!(mapped.validate().is_ok());
        assert_eq!(mapped.transform_button_flags(0x002), Some(0x080));
        assert_eq!(mapped.transform_button_flags(0x004), None);
        assert_eq!(mapped.transform_button_flags(0x008), None);

        let swapped = CursorSettings {
            swap_buttons: true,
            ..Default::default()
        };
        assert_eq!(swapped.transform_button_flags(0x001), Some(0x004));
        assert_eq!(swapped.transform_button_flags(0x008), Some(0x002));
        assert!(
            CursorSettings {
                button_mapping: [
                    Some(MouseButton::Primary),
                    Some(MouseButton::Primary),
                    None,
                    None,
                    None,
                ],
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn horizontal_wheel_policy_converts_signed_data_with_independent_scale() {
        use crate::emulated::Packet;

        let ignored = CursorSettings::default()
            .transform_wheel_packet(Packet::horizontal_wheel(0xF1000002, -120));
        assert!(ignored.is_none());

        let settings = CursorSettings {
            horizontal_wheel: HorizontalWheelPolicy::ConvertToVertical,
            horizontal_wheel_scale: 0.5,
            invert_horizontal_wheel: true,
            ..Default::default()
        };
        let converted = settings
            .transform_wheel_packet(Packet::horizontal_wheel(0xF1000002, -120))
            .unwrap();
        assert_eq!(converted.flags, 0x400);
        assert_eq!(converted.data, 60);
        let vertical = settings
            .transform_wheel_packet(Packet::wheel(0xF1000002, -120))
            .unwrap();
        assert_eq!(vertical.flags, 0x400);
        assert_eq!(vertical.data, (-120i16) as u16 as u32);

        let preserved = CursorSettings {
            horizontal_wheel: HorizontalWheelPolicy::Preserve,
            horizontal_wheel_scale: 0.5,
            invert_horizontal_wheel: true,
            ..Default::default()
        }
        .transform_wheel_packet(Packet::horizontal_wheel(0xF1000002, -120))
        .unwrap();
        assert_eq!(preserved.flags, 0x800);
        assert_eq!(preserved.data, 60);
    }

    #[test]
    fn conditional_horizontal_emulation_uses_raw_button_mask_and_scales_once() {
        use crate::emulated::Packet;

        let settings = CursorSettings {
            horizontal_wheel_emulation: HorizontalWheelEmulation::WhileBack,
            horizontal_wheel: HorizontalWheelPolicy::Ignore,
            horizontal_wheel_scale: 0.5,
            invert_horizontal_wheel: true,
            ..Default::default()
        };
        let packet = Packet::wheel(0xF1000002, -120);
        let not_held = settings
            .transform_wheel_packet_with_source_buttons(packet, 1 << 4)
            .unwrap();
        assert_eq!(not_held.flags, 0x400);
        assert_eq!(not_held.data, (-120i16) as u16 as u32);

        let held = settings
            .transform_wheel_packet_with_source_buttons(packet, 1 << 3)
            .unwrap();
        assert_eq!(held.flags, 0x800);
        assert_eq!(held.data, 60);

        let already_horizontal = settings.transform_wheel_packet_with_source_buttons(
            Packet::horizontal_wheel(0xF1000002, -120),
            1 << 3,
        );
        assert!(already_horizontal.is_none());

        let always = CursorSettings {
            horizontal_wheel_emulation: HorizontalWheelEmulation::Always,
            ..settings
        }
        .transform_wheel_packet_with_source_buttons(packet, 0)
        .unwrap();
        assert_eq!(always.flags, 0x800);
        assert_eq!(always.data, 60);
    }

    #[test]
    fn bounds_resolve_legacy_window_monitor_and_intersecting_region() {
        let desktop = Bounds {
            left: -1920,
            top: -200,
            right: 1920,
            bottom: 1080,
        };
        let window = Bounds {
            left: -400,
            top: 40,
            right: 800,
            bottom: 900,
        };
        let monitor = Bounds {
            left: -1920,
            top: -200,
            right: 0,
            bottom: 880,
        };
        let defaults = CursorSettings::default();
        assert_eq!(
            defaults
                .resolve_bounds(desktop, Some(window), None)
                .unwrap(),
            window
        );
        assert_eq!(
            defaults.resolve_bounds(desktop, None, None).unwrap(),
            desktop
        );
        assert_eq!(
            CursorSettings {
                bounds_mode: BoundsMode::Window,
                ..defaults.clone()
            }
            .resolve_bounds(desktop, None, None)
            .unwrap(),
            desktop
        );

        let monitor_settings = CursorSettings {
            bounds_mode: BoundsMode::Monitor,
            monitor_id: Some("DISPLAY\\MONITOR-A".into()),
            ..defaults.clone()
        };
        assert_eq!(
            monitor_settings
                .resolve_bounds(desktop, None, Some(monitor))
                .unwrap(),
            monitor
        );
        assert!(
            monitor_settings
                .resolve_bounds(desktop, None, None)
                .is_err()
        );

        let region_settings = CursorSettings {
            bounds_mode: BoundsMode::Region,
            monitor_id: Some("DISPLAY\\MONITOR-A".into()),
            region: Some(Bounds {
                left: -2200,
                top: 100,
                right: -1000,
                bottom: 1000,
            }),
            ..defaults
        };
        assert_eq!(
            region_settings
                .resolve_bounds(desktop, Some(window), Some(monitor))
                .unwrap(),
            Bounds {
                left: -1920,
                top: 100,
                right: -1000,
                bottom: 880,
            }
        );
    }

    #[test]
    fn old_cursor_settings_default_new_appearance_and_bounds_fields() {
        let old: CursorSettings = serde_json::from_str(
            r#"{"color":[45,212,191],"size":28.0,"visible":true,"label":true,"speed_x":1.0,"speed_y":1.0,"invert_x":false,"invert_y":false,"swap_buttons":false,"wheel_scale":1.0,"invert_wheel":false,"lock_window":true}"#,
        )
        .unwrap();
        assert_eq!(old.shape, CursorShape::Arrow);
        assert_eq!(old.badge, None);
        assert_eq!(old.bounds_mode, BoundsMode::Legacy);
        assert!(!old.monitor_work_area);
        assert_eq!(old.label_position, LabelPosition::Right);
        assert_eq!(old.label_font_size, 14.0);
        assert_eq!(old.badge_shape, BadgeShape::Rounded);
        assert_eq!(old.badge_position, BadgePosition::Inline);
        assert_eq!(old.idle_hide_ms, None);
        assert!(!old.requires_extended_overlay());
        assert!(old.validate().is_ok());

        assert!(
            CursorSettings {
                badge: Some("too many!".into()),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            CursorSettings {
                bounds_mode: BoundsMode::Monitor,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            CursorSettings {
                bounds_mode: BoundsMode::Region,
                monitor_id: Some("DISPLAY\\MONITOR-A".into()),
                region: Some(Bounds {
                    left: 1_000_001,
                    top: 0,
                    right: 1_000_002,
                    bottom: 1,
                }),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            !Bounds {
                left: i32::MIN,
                top: 0,
                right: i32::MAX,
                bottom: 10,
            }
            .valid_region()
        );
    }

    #[test]
    fn extended_overlay_bounds_and_idle_timeout_are_exact() {
        let large = CursorSettings {
            size: 256.,
            ..Default::default()
        };
        assert!(large.validate().is_ok());
        assert!(large.requires_extended_overlay());
        assert!(
            CursorSettings {
                size: 256.1,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            CursorSettings {
                label_font_size: 7.9,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            CursorSettings {
                idle_hide_ms: Some(249),
                ..Default::default()
            }
            .validate()
            .is_err()
        );

        let idle = CursorSettings {
            idle_hide_ms: Some(1_000),
            ..Default::default()
        };
        assert!(!idle.idle_hidden(999, false));
        assert!(!idle.idle_hidden(1_000, true));
        assert!(idle.idle_hidden(1_000, false));
        assert!(idle.requires_extended_overlay());
        assert!(!CursorSettings::default().idle_hidden(u64::MAX, false));
    }
}
