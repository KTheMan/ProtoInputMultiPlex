//! Fixed-width ABI for the per-target cursor snapshot mapping. No OS dependencies.

pub const MAGIC: u32 = u32::from_le_bytes(*b"PCUR");
pub const VERSION: u16 = 1;
pub const HEADER_BYTES: usize = 112;
pub const MAX_DIMENSION: u32 = 256;
pub const MAX_PAYLOAD_BYTES: usize = 256 * 256 * 4;
pub const MAPPING_BYTES: usize = HEADER_BYTES + MAX_PAYLOAD_BYTES;
pub const KIND_NONE: u32 = 0;
pub const KIND_BGRA8_PREMULTIPLIED: u32 = 2;

pub const STATUS_EMPTY: u32 = 0;
pub const STATUS_VALID: u32 = 1;
pub const STATUS_HIDDEN: u32 = 2;
pub const STATUS_UNSUPPORTED: u32 = 3;
pub const STATUS_CAPTURE_FAILED: u32 = 4;
pub const STATUS_STOPPED: u32 = 5;

const REQUEST_STATUS_BITS: u32 = 4;
const REQUEST_STATUS_MASK: u64 = (1 << REQUEST_STATUS_BITS) - 1;

/// A lock-free latest-request generation/status pair shared by observers and
/// useful to ensure a dropped newest capture invalidates older pending imagery.
pub struct RequestClock(core::sync::atomic::AtomicU64);

impl RequestClock {
    pub const fn new(status: u32) -> Self {
        Self(core::sync::atomic::AtomicU64::new(status as u64))
    }

    pub fn snapshot(&self) -> (u64, u32) {
        use core::sync::atomic::Ordering;
        let value = self.0.load(Ordering::Acquire);
        (
            value >> REQUEST_STATUS_BITS,
            (value & REQUEST_STATUS_MASK) as u32,
        )
    }

    pub fn advance(&self, status: u32) -> u64 {
        use core::sync::atomic::Ordering;
        let mut old = self.0.load(Ordering::Acquire);
        loop {
            let sequence =
                ((old >> REQUEST_STATUS_BITS).wrapping_add(1)) & (u64::MAX >> REQUEST_STATUS_BITS);
            let next =
                (sequence << REQUEST_STATUS_BITS) | (u64::from(status) & REQUEST_STATUS_MASK);
            match self
                .0
                .compare_exchange_weak(old, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return sequence,
                Err(actual) => old = actual,
            }
        }
    }

    /// Advances only if no newer request has superseded the expected generation.
    /// Failure leaves the newer request untouched; the shared mapping ABI is unchanged.
    pub fn advance_if(&self, expected_sequence: u64, status: u32) -> Option<u64> {
        use core::sync::atomic::Ordering;
        let old = self.0.load(Ordering::Acquire);
        if old >> REQUEST_STATUS_BITS != expected_sequence {
            return None;
        }
        let sequence = expected_sequence.wrapping_add(1) & (u64::MAX >> REQUEST_STATUS_BITS);
        let next = (sequence << REQUEST_STATUS_BITS) | (u64::from(status) & REQUEST_STATUS_MASK);
        self.0
            .compare_exchange(old, next, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| sequence)
    }

    pub fn set_status(&self, sequence: u64, status: u32) {
        use core::sync::atomic::Ordering;
        let old = self.0.load(Ordering::Acquire);
        if old >> REQUEST_STATUS_BITS != sequence {
            return;
        }
        let next = (sequence << REQUEST_STATUS_BITS) | (u64::from(status) & REQUEST_STATUS_MASK);
        let _ = self
            .0
            .compare_exchange(old, next, Ordering::AcqRel, Ordering::Acquire);
    }
}

