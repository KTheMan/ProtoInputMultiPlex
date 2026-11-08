use mux_application_sdk::{ClientCommand, ControlGrant};
use mux_core::application_event::{Input, KeyCode, Transition, UserId, UserScope, UserView};
use mux_core::desktop::MouseButton;
use std::collections::BTreeMap;

/// Only transitions sent by this client are released. Observed state from another
/// consumer must never be released by browser focus changes.
#[derive(Debug, Default)]
pub struct LocalInput {
    pub selected: Option<UserId>,
    pub pointer_enabled: bool,
    pub keys_enabled: bool,
    buttons: Vec<(UserId, MouseButton)>,
    keys: Vec<(UserId, KeyCode)>,
}

fn scope_contains(grant: &ControlGrant, user: UserId) -> bool {
    match &grant.users {
        UserScope::Only(users) => users.contains(&user),
        UserScope::AllOwned => true,
    }
}

/// Event visibility alone never establishes ownership or delegated control.
pub fn selects(grant: &ControlGrant, user: &UserView) -> bool {
    scope_contains(grant, user.id)
        && (user.owned_by_recipient
            || (grant.delegated_users && matches!(&grant.users, UserScope::Only(_))))
}

impl LocalInput {
    pub fn held_buttons(&self) -> &[(UserId, MouseButton)] {
        &self.buttons
    }
    pub fn held_keys(&self) -> &[(UserId, KeyCode)] {
        &self.keys
    }

    /// Call only after the frame was successfully handed to the socket.
    pub fn record(&mut self, user: UserId, input: Input) {
        match input {
            Input::Button { button, transition } => {
                transition_held(&mut self.buttons, (user, button), transition)
            }
            Input::Key { key, transition } => {
                transition_held(&mut self.keys, (user, key), transition)
            }
            _ => {}
        }
    }

    /// No queued replay. Releasing is best effort only while current authority
    /// remains valid. The server retires owned held state on transport teardown.
    pub fn drain_releases(
        &mut self,
        authority: Option<&ControlGrant>,
        users: &BTreeMap<UserId, UserView>,
    ) -> Vec<ClientCommand> {
        let mut commands = Vec::new();
        for (user, button) in self.buttons.drain(..) {
            if authority.is_some_and(|grant| {
                grant.pointer_input && users.get(&user).is_some_and(|view| selects(grant, view))
            }) {
                commands.push(ClientCommand::Input {
                    user,
                    input: Input::Button {
                        button,
                        transition: Transition::Up,
                    },
                });
            }
        }
        for (user, key) in self.keys.drain(..) {
            if authority.is_some_and(|grant| {
                grant.key_input && users.get(&user).is_some_and(|view| selects(grant, view))
            }) {
                commands.push(ClientCommand::Input {
                    user,
                    input: Input::Key {
                        key,
                        transition: Transition::Up,
                    },
                });
            }
        }
        commands
    }

    pub fn disable(&mut self) {
        self.pointer_enabled = false;
        self.keys_enabled = false;
        self.selected = None;
    }
}

fn transition_held<T: PartialEq>(held: &mut Vec<T>, value: T, transition: Transition) {
    match (transition, held.iter().position(|item| *item == value)) {
        (Transition::Down, None) => held.push(value),
        (Transition::Up, Some(index)) => {
            held.remove(index);
        }
        _ => {}
    }
}

pub fn mouse_button(button: i16) -> Option<MouseButton> {
    match button {
        0 => Some(MouseButton::Primary),
        1 => Some(MouseButton::Middle),
        2 => Some(MouseButton::Secondary),
        3 => Some(MouseButton::X1),
        4 => Some(MouseButton::X2),
        _ => None,
    }
}

