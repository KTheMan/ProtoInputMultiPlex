//! Portable per-seat output rules for virtual input conversion.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::emulated::Packet;

const MAX_KEY_RULES: usize = 255;

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum MouseOutput {
    /// Retain the existing `CursorSettings.button_mapping` behavior.
    #[default]
    Legacy,
    Key {
        key: u16,
    },
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum KeyOutput {
    Key { key: u16 },
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// Source order: primary, secondary, middle, X1, X2.
    pub buttons: [MouseOutput; 5],
    /// Source virtual key to output virtual key or disabled state.
    #[serde(deserialize_with = "deserialize_keys")]
    pub keys: BTreeMap<u16, KeyOutput>,
}

// Tagged commands buffer their payload before deserializing it. That buffer
// retains JSON object keys as strings, so use the same explicit key decoder
// for direct profiles and buffered worker/MCP requests.
fn deserialize_keys<'de, D>(deserializer: D) -> Result<BTreeMap<u16, KeyOutput>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{Error, MapAccess, Visitor};
    struct Keys;
    impl<'de> Visitor<'de> for Keys {
        type Value = BTreeMap<u16, KeyOutput>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("an object of canonical decimal virtual-key codes")
        }
        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
            let mut keys = BTreeMap::new();
            while let Some(source) = map.next_key::<String>()? {
                let key = source.parse::<u16>().map_err(M::Error::custom)?;
                if source != key.to_string() {
                    return Err(M::Error::custom(
                        "key codes must use canonical decimal strings",
                    ));
                }
                if keys.contains_key(&key) {
                    return Err(M::Error::custom("duplicate source key"));
                }
                if keys.len() >= MAX_KEY_RULES {
                    return Err(M::Error::custom("too many keyboard output rules"));
                }
                keys.insert(key, map.next_value()?);
            }
            Ok(keys)
        }
    }
    deserializer.deserialize_map(Keys)
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            buttons: [MouseOutput::Legacy; 5],
            keys: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    /// Let the existing router path handle this packet.
    Passthrough,
    /// Consume the source packet without emitting an output packet.
    Disabled,
    /// Emit a keyboard packet for this virtual key.
    Key(u16),
}

impl Settings {
    pub fn validate(&self) -> Result<()> {
        for output in self.buttons {
            if let MouseOutput::Key { key } = output {
                validate_key(key)?;
            }
        }
        ensure!(
            self.keys.len() <= MAX_KEY_RULES,
            "keyboard output map exceeds {MAX_KEY_RULES} rules"
        );
        for (source, output) in &self.keys {
            validate_key(*source)?;
            if let KeyOutput::Key { key } = output {
                validate_key(*key)?;
            }
        }
        Ok(())
    }

    pub fn requires_keyboard(&self) -> bool {
        self.buttons
            .iter()
            .any(|output| matches!(output, MouseOutput::Key { .. }))
            || self
                .keys
                .values()
                .any(|output| matches!(output, KeyOutput::Key { .. }))
    }

    /// Selects conversion from the original source packet, before mouse remapping.
    pub fn selected(&self, packet: Packet) -> Selection {
        if packet.kind == 0 {
            let button = match packet.flags {
                0x001 | 0x002 => 0,
                0x004 | 0x008 => 1,
                0x010 | 0x020 => 2,
                0x040 | 0x080 => 3,
                0x100 | 0x200 => 4,
                _ => return Selection::Passthrough,
            };
            return match self.buttons[button] {
                MouseOutput::Legacy => Selection::Passthrough,
                MouseOutput::Key { key } => Selection::Key(key),
                MouseOutput::Disabled => Selection::Disabled,
            };
        }
        if packet.kind == 1
            && let Some(output) = self.keys.get(&(packet.data as u16))
        {
            return match output {
                KeyOutput::Key { key } => Selection::Key(*key),
                KeyOutput::Disabled => Selection::Disabled,
            };
        }
        Selection::Passthrough
    }

