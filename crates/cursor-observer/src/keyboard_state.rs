//! Pure reducer for consumed, tagged Win32 keyboard messages.
//!
//! This file deliberately has no Win32, process, thread, lifecycle, or publication
//! dependencies. A caller supplies the original message fields and the final queue
//! disposition; lifecycle generation and native `SetKeyboardState` publication belong
//! to the eventual integration layer.

const PROTOINPUT_KEY_TAG: u64 = 0x1000_0000;
const VK_LIMIT: usize = 256;

const WM_KEYDOWN: u32 = 0x0100;
const WM_KEYUP: u32 = 0x0101;
const WM_SYSKEYDOWN: u32 = 0x0104;
const WM_SYSKEYUP: u32 = 0x0105;

const VK_SHIFT: usize = 0x10;
const VK_CONTROL: usize = 0x11;
const VK_MENU: usize = 0x12;
const VK_CAPITAL: usize = 0x14;
const VK_NUMLOCK: usize = 0x90;
const VK_SCROLL: usize = 0x91;

const VK_LSHIFT: usize = 0xA0;
const VK_RSHIFT: usize = 0xA1;
const VK_LCONTROL: usize = 0xA2;
const VK_RCONTROL: usize = 0xA3;
const VK_LMENU: usize = 0xA4;
const VK_RMENU: usize = 0xA5;

const DOWN: u8 = 0x80;
const TOGGLE: u8 = 0x01;

/// Original fields and final disposition for one Get/Peek queue result.
///
/// `raw_wparam` must be captured before the native keyboard signature is stripped.
/// `removed` is false for PeekMessage with PM_NOREMOVE. `final_allowed` is the result
/// after every queue filter has run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyMessage {
    pub message: u32,
    pub raw_wparam: u64,
    pub lparam: u64,
    pub removed: bool,
    pub final_allowed: bool,
}

/// A zero-initialized consumed-key table for one eventual consuming GUI thread.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyboardStateReducer {
    consumed_keyboard_table: [u8; VK_LIMIT],
}

impl Default for KeyboardStateReducer {
    fn default() -> Self {
        Self {
            consumed_keyboard_table: [0; VK_LIMIT],
        }
    }
}

impl KeyboardStateReducer {
    /// Apply one actual removed, final-allowed ProtoInput keyboard key message.
    ///
    /// Returns true exactly when the event was accepted and advanced the table.
    /// Rejected queue observations and non-key messages leave the table untouched.
    pub fn apply(&mut self, event: KeyMessage) -> bool {
        if !event.removed || !event.final_allowed {
            return false;
        }

        let down = match event.message {
            WM_KEYDOWN | WM_SYSKEYDOWN => true,
            WM_KEYUP | WM_SYSKEYUP => false,
            _ => return false,
        };

        let allowed_wparam_bits = PROTOINPUT_KEY_TAG | 0xFF;
        if event.raw_wparam & PROTOINPUT_KEY_TAG == 0
            || event.raw_wparam & !allowed_wparam_bits != 0
        {
            return false;
        }

        let vk = (event.raw_wparam & 0xFF) as usize;
        if vk == 0 || vk >= 0xFF {
            return false;
        }

        let scan = ((event.lparam >> 16) & 0xFF) as u8;
        let extended = event.lparam & (1 << 24) != 0;
        let Some(vk) = Self::normalize_modifier(vk, scan, extended) else {
            return false;
        };

        let old = self.consumed_keyboard_table[vk];
        let was_down = old & DOWN != 0;
        let mut next = (old & !DOWN) | if down { DOWN } else { 0 };

        if down && !was_down && Self::is_lock_key(vk) {
            next ^= TOGGLE;
        }

        self.consumed_keyboard_table[vk] = next;
        self.refresh_modifier_aliases(vk);
        true
    }

    /// Read the reduced 256-byte table for native publication or synchronous queries.
    pub fn keyboard_table(&self) -> &[u8; VK_LIMIT] {
        &self.consumed_keyboard_table
    }

