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
    /// Optional facts read by the trusted host. Missing facts remain unavailable.
    #[serde(default)]
    pub metadata: WindowMetadata,
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
    #[serde(default)]
    pub metadata: WindowMetadata,
    pub visible: bool,
    pub minimized: bool,
    pub maximized: bool,
    pub click_through: bool,
    pub enabled: bool,
    pub topmost: bool,
    pub opacity: u8,
}

/// Observed facts about a registered, owned window. Coordinates are physical
/// desktop pixels; IDs are informational and never authorize registration.
/// No native window or monitor handles cross this boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowMetadata {
    pub title: Option<String>,
    pub process_id: Option<u32>,
    pub ui_thread_id: Option<u32>,
    pub client_bounds: Option<Bounds>,
    pub dpi: Option<u32>,
    pub monitor_bounds: Option<Bounds>,
    pub monitor_work_area: Option<Bounds>,
}

impl WindowMetadata {
    /// Validate facts supplied by a host or received in an SDK snapshot/event.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self
            .title
            .as_ref()
            .is_some_and(|title| title.len() > 512 || title.contains('\0'))
        {
            return Err("window title");
        }
        if self.process_id == Some(0) || self.ui_thread_id == Some(0) {
            return Err("window owner identity");
        }
        if self
            .client_bounds
            .is_some_and(|bounds| {
                let limit = -1_000_000..=1_000_000;
                !limit.contains(&bounds.left)
                    || !limit.contains(&bounds.top)
                    || !limit.contains(&bounds.right)
                    || !limit.contains(&bounds.bottom)
                    || bounds.right < bounds.left
                    || bounds.bottom < bounds.top
            })
        {
            return Err("window client bounds");
        }
        if self.dpi.is_some_and(|dpi| !(1..=4096).contains(&dpi)) {
            return Err("window dpi");
        }
        if self
            .monitor_bounds
            .is_some_and(|bounds| !bounds.valid_region())
        {
            return Err("window monitor bounds");
        }
        if let Some(work) = self.monitor_work_area {
            let Some(monitor) = self.monitor_bounds else {
                return Err("window monitor work area");
            };
            if !work.valid_region()
                || work.left < monitor.left
                || work.top < monitor.top
                || work.right > monitor.right
                || work.bottom > monitor.bottom
            {
                return Err("window monitor work area");
            }
        }
        Ok(())
    }
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