/// DOM code to USB HID page 0x07; layout-independent, no text or keyCode leakage.
/// Unmapped browser/system keys are visibly ignored by the caller.
pub fn hid_key(code: &str) -> Option<KeyCode> {
    let value = match code {
        "Enter" => 0x28,
        "Escape" => 0x29,
        "Backspace" => 0x2a,
        "Tab" => 0x2b,
        "Space" => 0x2c,
        "Minus" => 0x2d,
        "Equal" => 0x2e,
        "BracketLeft" => 0x2f,
        "BracketRight" => 0x30,
        "Backslash" => 0x31,
        "Semicolon" => 0x33,
        "Quote" => 0x34,
        "Backquote" => 0x35,
        "Comma" => 0x36,
        "Period" => 0x37,
        "Slash" => 0x38,
        "CapsLock" => 0x39,
        "PrintScreen" => 0x46,
        "ScrollLock" => 0x47,
        "Pause" => 0x48,
        "Insert" => 0x49,
        "Home" => 0x4a,
        "PageUp" => 0x4b,
        "Delete" => 0x4c,
        "End" => 0x4d,
        "PageDown" => 0x4e,
        "ArrowRight" => 0x4f,
        "ArrowLeft" => 0x50,
        "ArrowDown" => 0x51,
        "ArrowUp" => 0x52,
        "NumLock" => 0x53,
        "NumpadDivide" => 0x54,
        "NumpadMultiply" => 0x55,
        "NumpadSubtract" => 0x56,
        "NumpadAdd" => 0x57,
        "NumpadEnter" => 0x58,
        "NumpadDecimal" => 0x63,
        "IntlBackslash" => 0x64,
        "ContextMenu" => 0x65,
        "ControlLeft" => 0xe0,
        "ShiftLeft" => 0xe1,
        "AltLeft" => 0xe2,
        "MetaLeft" => 0xe3,
        "ControlRight" => 0xe4,
        "ShiftRight" => 0xe5,
        "AltRight" => 0xe6,
        "MetaRight" => 0xe7,
        _ => {
            if code.len() == 4 && code.starts_with("Key") {
                let letter = code.as_bytes()[3];
                if letter.is_ascii_uppercase() {
                    return Some(KeyCode(u16::from(letter - b'A') + 4));
                }
            }
            if code.len() == 6 && code.starts_with("Digit") {
                let digit = code.as_bytes()[5];
                if digit.is_ascii_digit() {
                    return Some(KeyCode(if digit == b'0' {
                        0x27
                    } else {
                        u16::from(digit - b'1') + 0x1e
                    }));
                }
            }
            if let Some(function) = code
                .strip_prefix('F')
                .and_then(|value| value.parse::<u16>().ok())
            {
                if (1..=12).contains(&function) {
                    return Some(KeyCode(0x39 + function));
                }
                if (13..=24).contains(&function) {
                    return Some(KeyCode(0x68 + function - 13));
                }
            }
            if let Some(digit) = code
                .strip_prefix("Numpad")
                .and_then(|value| value.parse::<u16>().ok())
                && digit <= 9
            {
                return Some(KeyCode(if digit == 0 { 0x62 } else { 0x58 + digit }));
            }
            return None;
        }
    };
    Some(KeyCode(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn view(id: UserId, owned_by_recipient: bool) -> UserView {
        UserView {
            id,
            owned_by_recipient,
            name: "Observed".into(),
            source: mux_core::application_event::SourceId(id.local),
            pointer: None,
            cursor: None,
            held_keys: None,
        }
    }

    #[test]
    fn visible_foreign_users_need_explicit_delegation_for_control_selection() {
        let id = UserId { epoch: 1, local: 1 };
        let mut user = view(id, false);
        let mut grant = ControlGrant {
            users: UserScope::AllOwned,
            pointer_input: true,
            ..ControlGrant::default()
        };
        assert!(!selects(&grant, &user));
        grant.delegated_users = true;
        assert!(!selects(&grant, &user));
        grant.users = UserScope::Only(BTreeSet::from([id]));
        assert!(selects(&grant, &user));
        grant.delegated_users = false;
        assert!(!selects(&grant, &user));
        user.owned_by_recipient = true;
        assert!(selects(&grant, &user));
        grant.users = UserScope::AllOwned;
        assert!(selects(&grant, &user));
        grant.users = UserScope::Only(BTreeSet::new());
        assert!(!selects(&grant, &user));
    }

    #[test]
    fn releases_are_balanced_scoped_and_never_replayed() {
        let a = UserId { epoch: 1, local: 1 };
        let b = UserId { epoch: 1, local: 2 };
        let users = BTreeMap::from([(a, view(a, true)), (b, view(b, false))]);
        let mut local = LocalInput::default();
        local.record(
            a,
            Input::Button {
                button: MouseButton::X2,
                transition: Transition::Down,
            },
        );
        local.record(
            b,
            Input::Key {
                key: KeyCode(4),
                transition: Transition::Down,
            },
        );
        let grant = ControlGrant {
            users: UserScope::Only(BTreeSet::from([a])),
            pointer_input: true,
            ..ControlGrant::default()
        };
        assert_eq!(
            local.drain_releases(Some(&grant), &users),
            vec![ClientCommand::Input {
                user: a,
                input: Input::Button {
                    button: MouseButton::X2,
                    transition: Transition::Up
                }
            }]
        );
        assert!(local.drain_releases(Some(&grant), &users).is_empty());
        assert!(local.held_keys().is_empty());
    }

    #[test]
    fn dom_codes_and_all_five_buttons_map_without_text() {
        assert_eq!(hid_key("KeyZ"), Some(KeyCode(0x1d)));
        assert_eq!(hid_key("Digit0"), Some(KeyCode(0x27)));
        assert_eq!(hid_key("ShiftRight"), Some(KeyCode(0xe5)));
        assert_eq!(hid_key("F24"), Some(KeyCode(0x73)));
        assert_eq!(hid_key("Unidentified"), None);
        assert!((0..5).all(|button| mouse_button(button).is_some()));
        assert_eq!(mouse_button(5), None);
    }
}
