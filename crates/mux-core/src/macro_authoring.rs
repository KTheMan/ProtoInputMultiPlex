//! Portable timeline editing and deterministic path generation for authored macros.
use crate::{
    DeviceKind,
    automation::{
        Action, Clip, MAX_CLIP_DURATION_MICROS, MAX_CLIP_EVENTS, MAX_USER_ID, TimedAction,
    },
    emulated::{self, Packet},
};
use anyhow::{Result, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const MAX_COORDINATE: i32 = 1_000_000;
const MAX_RADIUS: u32 = 1_000_000;

/// Timeline edits are applied atomically and retain the standard Clip format.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Edit {
    Insert {
        event: TimedAction,
    },
    Replace {
        index: usize,
        event: TimedAction,
    },
    Delete {
        start: usize,
        end: usize,
    },
    Duplicate {
        start: usize,
        end: usize,
        offset_micros: u64,
    },
    Retime {
        index: usize,
        at_micros: u64,
    },
    Shift {
        start: usize,
        end: usize,
        delta_micros: i64,
    },
    Remap {
        from: usize,
        to: usize,
    },
    Clear,
}

/// Applies one edit. `None` represents an empty authoring timeline.
pub fn apply(clip: Option<&Clip>, edit: &Edit) -> Result<Option<Clip>> {
    if matches!(edit, Edit::Clear) {
        return Ok(None);
    }
    if let Some(clip) = clip {
        clip.validate()?;
    }
    let mut events = clip.map_or_else(Vec::new, |clip| clip.events.clone());

    match edit {
        Edit::Clear => unreachable!(),
        Edit::Insert { event } => events.push(clone_event(event)),
        Edit::Replace { index, event } => {
            ensure!(*index < events.len(), "replace index is out of range");
            events[*index] = clone_event(event);
        }
        Edit::Delete { start, end } => {
            validate_range(*start, *end, events.len())?;
            events.drain(*start..*end);
        }
        Edit::Duplicate {
            start,
            end,
            offset_micros,
        } => {
            validate_range(*start, *end, events.len())?;
            ensure!(start < end, "duplicate range is empty");
            ensure!(
                events.len().saturating_add(end - start) <= MAX_CLIP_EVENTS,
                "clip has too many events"
            );
            let mut duplicates = Vec::with_capacity(end - start);
            for event in &events[*start..*end] {
                let at_micros = event
                    .at_micros
                    .checked_add(*offset_micros)
                    .ok_or_else(|| anyhow::anyhow!("duplicate timestamp overflow"))?;
                duplicates.push(TimedAction {
                    at_micros,
                    action: event.action.clone(),
                });
            }
            events.extend(duplicates);
        }
        Edit::Retime { index, at_micros } => {
            ensure!(*index < events.len(), "retime index is out of range");
            events[*index].at_micros = *at_micros;
        }
        Edit::Shift {
            start,
            end,
            delta_micros,
        } => {
            validate_range(*start, *end, events.len())?;
            ensure!(start < end, "shift range is empty");
            for event in &mut events[*start..*end] {
                event.at_micros = if *delta_micros >= 0 {
                    event
                        .at_micros
                        .checked_add(*delta_micros as u64)
                        .ok_or_else(|| anyhow::anyhow!("shift timestamp overflow"))?
                } else {
                    event
                        .at_micros
                        .checked_sub(delta_micros.unsigned_abs())
                        .ok_or_else(|| anyhow::anyhow!("shift timestamp underflow"))?
                };
            }
        }
        Edit::Remap { from, to } => {
            ensure!(*from <= MAX_USER_ID, "source user ID is out of range");
            ensure!(*to <= MAX_USER_ID, "target user ID is out of range");
            for event in &mut events {
                if event.action.user() == *from {
                    event.action = event.action.with_user(*to);
                }
            }
        }
    }

    // Stable sorting preserves the authored order of actions at the same time.
    events.sort_by_key(|event| event.at_micros);
    if events.is_empty() {
        return Ok(None);
    }
    Ok(Some(Clip::new(events)?))
}

