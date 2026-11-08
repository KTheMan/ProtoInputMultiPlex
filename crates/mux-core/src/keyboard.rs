//! Per-user virtual keyboard source and recipient policy.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum KeyboardFocus {
    /// Preserve the exact clicked recipient used by existing profiles.
    #[default]
    Clicked,
    Focused,
    Native,
    Active,
    Caret,
}

/// Application API behavior, separate from the Rust keyboard recipient selector.
/// Native controls leave focus, capture and activation decisions to the application.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationFocus {
    /// Preserve existing ProtoInput focus hooks, notices and activation filters.
    #[default]
    Compatibility,
    /// Opt in before starting automatic desktop routing; requires native validation.
    NativeControls,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct KeyboardSettings {
    /// Disable the virtual keyboard source before recording, shortcuts and replication.
    pub enabled: bool,
    pub focus: KeyboardFocus,
    pub application_focus: ApplicationFocus,
}

impl Default for KeyboardSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            focus: KeyboardFocus::Clicked,
            application_focus: ApplicationFocus::Compatibility,
        }
    }
}

impl KeyboardSettings {
    pub fn validate_application_route(self, automatic: bool) -> Result<()> {
        ensure!(
            self.application_focus == ApplicationFocus::Compatibility || automatic,
            "Native control focus requires automatic desktop routing"
        );
        Ok(())
    }

    /// Run before interrupting automation, releasing input or changing settings.
    /// Application focus is pinned for the worker lifetime, including pause.
    pub fn validate_live_update(self, previous: Self) -> Result<()> {
        ensure!(
            self.application_focus == previous.application_focus,
            "Application focus cannot change during routing; stop and restart the session"
        );
        Ok(())
    }

    pub fn recommended() -> Self {
        Self {
            focus: KeyboardFocus::Focused,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_focus_requires_automatic_routing_and_a_new_session() {
        let legacy: KeyboardSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy.application_focus, ApplicationFocus::Compatibility);
        assert!(legacy.validate_application_route(false).is_ok());
        let native: KeyboardSettings =
            serde_json::from_str(r#"{"application_focus":"native_controls","focus":"focused"}"#)
                .unwrap();
        assert!(native.validate_application_route(false).is_err());
        assert!(native.validate_application_route(true).is_ok());
        assert!(native.validate_live_update(legacy).is_err());
        assert!(legacy.validate_live_update(native).is_err());
        let disabled = KeyboardSettings {
            enabled: false,
            ..native
        };
        assert!(disabled.validate_live_update(native).is_ok());
        assert_eq!(
            native,
            serde_json::from_value(serde_json::to_value(native).unwrap()).unwrap()
        );
        assert!(
            serde_json::from_str::<KeyboardSettings>(r#"{"application_focus":"foreground"}"#,)
                .is_err()
        );
    }

    #[test]
    fn legacy_defaults_and_explicit_keyboard_options_round_trip() {
        let legacy: KeyboardSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy, KeyboardSettings::default());
        assert_eq!(legacy.focus, KeyboardFocus::Clicked);
        for focus in [
            KeyboardFocus::Clicked,
            KeyboardFocus::Focused,
            KeyboardFocus::Native,
            KeyboardFocus::Active,
            KeyboardFocus::Caret,
        ] {
            let configured = KeyboardSettings {
                enabled: false,
                focus,
                ..KeyboardSettings::default()
            };
            assert_eq!(
                configured,
                serde_json::from_value(serde_json::to_value(configured).unwrap()).unwrap()
            );
        }
        assert!(serde_json::from_str::<KeyboardSettings>(r#"{"hardware":true}"#).is_err());
        assert!(serde_json::from_str::<KeyboardSettings>(r#"{"focus":"foreground"}"#).is_err());
    }
}
