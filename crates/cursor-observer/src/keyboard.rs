//! Candidate target-side synchronous keyboard provider.
//!
//! Integration is deliberately separate from cursor observer start/stop. The
//! native bridge calls these exports on the thread that actually removes a
//! message; no work is sent to a worker or another GUI thread.

#[path = "keyboard_state.rs"]
mod keyboard_state;

use keyboard_state::{KeyMessage, KeyboardStateReducer};
use std::{
    cell::RefCell,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
    sync::{Mutex, MutexGuard},
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetKeyboardState;

const VERSION: u32 = 1;
const OK: u32 = 0;
const DISABLED: u32 = 1;
const BUSY: u32 = 2;
const INVALID: u32 = 3;
const PUBLICATION_FAILED: u32 = 4;
const HELD: u32 = 5;
const MOUSE_MASK: u32 = 0x1f;
const MOUSE_KEYS: [usize; 5] = [0x01, 0x02, 0x04, 0x05, 0x06];
const MAX_REENTRANT_PUBLICATIONS: usize = 16;

#[derive(Default)]
struct Lifecycle {
    active_epoch: u64,
    last_epoch: u64,
    in_flight: usize,
    held_threads: usize,
    publication_failed: bool,
}

impl Lifecycle {
    fn start(&mut self, epoch: u64) -> u32 {
        if epoch == 0 {
            return INVALID;
        }
        if self.active_epoch != 0 || self.in_flight != 0 || self.held_threads != 0 {
            return BUSY;
        }
        if self.publication_failed {
            return PUBLICATION_FAILED;
        }
        if epoch == self.last_epoch {
            return INVALID;
        }
        self.active_epoch = epoch;
        self.last_epoch = epoch;
        OK
    }

    fn stop(&mut self, epoch: u64) -> u32 {
        if self.active_epoch == 0 {
            return DISABLED;
        }
        if epoch == 0 || epoch != self.active_epoch {
            return INVALID;
        }
        if self.publication_failed {
            return PUBLICATION_FAILED;
        }
        if self.in_flight != 0 {
            return BUSY;
        }
        if self.held_threads != 0 {
            return HELD;
        }
        self.active_epoch = 0;
        OK
    }

    fn admit(&mut self) -> Result<Admission, u32> {
        if self.active_epoch == 0 {
            return Err(DISABLED);
        }
        if self.publication_failed {
            return Err(PUBLICATION_FAILED);
        }
        self.in_flight += 1;
        Ok(Admission {
            epoch: self.active_epoch,
        })
    }
}

static LIFECYCLE: Mutex<Lifecycle> = Mutex::new(Lifecycle {
    active_epoch: 0,
    last_epoch: 0,
    in_flight: 0,
    held_threads: 0,
    publication_failed: false,
});

fn lifecycle() -> MutexGuard<'static, Lifecycle> {
    LIFECYCLE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

fn mark_publication_failed() {
    lifecycle().publication_failed = true;
}

fn abi(f: impl FnOnce() -> u32) -> u32 {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| {
        mark_publication_failed();
        INVALID
    })
}

struct Admission {
    epoch: u64,
}

impl Drop for Admission {
    fn drop(&mut self) {
        let mut state = lifecycle();
        state.in_flight -= 1;
    }
}

#[derive(Default)]
struct ThreadKeyboard {
    epoch: u64,
    reducer: KeyboardStateReducer,
    mouse_bits: u32,
    revision: u64,
    counted_held: bool,
}

impl ThreadKeyboard {
    fn ensure_epoch(&mut self, epoch: u64) {
        if self.epoch != epoch {
            // A different generation can start only after the previous one
            // cleanly stopped with no counted held thread.
            *self = Self {
                epoch,
                reducer: KeyboardStateReducer::default(),
                mouse_bits: 0,
                revision: 0,
                counted_held: false,
            };
        }
    }

    fn table(&self, mouse_bits: u32) -> [u8; 256] {
        compose_mouse(*self.reducer.keyboard_table(), mouse_bits)
    }

    fn has_keyboard_down(&self) -> bool {
        self.reducer
            .keyboard_table()
            .iter()
            .any(|value| value & 0x80 != 0)
    }

    fn removed_message(&mut self, event: KeyMessage, mouse_bits: u32) -> Option<bool> {
        if !event.removed {
            return None;
        }
        self.reducer.apply(event);
        self.mouse_bits = mouse_bits;
        self.revision = self.revision.wrapping_add(1);
        let held = self.has_keyboard_down();
        let changed = held != self.counted_held;
        self.counted_held = held;
        Some(changed)
    }
}

