use super::{Error, RecipientId};
use crate::desktop::Bounds;
use serde::{Deserialize, Serialize};

/// Opaque session-local token; never a native window handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WindowId(pub(crate) u64);

/// Initial backend-observed properties supplied by the trusted host, not guesses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowObservation {
    pub bounds: Bounds,
    pub visible: bool,
    pub minimized: bool,
    pub maximized: bool,
    pub click_through: bool,
    pub enabled: bool,
    pub topmost: bool,
    pub opacity: u8,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInfo {
    pub id: WindowId,
    pub bounds: Bounds,
    pub visible: bool,
    pub minimized: bool,
    pub maximized: bool,
    pub click_through: bool,
    pub enabled: bool,
    pub topmost: bool,
    pub opacity: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WindowUpdate {
    Tracked { window: WindowInfo },
    Changed { window: WindowInfo },
    Untracked { window: WindowId },
    Destroyed { window: WindowId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowAction {
    Show,
    Hide,
    Minimize,
    Maximize,
    Restore,
    Foreground,
    Focus,
    Raise,
    Enable,
    Disable,
    Close,
    MoveResize,
    Opacity,
    Topmost,
    ClickThrough,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum WindowControl {
    Show,
    Hide,
    Minimize,
    Maximize,
    Restore,
    Foreground,
    Focus,
    Raise,
    Enable,
    Disable,
    Close,
    MoveResize(Bounds),
    Opacity(u8),
    Topmost(bool),
    ClickThrough(bool),
}

impl WindowControl {
    pub fn action(&self) -> WindowAction {
        match self {
            Self::Show => WindowAction::Show,
            Self::Hide => WindowAction::Hide,
            Self::Minimize => WindowAction::Minimize,
            Self::Maximize => WindowAction::Maximize,
            Self::Restore => WindowAction::Restore,
            Self::Foreground => WindowAction::Foreground,
            Self::Focus => WindowAction::Focus,
            Self::Raise => WindowAction::Raise,
            Self::Enable => WindowAction::Enable,
            Self::Disable => WindowAction::Disable,
            Self::Close => WindowAction::Close,
            Self::MoveResize(_) => WindowAction::MoveResize,
            Self::Opacity(_) => WindowAction::Opacity,
            Self::Topmost(_) => WindowAction::Topmost,
            Self::ClickThrough(_) => WindowAction::ClickThrough,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowAcknowledgement {
    pub window: WindowId,
    pub action: WindowAction,
    /// Backend-observed state only. A close request may be accepted before destruction.
    pub observed: WindowInfo,
}

/// Implementors map only engine-registered owned windows to native handles privately.
/// The host must verify same-process ownership during registration. Neither this
/// trait nor its wire types accepts arbitrary HWNDs or process IDs. It must return
/// observed state, not manufacture success by updating the requested property.
pub trait OwnWindowAdapter {
    fn supported(&self, window: WindowId, action: WindowAction) -> bool;
    fn apply(
        &mut self,
        owner: RecipientId,
        current: &WindowInfo,
        control: &WindowControl,
    ) -> Result<WindowInfo, Error>;
}

/// Safe default for embeddings without a wired backend.
pub struct UnavailableWindowAdapter;

impl OwnWindowAdapter for UnavailableWindowAdapter {
    fn supported(&self, _window: WindowId, _action: WindowAction) -> bool {
        false
    }

    fn apply(
        &mut self,
        _owner: RecipientId,
        _current: &WindowInfo,
        _control: &WindowControl,
    ) -> Result<WindowInfo, Error> {
        Err(Error::Unsupported("own-window backend unavailable"))
    }
}
