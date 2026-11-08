use mux_core::application_event::*;
use mux_core::desktop::MouseButton;
use serde::{Deserialize, Serialize};

pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_POLL_EVENTS: usize = 128;
fn is_false(value: &bool) -> bool {
    !*value
}

/// A second, independently approved authority. Export grants alone never enable
/// mutation. Engine ownership checks still apply to every user/window operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlGrant {
    pub users: UserScope,
    /// Cross-owner control requires Only(user), this explicit host approval, and current export/input consent.
    pub delegated_users: bool,
    pub user_lifecycle: bool,
    pub cursor: bool,
    pub pointer_input: bool,
    pub key_input: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub window_publication: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub window_identity_metadata: bool,
    pub window_tracking: bool,
    pub window_control: bool,
}

impl Default for ControlGrant {
    fn default() -> Self {
        Self {
            users: UserScope::Only(Default::default()),
            delegated_users: false,
            user_lifecycle: false,
            cursor: false,
            pointer_input: false,
            key_input: false,
            window_publication: false,
            window_identity_metadata: false,
            window_tracking: false,
            window_control: false,
        }
    }
}

impl ControlGrant {
    pub(crate) fn selects(&self, user: UserId, recipient: RecipientId, owner: RecipientId) -> bool {
        match &self.users {
            UserScope::AllOwned => owner == recipient,
            UserScope::Only(users) => {
                users.contains(&user) && (owner == recipient || self.delegated_users)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientRequest {
    pub version: u32,
    pub command: ClientCommand,
}

/// A GUI-owned window reference is valid only in its issuing event epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteWindowRef {
    pub epoch: u64,
    pub window: WindowId,
}

/// Recipient identity is deliberately absent. The transport supplies it out of band.
/// There are no host registration, approval, epoch-reset or HWND commands here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientCommand {
    Snapshot,
    Subscribe {
        capacity: usize,
    },
    Poll {
        subscription: SubscriptionId,
        max_events: usize,
    },
    Stats {
        subscription: SubscriptionId,
    },
    Resubscribe {
        subscription: SubscriptionId,
        capacity: usize,
    },
    Unsubscribe {
        subscription: SubscriptionId,
    },
    CreateUser {
        name: String,
    },
    RenameUser {
        user: UserId,
        name: String,
    },
    DisposeUser {
        user: UserId,
    },
    Cursor {
        user: UserId,
        control: CursorControl,
    },
    Input {
        user: UserId,
        input: Input,
    },
    /// Browser/element inputs must reject stale coordinate transforms before mutation.
    InputAtRevision {
        user: UserId,
        input: Input,
        transform_revision: u64,
    },
    Click {
        user: UserId,
        button: MouseButton,
    },
    WindowQuery {
        window: WindowId,
    },
    WindowTrack {
        window: WindowId,
    },
    WindowUntrack {
        window: WindowId,
    },
    WindowControl {
        window: WindowId,
        control: WindowControl,
    },
    RemoteWindowQuery {
        reference: RemoteWindowRef,
    },
    RemoteWindowTrack {
        reference: RemoteWindowRef,
    },
    RemoteWindowUntrack {
        reference: RemoteWindowRef,
    },
    RemoteWindowControl {
        reference: RemoteWindowRef,
        control: WindowControl,
    },
    Disconnect,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientResponse {
    Snapshot {
        snapshot: Snapshot,
    },
    Subscribed {
        subscription: SubscriptionId,
        snapshot: Snapshot,
    },
    Polled {
        poll: Poll,
    },
    Stats {
        stats: SubscriberStats,
    },
    UserCreated {
        user: UserId,
    },
    Cursor {
        cursor: CursorState,
    },
    InputAccepted {
        sequences: Vec<u64>,
    },
    Window {
        window: WindowInfo,
    },
    WindowControlled {
        acknowledgement: WindowAcknowledgement,
    },
    Acknowledged,
    Disconnected,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostRequest {
    pub version: u32,
    pub command: HostCommand,
}

/// Trusted in-process embedding setup only. These are never client commands and
/// the native ABI is not an authentication boundary against code in this process.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostCommand {
    RegisterRecipient {
        local_identity: String,
    },
    CreateUser {
        owner: RecipientId,
        name: String,
    },
    ApproveEvents {
        recipient: RecipientId,
        grant: Grant,
    },
    ApproveControls {
        recipient: RecipientId,
        grant: ControlGrant,
    },
    DelegateWindow {
        window: WindowId,
        recipient: RecipientId,
        access: WindowAccess,
    },
    RegisterOwnWindow {
        recipient: RecipientId,
        native_window: usize,
    },
    RefreshOwnWindow {
        window: WindowId,
    },
    RetireOwnWindow {
        window: WindowId,
    },
    NotifyOwnWindow {
        window: WindowId,
    },
    AcknowledgeWake {
        window: WindowId,
    },
    UpdateScreens {
        screens: ScreenState,
    },
    Reset {
        epoch: u64,
        screens: ScreenState,
    },
    End,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostResponse {
    RecipientRegistered { recipient: RecipientId },
    UserCreated { user: UserId },
    WindowRegistered { window: WindowId, wake_message: u32 },
    Window { window: WindowInfo },
    Acknowledged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    Denied,
    Invalid,
    Missing,
    Limit,
    Ended,
    Locked,
    Transition,
    StaleRevision,
    Unsupported,
    Backend,
    /// The owner may have applied an action whose receipt was lost.
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub code: FailureCode,
    pub message: String,
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        let code = match &error {
            Error::Denied => FailureCode::Denied,
            Error::Invalid(_) => FailureCode::Invalid,
            Error::MissingUser
            | Error::MissingRecipient
            | Error::MissingSubscription
            | Error::MissingWindow => FailureCode::Missing,
            Error::Limit => FailureCode::Limit,
            Error::Ended => FailureCode::Ended,
            Error::Locked => FailureCode::Locked,
            Error::InvalidTransition => FailureCode::Transition,
            Error::StaleRevision => FailureCode::StaleRevision,
            Error::Unsupported(_) => FailureCode::Unsupported,
            Error::Adapter(_) => FailureCode::Backend,
        };
        Self {
            code,
            message: error.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Reply<T> {
    Ok { version: u32, response: T },
    Error { version: u32, error: Failure },
}

impl<T> From<Result<T, Failure>> for Reply<T> {
    fn from(result: Result<T, Failure>) -> Self {
        match result {
            Ok(response) => Self::Ok {
                version: PROTOCOL_VERSION,
                response,
            },
            Err(error) => Self::Error {
                version: PROTOCOL_VERSION,
                error,
            },
        }
    }
}

/// First event-transport frame. Origin is verified from the HTTP upgrade by the
/// server, never asserted here. Hold this event-only credential in transient
/// memory only. A GUI-approved, window-only MCP event client may use its own
/// issued descriptor; stopped Setup and routing MCP tokens cannot substitute.
/// Never put the credential in browser storage, status, or logs.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventHello {
    pub version: u32,
    pub credential: String,
}
impl std::fmt::Debug for EventHello {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EventHello")
            .field("version", &self.version)
            .field("credential", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventLimits {
    pub max_request_bytes: usize,
    pub max_poll_events: usize,
    pub max_queue: usize,
}
impl Default for EventLimits {
    fn default() -> Self {
        Self {
            max_request_bytes: MAX_REQUEST_BYTES,
            max_poll_events: MAX_POLL_EVENTS,
            max_queue: MAX_QUEUE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventReady {
    pub version: u32,
    pub epoch: u64,
    /// Informational opaque identity. The server continues to bind it out of band.
    pub recipient: RecipientId,
    pub events: Grant,
    pub controls: ControlGrant,
    pub capabilities: Vec<CapabilityStatus>,
    pub limits: EventLimits,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientMessage {
    pub request_id: u64,
    pub request: ClientRequest,
}

/// A report from the authenticated native publisher. Tokens identify only
/// bindings within its current lease; they are never Windows handles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerFrame {
    pub request_id: u64,
    pub operation: OwnerOperation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OwnerOperation {
    Register {
        token: u64,
        generation: u64,
        observed: WindowObservation,
        identity_metadata_consent: bool,
    },
    Refresh {
        window: WindowId,
        token: u64,
        generation: u64,
        observed: WindowObservation,
        identity_metadata_consent: bool,
    },
    Retire {
        window: WindowId,
        token: u64,
        generation: u64,
    },
    ApplyResult {
        operation_id: u64,
        window: WindowId,
        token: u64,
        generation: u64,
        result: Reply<WindowObservation>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OwnerResponse {
    Registered { window: WindowId },
    Window { window: WindowInfo },
    Acknowledged,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerMessage {
    Ready {
        ready: EventReady,
    },
    Response {
        request_id: u64,
        reply: Reply<ClientResponse>,
    },
    OwnerResponse {
        request_id: u64,
        reply: Reply<OwnerResponse>,
    },
    /// Dispatched to the native publisher's creating UI thread. The worker
    /// queues this frame and never executes the control.
    WindowApply {
        operation_id: u64,
        window: WindowId,
        token: u64,
        generation: u64,
        control: WindowControl,
    },
    /// Pushed only on activity, gap or terminal state. No browser polling loop.
    EventStream {
        subscription: SubscriptionId,
        stream: Poll,
    },
    Disconnected {
        reason: EndReason,
    },
}