impl Drop for ThreadKeyboard {
    fn drop(&mut self) {
        if self.counted_held {
            let mut state = lifecycle();
            if state.active_epoch == self.epoch {
                state.held_threads -= 1;
            }
        }
    }
}

thread_local! {
    static THREAD_KEYBOARD: RefCell<ThreadKeyboard> = RefCell::new(ThreadKeyboard::default());
}

fn compose_mouse(mut table: [u8; 256], mouse_bits: u32) -> [u8; 256] {
    for (bit, vk) in MOUSE_KEYS.iter().enumerate() {
        table[*vk] = if mouse_bits & (1 << bit) != 0 {
            0x80
        } else {
            0
        };
    }
    table
}

fn update_held(changed: bool, held: bool) {
    if changed {
        let mut state = lifecycle();
        if held {
            state.held_threads += 1;
        } else {
            state.held_threads -= 1;
        }
    }
}

fn publish_current(epoch: u64) -> u32 {
    for _ in 0..MAX_REENTRANT_PUBLICATIONS {
        // Never hold a RefCell borrow or lifecycle mutex across USER32. A
        // reentrant callback may change the table while this call is pending.
        let (table, revision) = THREAD_KEYBOARD.with(|slot| {
            let state = slot.borrow();
            (state.table(state.mouse_bits), state.revision)
        });
        if unsafe { SetKeyboardState(table.as_ptr()) } == 0 {
            mark_publication_failed();
            return PUBLICATION_FAILED;
        }
        let stable = THREAD_KEYBOARD.with(|slot| {
            let state = slot.borrow();
            state.epoch == epoch && state.revision == revision
        });
        if stable {
            return OK;
        }
    }
    mark_publication_failed();
    PUBLICATION_FAILED
}

#[unsafe(no_mangle)]
pub extern "C" fn ProtoMuxKeyboardVersion() -> u32 {
    abi(|| VERSION)
}

#[unsafe(no_mangle)]
pub extern "C" fn ProtoMuxKeyboardStart(epoch: u64) -> u32 {
    abi(|| lifecycle().start(epoch))
}

#[unsafe(no_mangle)]
pub extern "C" fn ProtoMuxKeyboardStop(epoch: u64) -> u32 {
    abi(|| lifecycle().stop(epoch))
}

/// Supplies the original, unfiltered queue result and its final disposition.
/// A fence acknowledgement is valid only after an actual removal has published
/// a table with every key-down high bit clear on this calling GUI thread.
#[unsafe(no_mangle)]
pub extern "C" fn ProtoMuxKeyboardMessage(
    message: u32,
    raw_wparam: u64,
    lparam: u64,
    removed: u32,
    final_allowed: u32,
    mouse_bits: u32,
    fence: u32,
) -> u32 {
    abi(|| {
        let admission = match lifecycle().admit() {
            Ok(value) => value,
            Err(status) => return status,
        };
        if removed > 1 || final_allowed > 1 || fence > 1 || mouse_bits & !MOUSE_MASK != 0 {
            return INVALID;
        }
        if removed == 0 {
            return if fence == 0 { OK } else { INVALID };
        }
        let event = KeyMessage {
            message,
            raw_wparam,
            lparam,
            removed: true,
            final_allowed: final_allowed != 0,
        };
        let (changed, held) = THREAD_KEYBOARD.with(|slot| {
            let mut state = slot.borrow_mut();
            state.ensure_epoch(admission.epoch);
            let changed = state.removed_message(event, mouse_bits).unwrap_or(false);
            (changed, state.counted_held)
        });
        update_held(changed, held);
        let result = publish_current(admission.epoch);
        if result != OK {
            return result;
        }
        if fence != 0
            && THREAD_KEYBOARD.with(|slot| {
                let state = slot.borrow();
                state.epoch != admission.epoch || state.has_keyboard_down()
            })
        {
            return HELD;
        }
        OK
    })
}

/// # Safety
/// `out` must point to a writable i16 for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ProtoMuxKeyboardGetKeyState(
    vkey: i32,
    mouse_bits: u32,
    out: *mut i16,
) -> u32 {
    abi(|| {
        let admission = match lifecycle().admit() {
            Ok(value) => value,
            Err(status) => return status,
        };
        if out.is_null() || !(0..256).contains(&vkey) || mouse_bits & !MOUSE_MASK != 0 {
            return INVALID;
        }
        let byte = THREAD_KEYBOARD.with(|slot| {
            let mut state = slot.borrow_mut();
            state.ensure_epoch(admission.epoch);
            state.table(mouse_bits)[vkey as usize]
        });
        unsafe { ptr::write(out, key_state_word(byte)) };
        OK
    })
}