    fn normalize_modifier(vk: usize, scan: u8, extended: bool) -> Option<usize> {
        match vk {
            VK_SHIFT => match scan {
                0x2A => Some(VK_LSHIFT),
                0x36 => Some(VK_RSHIFT),
                _ => None,
            },
            VK_CONTROL => Some(if extended { VK_RCONTROL } else { VK_LCONTROL }),
            VK_MENU => Some(if extended { VK_RMENU } else { VK_LMENU }),
            _ => Some(vk),
        }
    }

    fn refresh_modifier_aliases(&mut self, vk: usize) {
        let (generic, left, right) = match vk {
            VK_LSHIFT | VK_RSHIFT => (VK_SHIFT, VK_LSHIFT, VK_RSHIFT),
            VK_LCONTROL | VK_RCONTROL => (VK_CONTROL, VK_LCONTROL, VK_RCONTROL),
            VK_LMENU | VK_RMENU => (VK_MENU, VK_LMENU, VK_RMENU),
            _ => return,
        };

        let held = self.consumed_keyboard_table[left] & DOWN != 0
            || self.consumed_keyboard_table[right] & DOWN != 0;
        let generic_state = self.consumed_keyboard_table[generic];
        self.consumed_keyboard_table[generic] =
            (generic_state & !DOWN) | if held { DOWN } else { 0 };
    }

