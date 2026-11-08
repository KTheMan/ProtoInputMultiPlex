//! Per-user output-rule resolution shared by live input and supervised automation.
use anyhow::{Context, Result, ensure};
use mux_core::{
    Profile, ResolvedSeat,
    emulated::{BASE, MASK, Packet},
    output::{self, Selection},
};
use rmcp::schemars;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Inspect,
    Configure {
        user: usize,
        settings: output::Settings,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub acknowledged: bool,
    pub user: Option<usize>,
    pub users: Vec<UserStatus>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserStatus {
    pub user: usize,
    pub active: bool,
    pub settings: output::Settings,
}

#[derive(Clone)]
struct UserRules {
    resolved: ResolvedSeat,
    settings: output::Settings,
    active: bool,
}

#[derive(Clone)]
pub struct Rules {
    users: Vec<UserRules>,
}

impl Rules {
    pub fn new(profile: &Profile, seats: Vec<ResolvedSeat>) -> Result<Self> {
        ensure!(
            profile.seats.len() == seats.len(),
            "output rules and resolved seat counts differ"
        );
        let users = profile
            .seats
            .iter()
            .zip(seats)
            .map(|(seat, resolved)| UserRules {
                resolved,
                settings: seat.output.clone(),
                active: true,
            })
            .collect::<Vec<_>>();
        let rules = Self { users };
        for user in 0..rules.users.len() {
            rules.validate(user, &rules.users[user].settings)?;
        }
        Ok(rules)
    }

    pub fn defaults(seats: Vec<ResolvedSeat>) -> Self {
        Self {
            users: seats
                .into_iter()
                .map(|resolved| UserRules {
                    resolved,
                    settings: output::Settings::default(),
                    active: true,
                })
                .collect(),
        }
    }

    pub fn state(&self) -> State {
        State {
            acknowledged: false,
            user: None,
            users: self
                .users
                .iter()
                .enumerate()
                .map(|(user, rules)| UserStatus {
                    user,
                    active: rules.active,
                    settings: rules.settings.clone(),
                })
                .collect(),
            error: None,
        }
    }

    pub fn user(&self, packet: Packet) -> Result<usize> {
        packet.validate()?;
        let user = self
            .users
            .iter()
            .position(|rules| {
                rules.active
                    && if packet.kind == 0 {
                        rules.resolved.mouse == packet.device
                    } else {
                        rules.resolved.keyboard == Some(packet.device)
                    }
            })
            .context("packet device is not assigned to an active user")?;
        Ok(user)
    }

    /// Resolves a raw virtual packet to its configured destination packet.
    /// `None` means the source event is explicitly disabled.
    pub fn translate(&self, packet: Packet) -> Result<Option<Packet>> {
        packet.validate()?;
        let user = self.user(packet)?;
        let rules = &self.users[user];
        match rules.settings.selected(packet) {
            Selection::Passthrough => Ok(Some(packet)),
            Selection::Disabled => Ok(None),
            Selection::Key(key) => {
                if packet.kind == 1 && packet.data == u32::from(key) {
                    return Ok(Some(packet));
                }
                let keyboard = rules
                    .resolved
                    .keyboard
                    .context("key output requires an assigned keyboard device")?;
                let released = if packet.kind == 0 {
                    packet.flags.trailing_zeros() % 2 == 1
                } else {
                    packet.flags & 1 != 0
                };
                Ok(Some(mux_windows::keyboard::packet_code(
                    keyboard,
                    u32::from(key),
                    released,
                )?))
            }
        }
    }

    pub fn validate(&self, user: usize, settings: &output::Settings) -> Result<()> {
        let rules = self.users.get(user).context("Unknown output-rules user")?;
        ensure!(
            rules.active,
            "Output rules cannot be changed for a removed user"
        );
        settings.validate()?;
        if settings != &output::Settings::default() {
            ensure!(
                rules.resolved.mouse & MASK == BASE,
                "Output conversion currently requires a virtual mouse user"
            );
        }
        if settings.requires_keyboard() || !settings.keys.is_empty() {
            ensure!(
                rules.resolved.mouse & MASK == BASE,
                "key output requires an assigned virtual mouse"
            );
            ensure!(
                rules
                    .resolved
                    .keyboard
                    .is_some_and(|device| device & MASK == BASE),
                "key output requires an assigned virtual keyboard"
            );
        }
        if let Some(keyboard) = rules.resolved.keyboard {
            validate_keys(settings, keyboard)?;
        }
        Ok(())
    }

    /// Applies an already preflighted candidate. Validation is repeated to keep this API safe.
    pub fn set(&mut self, user: usize, settings: output::Settings) -> Result<()> {
        self.validate(user, &settings)?;
        self.users[user].settings = settings;
        Ok(())
    }

    pub fn prepare_user(&mut self) -> Result<()> {
        self.users.try_reserve(1)?;
        Ok(())
    }

    pub fn append_user(
        &mut self,
        resolved: ResolvedSeat,
        settings: output::Settings,
    ) -> Result<usize> {
        settings.validate()?;
        if settings != output::Settings::default() {
            ensure!(
                resolved.mouse & MASK == BASE,
                "Output conversion currently requires a virtual mouse user"
            );
        }
        if settings.requires_keyboard() || !settings.keys.is_empty() {
            ensure!(
                resolved.mouse & MASK == BASE,
                "key output requires an assigned virtual mouse"
            );
            ensure!(
                resolved
                    .keyboard
                    .is_some_and(|device| device & MASK == BASE),
                "key output requires an assigned virtual keyboard"
            );
        }
        let user = self.users.len();
        self.users.push(UserRules {
            resolved,
            settings,
            active: true,
        });
        Ok(user)
    }

    pub fn commit_remove_user(&mut self, user: usize) {
        let rules = &mut self.users[user];
        rules.active = false;
        rules.settings = output::Settings::default();
    }

    pub fn read_settings(&self, user: usize) -> Result<&output::Settings> {
        let rules = self.users.get(user).context("Unknown output-rules user")?;
        ensure!(rules.active, "Output-rules user has been removed");
        Ok(&rules.settings)
    }

    pub fn resolved(&self, user: usize) -> Result<&ResolvedSeat> {
        let rules = self.users.get(user).context("Unknown output-rules user")?;
        ensure!(rules.active, "Output-rules user has been removed");
        Ok(&rules.resolved)
    }
}

/// Validate destination codes against the existing virtual keyboard packet ABI.
/// This only reads the current keyboard layout; it never submits input.
pub fn validate_keys(settings: &output::Settings, keyboard: u32) -> Result<()> {
    settings.validate()?;
    let keys = settings
        .buttons
        .iter()
        .filter_map(|output| match output {
            output::MouseOutput::Key { key } => Some(*key),
            _ => None,
        })
        .chain(settings.keys.values().filter_map(|output| match output {
            output::KeyOutput::Key { key } => Some(*key),
            _ => None,
        }));
    for key in keys {
        mux_windows::keyboard::packet_code(keyboard, u32::from(key), false).with_context(|| {
            format!("Output key {key} is unavailable in the current keyboard layout")
        })?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use mux_core::output::{KeyOutput, MouseOutput};

    fn virtual_seat(mouse_id: u32, keyboard_id: Option<u32>) -> ResolvedSeat {
        ResolvedSeat {
            name: "Virtual".into(),
            mouse: BASE | (mouse_id << 1),
            keyboard: keyboard_id.map(|id| BASE | (id << 1) | 1),
            target_pid: 0,
        }
    }

    #[test]
    fn physical_default_rules_are_valid_but_key_outputs_need_virtual_mouse_and_keyboard() {
        let physical = ResolvedSeat {
            name: "Physical".into(),
            mouse: 0x1234,
            keyboard: None,
            target_pid: 0,
        };
        let mut rules = Rules::defaults(vec![physical]);
        assert!(rules.validate(0, &output::Settings::default()).is_ok());

        let key_rules = output::Settings {
            buttons: [MouseOutput::Key { key: 65 }; 5],
            keys: std::collections::BTreeMap::from([(66, KeyOutput::Disabled)]),
        };
        assert!(rules.validate(0, &key_rules).is_err());
        assert!(rules.set(0, key_rules).is_err());
        assert!(!rules.read_settings(0).unwrap().requires_keyboard());
    }

    #[test]
    fn text_input_is_resolved_to_its_virtual_user_and_bypasses_key_rules() {
        let seat = virtual_seat(1, Some(1));
        let mut rules = Rules::defaults(vec![seat.clone()]);
        rules
            .set(
                0,
                output::Settings {
                    buttons: [MouseOutput::Disabled; 5],
                    keys: std::collections::BTreeMap::from([(65, KeyOutput::Disabled)]),
                },
            )
            .unwrap();
        let text = Packet::text_unit(seat.keyboard.unwrap(), b'X' as u16);
        assert_eq!(rules.user(text).unwrap(), 0);
        let translated = rules.translate(text).unwrap().unwrap();
        assert_eq!(translated.kind, text.kind);
        assert_eq!(translated.device, text.device);
        assert_eq!(translated.data, text.data);
    }

    #[test]
    fn removed_slots_are_not_resolved_or_used_for_translation() {
        let seat = virtual_seat(1, Some(1));
        let mouse = seat.mouse;
        let mut rules = Rules::defaults(vec![seat]);
        rules.read_settings(0).unwrap();
        rules.commit_remove_user(0);
        assert!(rules.resolved(0).is_err());
        assert!(rules.user(Packet::motion(mouse, 0, 0)).is_err());
        assert!(!rules.state().users[0].active);
    }
}
