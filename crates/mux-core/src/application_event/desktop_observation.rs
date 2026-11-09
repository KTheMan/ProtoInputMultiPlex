//! Versioned virtual-desktop observations. No physical identity or input authority.
use super::{Error, Point, PointerState, ScreenState, Transition, UserView, WheelAxis};
use crate::desktop::{CursorShape, MouseButton};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const DESKTOP_OBSERVATION_VERSION: u32 = 1;
pub const MAX_NATIVE_HELD_KEYS: usize = 247;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanPrefix { None, E0, E1 }

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowsVirtualKey {
    pub virtual_key: u16,
    pub scan_code: u8,
    pub prefix: ScanPrefix,
}
impl WindowsVirtualKey {
    pub fn validate(self) -> Result<(), Error> {
        if !(8..=254).contains(&self.virtual_key) || self.scan_code == 0 {
            return Err(Error::Invalid("native key identity"));
        }
        Ok(())
    }
}

/// Configured presentation only, not effective application bitmap or rendered pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfiguredDesktopCursor {
    pub shape: CursorShape,
    pub size: f32,
    pub color: [u8; 3],
    pub visible: bool,
    pub follow_application: bool,
}
impl ConfiguredDesktopCursor {
    pub fn validate(self) -> Result<(), Error> {
        if !self.size.is_finite() || !(12.0..=256.0).contains(&self.size) {
            return Err(Error::Invalid("configured desktop cursor size"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopConsentV1 {
    pub version: u32,
    pub text_units: bool,
}
impl DesktopConsentV1 {
    pub fn validate(self) -> Result<(), Error> {
        if self.version != DESKTOP_OBSERVATION_VERSION {
            return Err(Error::Invalid("desktop observation version"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopAdmission { LogicalOnly, NativeSubmissionAccepted, SharedNativeHold }

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesktopObservation {
    Motion { position: Point },
    Button { button: MouseButton, transition: Transition },
    Wheel { axis: WheelAxis, units: i32 },
    Key { key: WindowsVirtualKey, transition: Transition },
    /// One UTF-16 code unit, including a surrogate; not a HID key or text string.
    TextUnit { unit: u16 },
    Cursor { cursor: ConfiguredDesktopCursor },
}
impl DesktopObservation {
    pub fn is_key(self) -> bool { matches!(self, Self::Key { .. } | Self::TextUnit { .. }) }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopRecordV1 {
    pub version: u32,
    pub sequence: u64,
    pub screen_revision: u64,
    pub admission: DesktopAdmission,
    pub change: DesktopObservation,
}

/// Trusted worker-atomic ownership baseline; no synthetic edges or past delivery claim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopBaselineV1 {
    pub version: u32,
    pub next_sequence: u64,
    pub screens: ScreenState,
    pub position: Point,
    pub cursor: ConfiguredDesktopCursor,
    pub held_buttons: Vec<MouseButton>,
    pub native_held_keys: Vec<WindowsVirtualKey>,
}
impl DesktopBaselineV1 {
    pub fn validate(&self) -> Result<(), Error> {
        self.screens.validate()?;
        self.cursor.validate()?;
        if self.version != 1 || self.next_sequence == 0
            || !self.screens.desktop.contains(self.position.x, self.position.y)
            || self.held_buttons.len() > 5
            || self.held_buttons.iter().collect::<std::collections::HashSet<_>>().len() != self.held_buttons.len()
        { return Err(Error::Invalid("desktop ownership baseline")); }
        validate_native_keys(&self.native_held_keys)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopProvenance { VirtualDesktopObservation }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopUserState {
    pub version: u32,
    pub provenance: DesktopProvenance,
    pub screen_revision: u64,
    pub last_sequence: u64,
    pub configured_cursor: Option<ConfiguredDesktopCursor>,
    pub native_held_keys: Option<Vec<WindowsVirtualKey>>,
    pub text_units: bool,
}

pub fn validate_native_keys(keys: &[WindowsVirtualKey]) -> Result<(), Error> {
    if keys.len() > MAX_NATIVE_HELD_KEYS { return Err(Error::Limit); }
    let mut seen = BTreeSet::new();
    for key in keys {
        key.validate()?;
        if !seen.insert(key.virtual_key) { return Err(Error::Invalid("duplicate native VK")); }
    }
    Ok(())
}

/// Shared strict folding for provider publication and negotiated consumer state.
/// The caller supplies the authenticated current screen revision/bounds.
pub fn fold_desktop_observation(view: &mut UserView, record: DesktopRecordV1, screens: &ScreenState) -> Result<(), Error> {
    let desktop = view.desktop.as_mut().ok_or(Error::Denied)?;
    if record.version != 1 || desktop.version != 1 || record.sequence <= desktop.last_sequence
        || record.screen_revision != screens.revision || desktop.screen_revision != screens.revision
    { return Err(Error::StaleRevision); }
    match record.change {
        DesktopObservation::Motion { position } => {
            if !screens.desktop.contains(position.x, position.y) { return Err(Error::Invalid("desktop position")); }
            view.pointer.as_mut().ok_or(Error::Denied)?.position = position;
        }
        DesktopObservation::Button { button, transition } => {
            let held = &mut view.pointer.as_mut().ok_or(Error::Denied)?.held_buttons;
            let old = held.iter().position(|value| *value == button);
            match (transition, old) {
                (Transition::Down, None) if held.len() < 5 => held.push(button),
                (Transition::Up, Some(index)) => { held.remove(index); },
                _ => return Err(Error::InvalidTransition),
            }
        }
        DesktopObservation::Wheel { units, .. } => {
            if view.pointer.is_none() || units == 0 || i16::try_from(units).is_err() { return Err(Error::Invalid("desktop wheel")); }
        }
        DesktopObservation::Key { key, transition } => {
            key.validate()?;
            let keys = desktop.native_held_keys.as_mut().ok_or(Error::Denied)?;
            let old = keys.iter().position(|value| value.virtual_key == key.virtual_key);
            match (transition, old) {
                (Transition::Down, None) if keys.len() < MAX_NATIVE_HELD_KEYS => keys.push(key),
                (Transition::Up, Some(index)) if keys[index] == key => { keys.remove(index); },
                _ => return Err(Error::InvalidTransition),
            }
        }
        DesktopObservation::TextUnit { .. } if !desktop.text_units => return Err(Error::Denied),
        DesktopObservation::TextUnit { .. } => {},
        DesktopObservation::Cursor { cursor } => {
            cursor.validate()?;
            if view.pointer.is_none() || desktop.configured_cursor.is_none() { return Err(Error::Denied); }
            desktop.configured_cursor = Some(cursor);
        }
    }
    desktop.last_sequence = record.sequence;
    Ok(())
}

pub(crate) fn baseline_view(baseline: &DesktopBaselineV1) -> (PointerState, DesktopUserState) {
    (PointerState { position: baseline.position, held_buttons: baseline.held_buttons.clone() }, DesktopUserState {
        version: 1, provenance: DesktopProvenance::VirtualDesktopObservation,
        screen_revision: baseline.screens.revision, last_sequence: baseline.next_sequence - 1,
        configured_cursor: Some(baseline.cursor), native_held_keys: Some(baseline.native_held_keys.clone()),
        text_units: true,
    })
}