/// Returns a copy of a clip with every action assigned to `user`.
pub fn remap(clip: &Clip, user: usize) -> Result<Clip> {
    ensure!(user <= MAX_USER_ID, "user ID is out of range");
    clip.validate()?;
    Clip::new(
        clip.events
            .iter()
            .map(|event| TimedAction {
                at_micros: event.at_micros,
                action: event.action.with_user(user),
            })
            .collect(),
    )
}

fn clone_event(event: &TimedAction) -> TimedAction {
    TimedAction {
        at_micros: event.at_micros,
        action: event.action.clone(),
    }
}

fn validate_range(start: usize, end: usize, len: usize) -> Result<()> {
    ensure!(
        start <= end && end <= len,
        "timeline range is out of bounds"
    );
    Ok(())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Pattern {
    #[default]
    Circle,
    Square,
    Triangle,
    Line,
    FigureEight,
    Jiggle,
}

/// Parameters for a bounded deterministic path generator.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Generator {
    pub shape: Pattern,
    pub user: usize,
    pub center: (i32, i32),
    pub duration_micros: u64,
    pub radius: u32,
    /// Total path intervals, except Jiggle where this is the sample count.
    pub points: u32,
    /// Optional emulated mouse button index: 0 primary, 1 secondary, 2 middle, 3/4 back/forward.
    pub click: Option<u8>,
    pub seed: u64,
}

impl Default for Generator {
    fn default() -> Self {
        Self {
            shape: Pattern::Circle,
            user: 0,
            center: (0, 0),
            duration_micros: 1_000_000,
            radius: 100,
            points: 100,
            click: None,
            seed: 0,
        }
    }
}

/// Generates a standard, validated clip containing only moves and paired mouse packets.
pub fn generate(generator: &Generator) -> Result<Clip> {
    validate_generator(generator)?;
    let point_count = match generator.shape {
        Pattern::Jiggle => generator.points as usize,
        _ => generator.points as usize + 1,
    };
    let actions_per_point = if generator.click.is_some() { 3 } else { 1 };
    let event_count = point_count
        .checked_mul(actions_per_point)
        .ok_or_else(|| anyhow::anyhow!("generated event count overflow"))?;
    ensure!(event_count <= MAX_CLIP_EVENTS, "clip has too many events");

    let device = if generator.click.is_some() {
        Some(emulated::device(DeviceKind::Mouse, 1)?.handle as u32)
    } else {
        None
    };
    let mut events = Vec::with_capacity(event_count);
    let mut random = SplitMix64(generator.seed);
    for index in 0..point_count {
        let point = generated_point(generator, index, &mut random)?;
        ensure!(
            point.0.unsigned_abs() <= MAX_COORDINATE as u32
                && point.1.unsigned_abs() <= MAX_COORDINATE as u32,
            "generated coordinate exceeds the virtual coordinate bound"
        );
        let at_micros = match generator.shape {
            Pattern::Jiggle if point_count == 1 => 0,
            Pattern::Jiggle => scaled_time(generator.duration_micros, index, point_count - 1),
            _ => scaled_time(generator.duration_micros, index, generator.points as usize),
        };
        events.push(TimedAction {
            at_micros,
            action: Action::MoveTo {
                user: generator.user,
                x: point.0,
                y: point.1,
            },
        });
        if let (Some(button), Some(device)) = (generator.click, device) {
            events.push(TimedAction {
                at_micros,
                action: Action::Packet {
                    user: generator.user,
                    packet: Packet::button(device, u32::from(button), false),
                },
            });
            events.push(TimedAction {
                at_micros,
                action: Action::Packet {
                    user: generator.user,
                    packet: Packet::button(device, u32::from(button), true),
                },
            });
        }
    }
    Clip::new(events)
}