/// A cursor state record. `kind` is 0 for no shape, an IDC stock resource ID
/// (32512..32651 set listed below) for a host-rendered system shape, or 2 for
/// premultiplied BGRA8 custom pixels. For stock shapes `format` and all image
/// fields are zero; for custom pixels `format=1`. No target-owned handle crosses.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SnapshotHeader {
    pub magic: u32,             // 0
    pub version: u16,           // 4
    pub header_bytes: u16,      // 6
    pub mapping_bytes: u32,     // 8
    pub target_pid: u32,        // 12
    pub status: u32,            // 16
    pub flags: u32,             // 20; bit 0 = visible
    pub attachment_epoch: u64,  // 24
    pub association_epoch: u64, // 32
    pub sequence: u64,          // 40
    pub selected_hwnd: u64,     // 48
    pub kind: u32,              // 56
    pub width: u32,             // 60
    pub height: u32,            // 64
    pub stride: u32,            // 68
    pub format: u32,            // 72; 1 = premultiplied BGRA8
    pub hotspot_x: i32,         // 76
    pub hotspot_y: i32,         // 80
    pub payload_len: u32,       // 84
    pub reserved: [u32; 6],     // 88..112; must be zero
}

impl SnapshotHeader {
    pub const fn empty(target_pid: u32, attachment_epoch: u64) -> Self {
        Self {
            magic: MAGIC,
            version: VERSION,
            header_bytes: HEADER_BYTES as u16,
            mapping_bytes: MAPPING_BYTES as u32,
            target_pid,
            status: STATUS_EMPTY,
            flags: 0,
            attachment_epoch,
            association_epoch: 0,
            sequence: 0,
            selected_hwnd: 0,
            kind: KIND_NONE,
            width: 0,
            height: 0,
            stride: 0,
            format: 0,
            hotspot_x: 0,
            hotspot_y: 0,
            payload_len: 0,
            reserved: [0; 6],
        }
    }

    pub fn validate(&self, mapping_len: usize, expected_pid: u32, attachment: u64) -> bool {
        if self.magic != MAGIC
            || self.version != VERSION
            || usize::from(self.header_bytes) != HEADER_BYTES
            || usize::try_from(self.mapping_bytes).ok() != Some(MAPPING_BYTES)
            || mapping_len < MAPPING_BYTES
            || self.target_pid != expected_pid
            || self.attachment_epoch != attachment
            || self.reserved != [0; 6]
            || self.status > STATUS_STOPPED
            || self.flags & !1 != 0
            || self.flags != u32::from(self.status == STATUS_VALID)
            || self.payload_len as usize > MAX_PAYLOAD_BYTES
        {
            return false;
        }
        match self.kind {
            KIND_NONE => self.no_image() && self.status != STATUS_VALID,
            32512 | 32513 | 32514 | 32515 | 32516 | 32642 | 32643 | 32644 | 32645 | 32646
            | 32648 | 32649 | 32650 | 32651 => self.no_image() && self.status == STATUS_VALID,
            KIND_BGRA8_PREMULTIPLIED => {
                self.status == STATUS_VALID
                    && self.width != 0
                    && self.height != 0
                    && self.width <= MAX_DIMENSION
                    && self.height <= MAX_DIMENSION
                    && self.stride == self.width.saturating_mul(4)
                    && self.format == 1
                    && self.payload_len == self.stride.saturating_mul(self.height)
                    && self.hotspot_x >= 0
                    && self.hotspot_y >= 0
                    && self.hotspot_x < self.width as i32
                    && self.hotspot_y < self.height as i32
            }
            _ => false,
        }
    }

    /// Checks pixel bytes after the host copies them while holding the mapping mutex.
    pub fn validate_bgra_payload(&self, payload: &[u8]) -> bool {
        if self.kind != KIND_BGRA8_PREMULTIPLIED
            || payload.len() != self.payload_len as usize
            || !self.validate(MAPPING_BYTES, self.target_pid, self.attachment_epoch)
        {
            return false;
        }
        payload
            .chunks(4)
            .all(|pixel| pixel[0] <= pixel[3] && pixel[1] <= pixel[3] && pixel[2] <= pixel[3])
    }

    fn no_image(&self) -> bool {
        self.width == 0
            && self.height == 0
            && self.stride == 0
            && self.format == 0
            && self.hotspot_x == 0
            && self.hotspot_y == 0
            && self.payload_len == 0
    }
}

pub const fn known_system_resource(id: u32) -> bool {
    matches!(
        id,
        32512
            | 32513
            | 32514
            | 32515
            | 32516
            | 32642
            | 32643
            | 32644
            | 32645
            | 32646
            | 32648
            | 32649
            | 32650
            | 32651
    )
}