    /// Reports whether a held input's effective mapping changes between two settings values.
    pub fn source_changed(&self, new: &Self, kind: u32, code: u32) -> bool {
        match kind {
            0 if code < 5 => self.buttons[code as usize] != new.buttons[code as usize],
            1 if code <= u32::from(u16::MAX) => {
                let key = code as u16;
                let effective = |settings: &Self| match settings.keys.get(&key) {
                    Some(KeyOutput::Key { key }) => Some(*key),
                    Some(KeyOutput::Disabled) => None,
                    None => Some(key),
                };
                effective(self) != effective(new)
            }
            _ => false,
        }
    }
}

fn validate_key(key: u16) -> Result<()> {
    ensure!(
        (8..=254).contains(&key),
        "virtual keyboard codes must be 8-254"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mouse(button_flags: u32, released: bool) -> Packet {
        let flag = if released {
            button_flags << 1
        } else {
            button_flags
        };
        Packet {
            device: 0xF1000002,
            kind: 0,
            flags: flag,
            x: 0,
            y: 0,
            data: 0,
        }
    }

    fn keyboard(key: u32, released: bool) -> Packet {
        Packet {
            device: 0xF1000003,
            kind: 1,
            flags: u32::from(released),
            x: 0,
            y: 0,
            data: key,
        }
    }

    #[test]
    fn defaults_preserve_legacy_mouse_behavior_and_allow_shared_key_outputs() {
        let settings = Settings::default();
        assert_eq!(settings.buttons, [MouseOutput::Legacy; 5]);
        assert_eq!(
            settings.selected(mouse(0x001, false)),
            Selection::Passthrough
        );
        assert!(!settings.requires_keyboard());

        let shared = Settings {
            buttons: [
                MouseOutput::Key { key: 65 },
                MouseOutput::Key { key: 65 },
                MouseOutput::Legacy,
                MouseOutput::Disabled,
                MouseOutput::Legacy,
            ],
            ..Default::default()
        };
        assert!(shared.validate().is_ok());
        assert!(shared.requires_keyboard());
    }

    #[test]
    fn button_down_and_up_select_the_same_key_and_non_buttons_bypass_rules() {
        let settings = Settings {
            buttons: [MouseOutput::Key { key: 65 }; 5],
            keys: BTreeMap::from([(66, KeyOutput::Key { key: 67 })]),
        };
        assert_eq!(settings.selected(mouse(0x001, false)), Selection::Key(65));
        assert_eq!(settings.selected(mouse(0x001, true)), Selection::Key(65));
        assert_eq!(
            settings.selected(mouse(0x400, false)),
            Selection::Passthrough
        );
        assert_eq!(settings.selected(keyboard(66, false)), Selection::Key(67));
        assert_eq!(settings.selected(keyboard(66, true)), Selection::Key(67));
        assert_eq!(
            settings.selected(keyboard(65, false)),
            Selection::Passthrough
        );
        assert_eq!(
            settings.selected(Packet::text_unit(0xF1000003, b'A' as u16)),
            Selection::Passthrough
        );
    }

    #[test]
    fn validation_bounds_virtual_keys_and_source_change_detection_is_precise() {
        let old = Settings::default();
        let changed = Settings {
            buttons: [MouseOutput::Key { key: 65 }; 5],
            keys: BTreeMap::from([(66, KeyOutput::Disabled)]),
        };
        assert!(changed.validate().is_ok());
        assert!(old.source_changed(&changed, 0, 2));
        assert!(old.source_changed(&changed, 1, 66));
        assert!(!old.source_changed(&changed, 1, 65));
        assert!(!old.source_changed(
            &Settings {
                keys: BTreeMap::from([(65, KeyOutput::Key { key: 65 })]),
                ..Default::default()
            },
            1,
            65
        ));

        assert!(
            Settings {
                buttons: [MouseOutput::Key { key: 0 }; 5],
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Settings {
                keys: BTreeMap::from([(256, KeyOutput::Disabled)]),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Settings {
                keys: BTreeMap::from([(1, KeyOutput::Key { key: 256 })]),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Settings {
                keys: (1..=256).map(|key| (key, KeyOutput::Disabled)).collect(),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
}