/// # Safety
/// `out` must point to a writable array of at least 256 bytes for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ProtoMuxKeyboardGetKeyboardState(mouse_bits: u32, out: *mut u8) -> u32 {
    abi(|| {
        let admission = match lifecycle().admit() {
            Ok(value) => value,
            Err(status) => return status,
        };
        if out.is_null() || mouse_bits & !MOUSE_MASK != 0 {
            return INVALID;
        }
        let table = THREAD_KEYBOARD.with(|slot| {
            let mut state = slot.borrow_mut();
            state.ensure_epoch(admission.epoch);
            state.table(mouse_bits)
        });
        unsafe { ptr::copy_nonoverlapping(table.as_ptr(), out, table.len()) };
        OK
    })
}

fn key_state_word(byte: u8) -> i16 {
    ((u16::from(byte & 0x80) << 8) | u16::from(byte & 0x01)) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_requires_quiescence_release_and_distinct_nonce() {
        let mut state = Lifecycle::default();
        assert_eq!(state.start(3), OK);
        assert_eq!(state.start(4), BUSY);
        state.in_flight = 1;
        assert_eq!(state.stop(3), BUSY);
        state.in_flight = 0;
        state.held_threads = 1;
        assert_eq!(state.stop(3), HELD);
        state.held_threads = 0;
        assert_eq!(state.stop(3), OK);
        assert_eq!(state.start(0), INVALID);
        assert_eq!(state.start(3), INVALID);
        assert_eq!(state.start(2), OK);
        assert_eq!(state.stop(2), OK);
        assert_eq!(state.start(2), INVALID);
        assert_eq!(state.start(4), OK);
        state.publication_failed = true;
        assert_eq!(state.stop(4), PUBLICATION_FAILED);
    }

    #[test]
    fn mouse_only_snapshot_does_not_hold_keyboard_or_block_fence() {
        let mut state = ThreadKeyboard::default();
        state.ensure_epoch(7);
        let removed = KeyMessage {
            message: 0x0100,
            raw_wparam: 0x41, // Physical, so it cannot advance the reducer.
            lparam: 0,
            removed: true,
            final_allowed: false,
        };
        assert_eq!(state.removed_message(removed, 0b1), Some(false));
        assert_eq!(state.table(state.mouse_bits)[0x01], 0x80);
        assert!(!state.counted_held);
        assert!(!state.has_keyboard_down());

        let virtual_down = KeyMessage {
            raw_wparam: 0x1000_0041,
            final_allowed: true,
            ..removed
        };
        assert_eq!(state.removed_message(virtual_down, 0b1), Some(true));
        assert!(state.has_keyboard_down());
        let virtual_up = KeyMessage {
            message: 0x0101,
            ..virtual_down
        };
        assert_eq!(state.removed_message(virtual_up, 0b1), Some(true));
        assert!(!state.counted_held);
        assert!(!state.has_keyboard_down());
        assert_eq!(state.table(state.mouse_bits)[0x01], 0x80);
    }

    #[test]
    fn removed_physical_message_republishes_without_advancing_virtual_keys() {
        let mut state = ThreadKeyboard::default();
        state.ensure_epoch(7);
        let virtual_down = KeyMessage {
            message: 0x0100,
            raw_wparam: 0x1000_0041,
            lparam: 0,
            removed: true,
            final_allowed: true,
        };
        assert_eq!(state.removed_message(virtual_down, 0), Some(true));
        let blocked_physical_up = KeyMessage {
            raw_wparam: 0x41,
            message: 0x0101,
            final_allowed: false,
            ..virtual_down
        };
        assert_eq!(state.removed_message(blocked_physical_up, 0), Some(false));
        assert_eq!(state.table(0)[0x41], 0x80);
        assert_eq!(state.revision, 2);
        let peek = KeyMessage {
            removed: false,
            ..virtual_down
        };
        assert_eq!(state.removed_message(peek, 0), None);
        assert_eq!(state.revision, 2);
    }

    #[test]
    fn mouse_overlay_matches_legacy_virtual_keys_and_word_bits() {
        let table = compose_mouse([0xff; 256], 0b1_0101);
        for (bit, vk) in MOUSE_KEYS.iter().enumerate() {
            assert_eq!(
                table[*vk],
                if 0b1_0101 & (1 << bit) != 0 { 0x80 } else { 0 }
            );
        }
        assert_eq!(key_state_word(table[0x01]), i16::MIN);
        assert_eq!(key_state_word(table[0x02]), 0);
        assert_eq!(key_state_word(0x81), i16::MIN | 1);
    }
}
