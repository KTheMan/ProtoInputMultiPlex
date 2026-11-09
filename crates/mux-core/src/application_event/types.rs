use crate::desktop::{Bounds, CursorShape, MouseButton};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_USERS: usize = 64;
pub const MAX_RECIPIENTS: usize = 64;
pub const MAX_SUBSCRIBERS: usize = 64;
pub const MAX_QUEUE: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct UserId {
    pub epoch: u64,
    pub local: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RecipientId(pub(crate) u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SubscriptionId(pub(crate) u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SourceId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transition {
    Down,
    Up,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WheelAxis {
    Vertical,
    Horizontal,
}

/// USB HID Keyboard/Keypad usage on page 0x07. No key text or native handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct KeyCode(pub u16);

impl KeyCode {
    pub fn valid(self) -> bool {
        (0x04..=0xa4).contains(&self.0) || (0xe0..=0xe7).contains(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Input {
    Motion {
        position: Point,
    },
    Button {
        button: MouseButton,
        transition: Transition,
    },
    /// Signed units, with 120 units per detent; zero is rejected.
    Wheel {
        axis: WheelAxis,
        units: i32,
    },
    Key {
        key: KeyCode,
        transition: Transition,
    },
}

impl Input {
    pub fn is_key(self) -> bool {
        matches!(self, Self::Key { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointerState {
    pub position: Point,
    /// Button identities held at the snapshot boundary; order has no meaning.
    pub held_buttons: Vec<MouseButton>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CursorState {
    pub visible: bool,
    pub shape: CursorShape,
    pub size: u16,
    pub color: [u8; 4],
    pub movement_locked: bool,
    pub type_locked: bool,
}

impl Default for CursorState {
    fn default() -> Self {
        Self {
            visible: true,
            shape: CursorShape::Arrow,
            size: 28,
            color: [40, 150, 255, 255],
            movement_locked: false,
            type_locked: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum CursorControl {
    Visible(bool),
    Shape(CursorShape),
    Size(u16),
    Color([u8; 4]),
    MovementLocked(bool),
    TypeLocked(bool),
    /// Reserved surface: no renderer theme assets are implemented.
    Theme(String),
    /// Reserved surface: only zero flags are supported.
    Flags(u32),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop: Option<super::DesktopUserState>,
    pub id: UserId,
    /// Recipient-relative ownership hint for control visibility. Server-side
    /// authorization always checks the actual owner independently of this hint.
    #[serde(default)]
    pub owned_by_recipient: bool,
    pub name: String,
    pub source: SourceId,
    pub pointer: Option<PointerState>,
    pub cursor: Option<CursorState>,
    /// Omitted when this recipient lacks the distinct key-event grant.
    pub held_keys: Option<Vec<KeyCode>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "users", rename_all = "snake_case")]
pub enum UserScope {
    /// Only users actually owned by this recipient, including future creations.
    AllOwned,
    Only(BTreeSet<UserId>),
}

/// Host approval, independent of MCP access or command credentials. Default is off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    #[serde(default, skip_serializing_if = "is_false")]
    pub host_state: bool,
    pub users: UserScope,
    pub pointer_events: bool,
    pub key_events: bool,
    pub owned_windows: bool,
}

impl Default for Grant {
    fn default() -> Self {
        Self {
            host_state: false,
            users: UserScope::Only(BTreeSet::new()),
            pointer_events: false,
            key_events: false,
            owned_windows: false,
        }
    }
}

impl Grant {
    pub fn enabled(&self) -> bool {
        self.pointer_events || self.key_events || self.owned_windows || self.host_state
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    DesktopObservationV1,
    CooperativeVirtualInput,
    PortableCursorState,
    OwnWindowAdapter,
    NativeRecipientTransport,
    BrowserTransport,
    SystemCursor,
    CursorThemes,
    CursorFlags,
    HostVisibility,
    HostedRouting,
    TargetInjection,
    PhysicalInput,
    Pen,
    Touch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityStatus {
    pub capability: Capability,
    pub available: bool,
    pub detail: String,
}

pub fn capabilities() -> Vec<CapabilityStatus> {
    use Capability::*;
    [
        (
            CooperativeVirtualInput,
            true,
            "Embedded virtual users and direct event hub",
        ),
        (
            PortableCursorState,
            true,
            "Consumer-rendered arrow, crosshair, I-beam and ring state",
        ),
        (
            OwnWindowAdapter,
            true,
            "Abstract scoped contract; backend support queried separately",
        ),
        (
            NativeRecipientTransport,
            false,
            "Native recipient transport not integrated",
        ),
        (
            BrowserTransport,
            false,
            "Rust/WASM client and browser transport not integrated",
        ),
        (
            SystemCursor,
            false,
            "System cursor pixels unavailable to this portable engine",
        ),
        (CursorThemes, false, "Theme renderer unavailable"),
        (CursorFlags, false, "Nonzero cursor flags unavailable"),
        (HostVisibility, false, "Host UI adapter unavailable"),
        (
            TargetInjection,
            false,
            "Cooperative engine never injects target input",
        ),
        (PhysicalInput, false, "No physical device provider"),
        (Pen, false, "No pen source"),
        (Touch, false, "No touch source"),
    ]
    .into_iter()
    .map(|(capability, available, detail)| CapabilityStatus {
        capability,
        available,
        detail: detail.into(),
    })
    .collect()
}

/// Screen coordinates are desktop physical pixels, with a caller supplied revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenState {
    pub revision: u64,
    pub desktop: Bounds,
    pub screens: Vec<Bounds>,
}

impl ScreenState {
    pub fn validate(&self) -> Result<(), Error> {
        if self.revision == 0
            || !self.desktop.valid_region()
            || self.screens.is_empty()
            || self.screens.len() > 64
            || self.screens.iter().any(|bounds| {
                !bounds.valid_region()
                    || bounds.left < self.desktop.left
                    || bounds.top < self.desktop.top
                    || bounds.right > self.desktop.right
                    || bounds.bottom > self.desktop.bottom
            })
        {
            return Err(Error::Invalid("screen bounds or revision"));
        }
        Ok(())
    }
}

/// Explicit screen rectangle to canvas-content coordinates. DOM glue supplies the
/// screen rectangle and backing pixel extent; mixed-DPI discovery is not inferred.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanvasTransform {
    pub revision: u64,
    pub screen: Bounds,
    pub width: f64,
    pub height: f64,
}

impl CanvasTransform {
    fn validate(&self, revision: u64) -> Result<(), Error> {
        if revision != self.revision {
            return Err(Error::StaleRevision);
        }
        if self.revision == 0
            || !self.screen.valid_region()
            || !self.width.is_finite()
            || !self.height.is_finite()
            || self.width <= 0.0
            || self.height <= 0.0
            || self.width > 1_000_000.0
            || self.height > 1_000_000.0
        {
            return Err(Error::Invalid("canvas transform"));
        }
        Ok(())
    }

    pub fn to_canvas(&self, point: Point, revision: u64) -> Result<[f64; 2], Error> {
        self.validate(revision)?;
        Ok([
            (f64::from(point.x) - f64::from(self.screen.left)) * self.width
                / (f64::from(self.screen.right) - f64::from(self.screen.left)),
            (f64::from(point.y) - f64::from(self.screen.top)) * self.height
                / (f64::from(self.screen.bottom) - f64::from(self.screen.top)),
        ])
    }

    pub fn to_screen(&self, point: [f64; 2], revision: u64) -> Result<Point, Error> {
        self.validate(revision)?;
        if point.iter().any(|value| !value.is_finite()) {
            return Err(Error::Invalid("canvas point"));
        }
        let x = f64::from(self.screen.left)
            + point[0] / self.width * (f64::from(self.screen.right) - f64::from(self.screen.left));
        let y = f64::from(self.screen.top)
            + point[1] / self.height * (f64::from(self.screen.bottom) - f64::from(self.screen.top));
        if x < f64::from(i32::MIN)
            || x > f64::from(i32::MAX)
            || y < f64::from(i32::MIN)
            || y > f64::from(i32::MAX)
        {
            return Err(Error::Invalid(
                "canvas point exceeds screen coordinate range",
            ));
        }
        Ok(Point {
            x: x.round() as i32,
            y: y.round() as i32,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    DesktopObserved { user: UserId, source: SourceId, record: super::DesktopRecordV1, position: Option<Point> },
    HostedStateChanged {
        state: HostedState,
    },
    UserCreated {
        user: UserView,
    },
    UserUpdated {
        user: UserView,
    },
    UserDisposed {
        user: UserId,
    },
    Input {
        user: UserId,
        source: SourceId,
        input: Input,
        position: Option<Point>,
        transform_revision: u64,
    },
    ScreensChanged {
        screens: ScreenState,
    },
    Window {
        update: super::WindowUpdate,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub version: u32,
    pub epoch: u64,
    pub sequence: u64,
    pub event: Event,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosted: Option<HostedState>,
    pub version: u32,
    pub epoch: u64,
    /// All queued events in this subscription have a greater sequence.
    pub through_sequence: u64,
    pub users: Vec<UserView>,
    pub screens: ScreenState,
    pub windows: Vec<super::WindowInfo>,
    pub capabilities: Vec<CapabilityStatus>,
    pub mode: ApplicationMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    Revoked,
    GrantChanged,
    RecipientDisconnected,
    Reset,
    SessionEnded,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Poll {
    Events {
        events: Vec<Envelope>,
    },
    Gap {
        after_sequence: u64,
        lost_through: u64,
    },
    Ended {
        reason: EndReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriberStats {
    pub capacity: usize,
    pub queued: usize,
    pub delivered: u64,
    pub overflowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Invalid(&'static str),
    Denied,
    MissingUser,
    MissingRecipient,
    MissingSubscription,
    MissingWindow,
    Limit,
    Ended,
    Locked,
    InvalidTransition,
    StaleRevision,
    Unsupported(&'static str),
    Adapter(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationMode {
    Cooperative,
    TargetInjection,
    DesktopIsolation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostControl {
    Show,
    Hide,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostedMode {
    Native,
    Switched,
    Concurrent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostedPhase {
    Native,
    Starting,
    Ready,
    Assigned,
    Stopping,
    Quarantined,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedState {
    pub revision: u64,
    pub routing_generation: u64,
    pub phase: HostedPhase,
    pub mode: Option<HostedMode>,
    pub multi_keyboard: Option<bool>,
    pub minimized: Option<bool>,
}

impl HostedState {
    pub fn validate(&self) -> Result<(), Error> {
        if self.revision == 0 {
            return Err(Error::Invalid("nonzero hosted revision required"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostRevision {
    pub epoch: u64,
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostViewportAction {
    Minimize,
    Restore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum HostedAction {
    SetMode(HostedMode),
    Viewport(HostViewportAction),
}