    fn is_lock_key(vk: usize) -> bool {
        matches!(vk, VK_CAPITAL | VK_NUMLOCK | VK_SCROLL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(message: u32, vk: u8, scan: u8, extended: bool) -> KeyMessage {
        KeyMessage {
            message,
            raw_wparam: PROTOINPUT_KEY_TAG | u64::from(vk),
            lparam: (u64::from(scan) << 16) | (u64::from(extended) << 24),
            removed: true,
            final_allowed: true,
        }
    }

    #[test]
    fn removed_key_and_syskey_edges_preserve_toggle_bits_on_release() {
        let mut state = KeyboardStateReducer::default();
        assert_eq!(state.keyboard_table(), &[0; VK_LIMIT]);

        assert!(state.apply(key(WM_KEYDOWN, 0x41, 0x1E, false)));
        assert_eq!(state.keyboard_table()[0x41], DOWN);
        assert!(state.apply(key(WM_SYSKEYUP, 0x41, 0x1E, false)));
        assert_eq!(state.keyboard_table()[0x41], 0);

        assert!(state.apply(key(WM_SYSKEYDOWN, VK_CAPITAL as u8, 0x3A, false)));
        assert_eq!(state.keyboard_table()[VK_CAPITAL], DOWN | TOGGLE);
        assert!(state.apply(key(WM_KEYUP, VK_CAPITAL as u8, 0x3A, false)));
        assert_eq!(state.keyboard_table()[VK_CAPITAL], TOGGLE);
    }

    #[test]
    fn no_remove_or_final_block_never_advances_or_toggles() {
        let mut state = KeyboardStateReducer::default();
        let mut event = key(WM_KEYDOWN, VK_CAPITAL as u8, 0x3A, false);
        event.removed = false;
        assert!(!state.apply(event));

        event.removed = true;
        event.final_allowed = false;
        assert!(!state.apply(event));
        assert_eq!(state.keyboard_table()[VK_CAPITAL], 0);

        event.removed = true;
        event.final_allowed = true;
        assert!(state.apply(event));
        assert!(state.apply(event));
        assert_eq!(state.keyboard_table()[VK_CAPITAL], DOWN | TOGGLE);
    }

    #[test]
    fn untagged_and_blocked_physical_events_leave_virtual_state_intact() {
        let mut state = KeyboardStateReducer::default();
        assert!(state.apply(key(WM_KEYDOWN, 0x41, 0x1E, false)));

        let mut blocked_up = key(WM_KEYUP, 0x41, 0x1E, false);
        blocked_up.final_allowed = false;
        assert!(!state.apply(blocked_up));

        let mut untagged_up = key(WM_KEYUP, 0x41, 0x1E, false);
        untagged_up.raw_wparam = 0x41;
        assert!(!state.apply(untagged_up));
        assert_eq!(state.keyboard_table()[0x41], DOWN);
    }

    #[test]
    fn generic_aliases_follow_both_sides_independently() {
        let mut state = KeyboardStateReducer::default();
        assert!(state.apply(key(WM_KEYDOWN, VK_SHIFT as u8, 0x2A, false)));
        assert_eq!(state.keyboard_table()[VK_LSHIFT], DOWN);
        assert_eq!(state.keyboard_table()[VK_SHIFT], DOWN);
        assert!(state.apply(key(WM_KEYDOWN, VK_SHIFT as u8, 0x36, false)));
        assert_eq!(state.keyboard_table()[VK_RSHIFT], DOWN);

        assert!(state.apply(key(WM_KEYUP, VK_LSHIFT as u8, 0x2A, false)));
        assert_eq!(state.keyboard_table()[VK_LSHIFT], 0);
        assert_eq!(state.keyboard_table()[VK_SHIFT], DOWN);
        assert!(state.apply(key(WM_SYSKEYDOWN, VK_CONTROL as u8, 0x1D, false)));
        assert!(state.apply(key(WM_SYSKEYDOWN, VK_CONTROL as u8, 0x1D, true)));
        assert_eq!(state.keyboard_table()[VK_CONTROL], DOWN);
        assert_eq!(state.keyboard_table()[VK_LCONTROL], DOWN);
        assert_eq!(state.keyboard_table()[VK_RCONTROL], DOWN);
        assert!(state.apply(key(WM_KEYUP, VK_CONTROL as u8, 0x1D, false)));
        assert_eq!(state.keyboard_table()[VK_CONTROL], DOWN);
        assert!(state.apply(key(WM_SYSKEYUP, VK_CONTROL as u8, 0x1D, true)));
        assert_eq!(state.keyboard_table()[VK_CONTROL], 0);

        assert!(state.apply(key(WM_KEYDOWN, VK_MENU as u8, 0x38, false)));
        assert!(state.apply(key(WM_KEYDOWN, VK_MENU as u8, 0x38, true)));
        assert_eq!(state.keyboard_table()[VK_MENU], DOWN);
        assert_eq!(state.keyboard_table()[VK_LMENU], DOWN);
        assert_eq!(state.keyboard_table()[VK_RMENU], DOWN);
    }

    #[test]
    fn lock_repeats_toggle_once_and_keyup_keeps_the_low_bit() {
        let mut state = KeyboardStateReducer::default();
        for vk in [VK_CAPITAL, VK_NUMLOCK, VK_SCROLL] {
            assert!(state.apply(key(WM_KEYDOWN, vk as u8, 0, false)));
            assert!(state.apply(key(WM_KEYDOWN, vk as u8, 0, false)));
            assert_eq!(state.keyboard_table()[vk], DOWN | TOGGLE);
            assert!(state.apply(key(WM_KEYUP, vk as u8, 0, false)));
            assert_eq!(state.keyboard_table()[vk], TOGGLE);
        }
    }

    #[test]
    fn invalid_virtual_keys_tag_residue_and_character_messages_are_ignored() {
        let mut state = KeyboardStateReducer::default();
        for vk in [0, 0xFF] {
            assert!(!state.apply(key(WM_KEYDOWN, vk, 0, false)));
        }

        let mut residue = key(WM_KEYDOWN, 0x41, 0x1E, false);
        residue.raw_wparam |= 1 << 8;
        assert!(!state.apply(residue));

        for message in [0x0102, 0x0103, 0x0106] {
            assert!(!state.apply(key(message, 0x41, 0x1E, false)));
        }
        assert_eq!(state.keyboard_table(), &[0; VK_LIMIT]);
    }
}