fn validate_generator(generator: &Generator) -> Result<()> {
    ensure!(generator.user <= MAX_USER_ID, "user ID is out of range");
    ensure!(
        generator.center.0.unsigned_abs() <= MAX_COORDINATE as u32
            && generator.center.1.unsigned_abs() <= MAX_COORDINATE as u32,
        "generator center exceeds the virtual coordinate bound"
    );
    ensure!(
        generator.duration_micros > 0 && generator.duration_micros <= MAX_CLIP_DURATION_MICROS,
        "generator duration is out of range"
    );
    ensure!(
        (1..=MAX_RADIUS).contains(&generator.radius),
        "generator radius is out of range"
    );
    ensure!(
        generator.click.is_none_or(|button| button <= 4),
        "click button must be between 0 and 4"
    );
    let points = generator.points;
    let valid_points = match generator.shape {
        Pattern::Circle => points >= 3,
        Pattern::Square => points >= 4,
        Pattern::Triangle => points >= 3,
        Pattern::Line => points >= 2,
        Pattern::FigureEight => points >= 4,
        Pattern::Jiggle => points >= 1,
    };
    ensure!(
        valid_points,
        "point count does not fit the selected path shape"
    );
    ensure!(
        generator.shape != Pattern::Jiggle || generator.click.is_none(),
        "jiggle does not support click actions"
    );
    let count = if generator.shape == Pattern::Jiggle {
        points as usize
    } else {
        points as usize + 1
    };
    let actions_per_point = if generator.click.is_some() { 3 } else { 1 };
    ensure!(
        count.saturating_mul(actions_per_point) <= MAX_CLIP_EVENTS,
        "clip has too many events"
    );
    Ok(())
}

fn scaled_time(duration: u64, index: usize, denominator: usize) -> u64 {
    ((u128::from(duration) * index as u128) / denominator as u128) as u64
}

fn generated_point(
    generator: &Generator,
    index: usize,
    random: &mut SplitMix64,
) -> Result<(i32, i32)> {
    let points = generator.points as usize;
    let radius = f64::from(generator.radius);
    let (x, y) = match generator.shape {
        Pattern::Circle => {
            let angle = std::f64::consts::TAU * index as f64 / points as f64;
            (
                f64::from(generator.center.0) + radius * angle.cos(),
                f64::from(generator.center.1) + radius * angle.sin(),
            )
        }
        Pattern::Square => polygon_point(
            generator.center,
            points,
            index,
            &[
                (-radius, -radius),
                (radius, -radius),
                (radius, radius),
                (-radius, radius),
            ],
        ),
        Pattern::Triangle => {
            let half_width = radius * 0.866_025_403_784_438_6;
            polygon_point(
                generator.center,
                points,
                index,
                &[
                    (0.0, -radius),
                    (half_width, radius * 0.5),
                    (-half_width, radius * 0.5),
                ],
            )
        }
        Pattern::Line => {
            let forward = points.div_ceil(2);
            let backward = points - forward;
            let horizontal = if index <= forward {
                -radius + 2.0 * radius * index as f64 / forward as f64
            } else {
                radius - 2.0 * radius * (index - forward) as f64 / backward as f64
            };
            (
                f64::from(generator.center.0) + horizontal,
                f64::from(generator.center.1),
            )
        }
        Pattern::FigureEight => {
            let angle = std::f64::consts::TAU * index as f64 / points as f64;
            (
                f64::from(generator.center.0) + radius * angle.sin(),
                f64::from(generator.center.1) + radius * 0.5 * (2.0 * angle).sin(),
            )
        }
        Pattern::Jiggle => {
            let radius_i64 = i64::from(generator.radius);
            (
                f64::from(generator.center.0) + random.offset(radius_i64) as f64,
                f64::from(generator.center.1) + random.offset(radius_i64) as f64,
            )
        }
    };
    ensure!(
        x.is_finite() && y.is_finite(),
        "generated coordinate is not finite"
    );
    let x = x.round();
    let y = y.round();
    ensure!(
        x >= i32::MIN as f64
            && x <= i32::MAX as f64
            && y >= i32::MIN as f64
            && y <= i32::MAX as f64,
        "generated coordinate overflows"
    );
    Ok((x as i32, y as i32))
}

fn polygon_point(
    center: (i32, i32),
    intervals: usize,
    index: usize,
    vertices: &[(f64, f64)],
) -> (f64, f64) {
    let edges = vertices.len();
    if index == intervals {
        return (
            f64::from(center.0) + vertices[0].0,
            f64::from(center.1) + vertices[0].1,
        );
    }
    let base = intervals / edges;
    let remainder = intervals % edges;
    let mut edge_start = 0;
    for edge in 0..edges {
        let segments = base + usize::from(edge < remainder);
        if index < edge_start + segments {
            let step = (index - edge_start) as f64 / segments as f64;
            let from = vertices[edge];
            let to = vertices[(edge + 1) % edges];
            return (
                f64::from(center.0) + from.0 + (to.0 - from.0) * step,
                f64::from(center.1) + from.1 + (to.1 - from.1) * step,
            );
        }
        edge_start += segments;
    }
    unreachable!("polygon samples cover every interval")
}

struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    fn offset(&mut self, radius: i64) -> i64 {
        let width = (radius as u128) * 2 + 1;
        let scaled = (u128::from(self.next()) * width) >> 64;
        scaled as i64 - radius
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn move_clip(times_and_x: &[(u64, i32)]) -> Clip {
        Clip::new(
            times_and_x
                .iter()
                .map(|(at_micros, x)| TimedAction {
                    at_micros: *at_micros,
                    action: Action::MoveTo {
                        user: 0,
                        x: *x,
                        y: 0,
                    },
                })
                .collect(),
        )
        .unwrap()
    }

    fn move_x(clip: &Clip) -> Vec<i32> {
        clip.events
            .iter()
            .filter_map(|event| match &event.action {
                Action::MoveTo { x, .. } => Some(*x),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn edits_keep_stable_equal_time_order_and_are_atomic() {
        let original = move_clip(&[(0, 1), (10, 2), (10, 3)]);
        let retimed = apply(
            Some(&original),
            &Edit::Retime {
                index: 0,
                at_micros: 10,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(move_x(&retimed), vec![1, 2, 3]);

        let with_insert = apply(
            Some(&retimed),
            &Edit::Insert {
                event: TimedAction {
                    at_micros: 10,
                    action: Action::MoveTo {
                        user: 0,
                        x: 4,
                        y: 0,
                    },
                },
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(move_x(&with_insert), vec![1, 2, 3, 4]);

        let failed = apply(
            Some(&original),
            &Edit::Shift {
                start: 0,
                end: 1,
                delta_micros: -1,
            },
        );
        assert!(failed.is_err());
        assert_eq!(move_x(&original), vec![1, 2, 3]);
        assert!(
            apply(Some(&original), &Edit::Delete { start: 0, end: 3 })
                .unwrap()
                .is_none()
        );

        let duplicated = apply(
            Some(&original),
            &Edit::Duplicate {
                start: 1,
                end: 3,
                offset_micros: 5,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            duplicated
                .events
                .iter()
                .map(|event| event.at_micros)
                .collect::<Vec<_>>(),
            vec![0, 10, 10, 15, 15]
        );
        let shifted = apply(
            Some(&duplicated),
            &Edit::Shift {
                start: 3,
                end: 5,
                delta_micros: 5,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            shifted
                .events
                .iter()
                .map(|event| event.at_micros)
                .collect::<Vec<_>>(),
            vec![0, 10, 10, 20, 20]
        );
    }

    #[test]
    fn polygon_generators_retain_corners_with_arbitrary_interval_counts() {
        let square = generate(&Generator {
            shape: Pattern::Square,
            center: (50, 60),
            radius: 10,
            points: 5,
            ..Generator::default()
        })
        .unwrap();
        assert_eq!(move_x(&square).len(), 6);
        let points: Vec<_> = square
            .events
            .iter()
            .filter_map(|event| match &event.action {
                Action::MoveTo { x, y, .. } => Some((*x, *y)),
                _ => None,
            })
            .collect();
        assert_eq!(
            points,
            vec![(40, 50), (50, 50), (60, 50), (60, 70), (40, 70), (40, 50)]
        );

        let triangle = generate(&Generator {
            shape: Pattern::Triangle,
            points: 5,
            ..Generator::default()
        })
        .unwrap();
        let triangle_points: Vec<_> = triangle
            .events
            .iter()
            .filter_map(|event| match &event.action {
                Action::MoveTo { x, y, .. } => Some((*x, *y)),
                _ => None,
            })
            .collect();
        assert_eq!(triangle_points.len(), 6);
        assert_eq!(triangle_points[0], triangle_points[5]);
        assert!(triangle_points[2] != triangle_points[1]);
    }

    #[test]
    fn circle_and_figure_eight_have_exact_closed_landmarks() {
        let circle = generate(&Generator {
            center: (20, -10),
            radius: 5,
            points: 4,
            duration_micros: 100,
            ..Generator::default()
        })
        .unwrap();
        let circle_points: Vec<_> = circle
            .events
            .iter()
            .filter_map(|event| match &event.action {
                Action::MoveTo { x, y, .. } => Some((*x, *y)),
                _ => None,
            })
            .collect();
        assert_eq!(
            circle_points,
            vec![(25, -10), (20, -5), (15, -10), (20, -15), (25, -10)]
        );
        assert_eq!(circle.events.last().unwrap().at_micros, 100);

        let figure_eight = generate(&Generator {
            shape: Pattern::FigureEight,
            points: 8,
            ..Generator::default()
        })
        .unwrap();
        let points: Vec<_> = figure_eight
            .events
            .iter()
            .filter_map(|event| match &event.action {
                Action::MoveTo { x, y, .. } => Some((*x, *y)),
                _ => None,
            })
            .collect();
        assert_eq!(points[0], points[8]);
        assert_eq!(points[0], points[4]);
    }

    #[test]
    fn click_generators_pair_each_of_the_five_buttons_at_every_point() {
        for button in 0..=4 {
            let clip = generate(&Generator {
                shape: Pattern::Line,
                points: 2,
                click: Some(button),
                ..Generator::default()
            })
            .unwrap();
            assert_eq!(clip.events.len(), 9);
            for group in clip.events.as_chunks::<3>().0 {
                assert!(matches!(&group[0].action, Action::MoveTo { .. }));
                let Action::Packet { packet: down, .. } = &group[1].action else {
                    panic!("missing click down packet");
                };
                let Action::Packet { packet: up, .. } = &group[2].action else {
                    panic!("missing click up packet");
                };
                assert_eq!(down.flags, 1 << (u32::from(button) * 2));
                assert_eq!(up.flags, 1 << (u32::from(button) * 2 + 1));
                down.validate().unwrap();
                up.validate().unwrap();
            }
        }
    }

    #[test]
    fn seeded_jiggle_is_repeatable_and_bounded() {
        let generator = Generator {
            shape: Pattern::Jiggle,
            center: (-20, 15),
            radius: 8,
            points: 30,
            seed: 77,
            ..Generator::default()
        };
        let first = generate(&generator).unwrap();
        let second = generate(&generator).unwrap();
        assert_eq!(move_x(&first), move_x(&second));
        let other = generate(&Generator {
            seed: 78,
            ..generator.clone()
        })
        .unwrap();
        assert_ne!(move_x(&first), move_x(&other));
        for event in &first.events {
            let Action::MoveTo { x, y, .. } = &event.action else {
                panic!("jiggle emitted a non-movement action");
            };
            assert!((*x + 20).abs() <= 8 && (*y - 15).abs() <= 8);
        }
    }

    #[test]
    fn generators_reject_bounds_click_mismatch_and_event_overflow() {
        assert!(
            generate(&Generator {
                center: (MAX_COORDINATE, 0),
                radius: 1,
                ..Generator::default()
            })
            .is_err()
        );
        assert!(
            generate(&Generator {
                shape: Pattern::Jiggle,
                click: Some(0),
                ..Generator::default()
            })
            .is_err()
        );
        assert!(
            generate(&Generator {
                shape: Pattern::Circle,
                points: 33_334,
                click: Some(0),
                ..Generator::default()
            })
            .is_err()
        );
    }

    #[test]
    fn remap_changes_owners_without_changing_packet_identity_or_time() {
        let clip = generate(&Generator {
            shape: Pattern::Line,
            points: 2,
            click: Some(4),
            ..Generator::default()
        })
        .unwrap();
        let remapped = remap(&clip, 42).unwrap();
        assert!(
            remapped
                .events
                .iter()
                .all(|event| event.action.user() == 42)
        );
        assert_eq!(
            remapped
                .events
                .iter()
                .map(|event| event.at_micros)
                .collect::<Vec<_>>(),
            clip.events
                .iter()
                .map(|event| event.at_micros)
                .collect::<Vec<_>>()
        );
        let original_device = match &clip.events[1].action {
            Action::Packet { packet, .. } => packet.device,
            _ => unreachable!(),
        };
        let remapped_device = match &remapped.events[1].action {
            Action::Packet { packet, .. } => packet.device,
            _ => unreachable!(),
        };
        assert_eq!(original_device, remapped_device);
    }
}