const _: [(); HEADER_BYTES] = [(); core::mem::size_of::<SnapshotHeader>()];
const _: [(); 24] = [(); core::mem::offset_of!(SnapshotHeader, attachment_epoch)];
const _: [(); 32] = [(); core::mem::offset_of!(SnapshotHeader, association_epoch)];
const _: [(); 48] = [(); core::mem::offset_of!(SnapshotHeader, selected_hwnd)];
const _: [(); 76] = [(); core::mem::offset_of!(SnapshotHeader, hotspot_x)];
const _: [(); 84] = [(); core::mem::offset_of!(SnapshotHeader, payload_len)];

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> SnapshotHeader {
        SnapshotHeader::empty(42, 7)
    }

    #[test]
    fn validates_known_shapes_status_and_reserved_wire_values() {
        let mut value = header();
        assert!(value.validate(MAPPING_BYTES, 42, 7));

        value.status = STATUS_VALID;
        value.flags = 1;
        value.kind = 32512;
        assert!(value.validate(MAPPING_BYTES, 42, 7));

        value.kind = 999;
        assert!(!value.validate(MAPPING_BYTES, 42, 7));
        value.kind = 32512;
        value.format = 4;
        assert!(!value.validate(MAPPING_BYTES, 42, 7));
        value.format = 0;
        value.status = 6;
        assert!(!value.validate(MAPPING_BYTES, 42, 7));
        value.status = STATUS_VALID;
        value.flags = 3;
        assert!(!value.validate(MAPPING_BYTES, 42, 7));
    }

    #[test]
    fn validates_custom_bounds_and_premultiplied_payload() {
        let mut value = header();
        value.status = STATUS_VALID;
        value.flags = 1;
        value.kind = KIND_BGRA8_PREMULTIPLIED;
        value.width = 1;
        value.height = 1;
        value.stride = 4;
        value.format = 1;
        value.hotspot_x = 0;
        value.hotspot_y = 0;
        value.payload_len = 4;
        assert!(value.validate(MAPPING_BYTES, 42, 7));
        assert!(value.validate_bgra_payload(&[10, 20, 30, 40]));
        assert!(!value.validate_bgra_payload(&[10, 20, 41, 40]));

        value.width = MAX_DIMENSION + 1;
        assert!(!value.validate(MAPPING_BYTES, 42, 7));
    }

    #[test]
    fn newest_request_supersedes_a_pending_capture_status() {
        let clock = RequestClock::new(STATUS_EMPTY);
        let old = clock.advance(STATUS_VALID);
        let newest = clock.advance(STATUS_HIDDEN);
        clock.set_status(old, STATUS_CAPTURE_FAILED);
        assert_eq!(clock.snapshot(), (newest, STATUS_HIDDEN));
        clock.set_status(newest, STATUS_UNSUPPORTED);
        assert_eq!(clock.snapshot(), (newest, STATUS_UNSUPPORTED));
    }

    #[test]
    fn concurrent_notifications_receive_unique_generations() {
        let clock = std::sync::Arc::new(RequestClock::new(STATUS_EMPTY));
        let mut threads = Vec::new();
        for _ in 0..8 {
            let clock = std::sync::Arc::clone(&clock);
            threads.push(std::thread::spawn(move || {
                (0..128)
                    .map(|_| clock.advance(STATUS_VALID))
                    .collect::<Vec<_>>()
            }));
        }
        let sequences = threads
            .into_iter()
            .flat_map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        let mut ordered = sequences.clone();
        ordered.sort_unstable();
        ordered.dedup();
        assert_eq!(ordered.len(), 1024);
        assert_eq!(clock.snapshot().0, 1024);
    }
    #[test]
    fn conditional_outcome_cannot_overwrite_a_later_notification() {
        let clock = RequestClock::new(STATUS_EMPTY);
        let expected = clock.snapshot().0;
        let outcome = clock.advance_if(expected, STATUS_UNSUPPORTED).unwrap();
        assert_eq!(clock.snapshot(), (outcome, STATUS_UNSUPPORTED));
        let notified = clock.advance(STATUS_HIDDEN);
        assert_eq!(clock.advance_if(outcome, STATUS_CAPTURE_FAILED), None);
        assert_eq!(clock.snapshot(), (notified, STATUS_HIDDEN));
        assert_eq!(clock.advance_if(expected, STATUS_UNSUPPORTED), None);
    }
}
