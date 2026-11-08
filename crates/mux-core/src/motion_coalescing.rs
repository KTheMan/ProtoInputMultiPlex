//! Bounded scheduling primitives for publication-only relative/absolute motion.
//!
//! Callers must transform each incoming motion sample before enqueueing it. This
//! scheduler only combines already-transformed native publications.
use crate::desktop::{Bounds, MotionCoalescingSettings};
use anyhow::{Result, ensure};
use std::time::{Duration, Instant};

/// Identity of the native recipient for one user's pending movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MotionKey {
    pub user: usize,
    pub pid: u32,
    pub hwnd: Option<usize>,
    pub mouse: u32,
    pub client: Option<Bounds>,
}

/// A latest absolute goal or a sum of already-transformed relative deltas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionPayload {
    Position { x: i32, y: i32 },
    Delta { x: i64, y: i64 },
}

/// One bounded pending publication segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingMotion {
    pub key: MotionKey,
    pub settings: MotionCoalescingSettings,
    pub first_at: Instant,
    pub events: u8,
    pub payload: MotionPayload,
}

impl PendingMotion {
    pub fn new(
        key: MotionKey,
        settings: MotionCoalescingSettings,
        now: Instant,
        payload: MotionPayload,
    ) -> Result<Self> {
        settings.validate()?;
        ensure!(
            settings.buffer_events.is_some() || settings.interval_ms.is_some(),
            "motion coalescing needs a buffer or timer trigger"
        );
        Ok(Self {
            key,
            settings,
            first_at: now,
            events: 1,
            payload,
        })
    }

    pub fn matches(&self, key: MotionKey, settings: MotionCoalescingSettings) -> bool {
        self.key == key && self.settings == settings
    }

    pub fn push_position(&mut self, key: MotionKey, x: i32, y: i32) -> Result<()> {
        ensure!(self.key == key, "motion recipient changed before flush");
        ensure!(
            matches!(self.payload, MotionPayload::Position { .. }),
            "motion publication type changed before flush"
        );
        ensure!(
            self.events < 64,
            "motion event safety limit reached before flush"
        );
        self.events += 1;
        self.payload = MotionPayload::Position { x, y };
        Ok(())
    }

    pub fn push_delta(&mut self, key: MotionKey, x: i32, y: i32) -> Result<()> {
        ensure!(self.key == key, "motion recipient changed before flush");
        let MotionPayload::Delta {
            x: pending_x,
            y: pending_y,
        } = self.payload
        else {
            anyhow::bail!("motion publication type changed before flush")
        };
        ensure!(
            self.events < 64,
            "motion event safety limit reached before flush"
        );
        let next_x = pending_x
            .checked_add(i64::from(x))
            .ok_or_else(|| anyhow::anyhow!("coalesced X motion overflow"))?;
        let next_y = pending_y
            .checked_add(i64::from(y))
            .ok_or_else(|| anyhow::anyhow!("coalesced Y motion overflow"))?;
        self.events += 1;
        self.payload = MotionPayload::Delta {
            x: next_x,
            y: next_y,
        };
        Ok(())
    }

    /// Time trigger for this segment. Without a configured timer, 16 ms bounds
    /// the age of a buffer-only segment.
    pub fn deadline(self) -> Instant {
        self.first_at + Duration::from_millis(u64::from(self.settings.interval_ms.unwrap_or(16)))
    }

    /// A configured trigger, or a safety cap, has been reached.
    pub fn should_flush(self, now: Instant) -> bool {
        let event_limit = self.settings.buffer_events.unwrap_or(64);
        self.events >= event_limit || now >= self.deadline()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(hwnd: Option<usize>) -> MotionKey {
        MotionKey {
            user: 2,
            pid: 41,
            hwnd,
            mouse: 7,
            client: hwnd.map(|_| Bounds {
                left: 10,
                top: 20,
                right: 210,
                bottom: 120,
            }),
        }
    }

    #[test]
    fn count_trigger_keeps_only_latest_absolute_goal() {
        let now = Instant::now();
        let settings = MotionCoalescingSettings {
            buffer_events: Some(2),
            interval_ms: Some(16),
        };
        let mut pending = PendingMotion::new(
            key(Some(100)),
            settings,
            now,
            MotionPayload::Position { x: 15, y: 25 },
        )
        .unwrap();
        pending.push_position(key(Some(100)), 33, 44).unwrap();
        assert_eq!(pending.events, 2);
        assert!(pending.should_flush(now));
        assert_eq!(pending.payload, MotionPayload::Position { x: 33, y: 44 });
    }

    #[test]
    fn delta_trigger_sums_transformed_samples_without_narrowing() {
        let now = Instant::now();
        let settings = MotionCoalescingSettings {
            buffer_events: Some(64),
            interval_ms: Some(16),
        };
        let mut pending = PendingMotion::new(
            key(None),
            settings,
            now,
            MotionPayload::Delta {
                x: i64::from(i32::MAX),
                y: -5,
            },
        )
        .unwrap();
        pending.push_delta(key(None), i32::MAX, i32::MIN).unwrap();
        assert_eq!(
            pending.payload,
            MotionPayload::Delta {
                x: i64::from(i32::MAX) * 2,
                y: i64::from(i32::MIN) - 5,
            }
        );
    }

    #[test]
    fn timer_uses_oldest_sample_and_caps_timer_only_event_count() {
        let now = Instant::now();
        let settings = MotionCoalescingSettings {
            buffer_events: None,
            interval_ms: Some(2),
        };
        let mut pending = PendingMotion::new(
            key(None),
            settings,
            now,
            MotionPayload::Delta { x: 1, y: 0 },
        )
        .unwrap();
        pending.push_delta(key(None), 1, 0).unwrap();
        assert_eq!(pending.deadline(), now + Duration::from_millis(2));
        assert!(!pending.should_flush(now));
        assert!(pending.should_flush(now + Duration::from_millis(2)));

        let safety = MotionCoalescingSettings {
            buffer_events: None,
            interval_ms: Some(16),
        };
        let mut capped =
            PendingMotion::new(key(None), safety, now, MotionPayload::Delta { x: 1, y: 0 })
                .unwrap();
        for _ in 1..64 {
            capped.push_delta(key(None), 1, 0).unwrap();
        }
        assert_eq!(capped.events, 64);
        assert!(capped.should_flush(now));
    }

    #[test]
    fn buffer_only_uses_bounded_age_and_identity_includes_geometry() {
        let now = Instant::now();
        let settings = MotionCoalescingSettings {
            buffer_events: Some(64),
            interval_ms: None,
        };
        let mut pending = PendingMotion::new(
            key(Some(100)),
            settings,
            now,
            MotionPayload::Position { x: 15, y: 25 },
        )
        .unwrap();
        for _ in 1..64 {
            pending.push_position(key(Some(100)), 15, 25).unwrap();
        }
        assert_eq!(pending.deadline(), now + Duration::from_millis(16));
        assert!(pending.should_flush(now)); // count safety bound
        assert!(!pending.matches(
            MotionKey {
                client: Some(Bounds {
                    left: 11,
                    top: 20,
                    right: 210,
                    bottom: 120,
                }),
                ..key(Some(100))
            },
            settings
        ));
        assert!(pending.should_flush(now + Duration::from_millis(16)));
    }

    #[test]
    fn pushes_reject_more_than_safety_limit_without_mutating_state() {
        let now = Instant::now();
        let settings = MotionCoalescingSettings {
            buffer_events: Some(64),
            interval_ms: Some(16),
        };
        let mut pending = PendingMotion::new(
            key(None),
            settings,
            now,
            MotionPayload::Delta {
                x: i64::MAX - 1,
                y: 0,
            },
        )
        .unwrap();
        assert!(pending.push_delta(key(None), 2, 0).is_err());
        assert_eq!(pending.events, 1);
        assert_eq!(
            pending.payload,
            MotionPayload::Delta {
                x: i64::MAX - 1,
                y: 0
            }
        );

        for _ in 1..64 {
            pending.push_delta(key(None), 0, 0).unwrap();
        }
        let full = pending;
        assert!(pending.push_delta(key(None), 1, 0).is_err());
        assert_eq!(pending, full);
    }
}
