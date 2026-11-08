//! Window-only MCP consumer of a GUI-issued application-event receiver lease.
//! This router cannot create users, grant itself access, or use stopped Setup.
use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use mux_application_sdk::{
    ClientCommand, ClientMessage, ClientRequest, ClientResponse, ClientState, EventHello,
    EventReady, FailureCode, FoldOutcome, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, PROTOCOL_VERSION,
    RemoteWindowRef, Reply, ServerMessage, UserScope, WindowControl, WindowId, WindowInfo,
    WindowObservationSource,
};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::Read,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{net::TcpStream, sync::Mutex, time::timeout};
use tokio_tungstenite::{
    WebSocketStream, client_async_with_config,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        http::{HeaderValue, Uri},
        protocol::WebSocketConfig,
    },
};

const PATH: &str = "/application-events/v1";
const CALL_TIMEOUT: Duration = Duration::from_secs(6);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Descriptor {
    protoinputmultiplex_application_events: u32,
    endpoint: String,
    exact_origin: String,
    credential: String,
    expires_unix_ms: u64,
}

impl Descriptor {
    fn read(path: &PathBuf) -> Result<(Self, SocketAddr)> {
        let file = std::fs::File::open(path).context("open private event connection")?;
        ensure!(
            file.metadata()?.len() <= 4096,
            "Event connection exceeds its size bound"
        );
        let mut bytes = Vec::with_capacity(4096);
        file.take(4097)
            .read_to_end(&mut bytes)
            .context("read private event connection")?;
        ensure!(
            bytes.len() <= 4096,
            "Event connection exceeds its size bound"
        );
        let descriptor: Self =
            serde_json::from_slice(&bytes).context("invalid event connection")?;
        ensure!(
            descriptor.protoinputmultiplex_application_events == PROTOCOL_VERSION,
            "Event connection version mismatch"
        );
        let port_text = descriptor
            .endpoint
            .strip_prefix("ws://127.0.0.1:")
            .and_then(|value| value.strip_suffix(PATH))
            .context("Event connection must use the local event listener")?;
        let port: u16 = port_text.parse().context("invalid event listener port")?;
        ensure!(
            port != 0 && port.to_string() == port_text,
            "invalid event listener port"
        );
        ensure!(
            descriptor.credential.len() == 64
                && descriptor
                    .credential
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit()),
            "invalid event connection credential"
        );
        let origin: Uri = descriptor
            .exact_origin
            .parse()
            .context("invalid event origin")?;
        let scheme = origin.scheme_str().context("missing event origin scheme")?;
        let authority = origin
            .authority()
            .context("missing event origin authority")?
            .as_str();
        ensure!(
            matches!(scheme, "http" | "https")
                && !authority.is_empty()
                && !authority.contains(['@', '*', '%', '\\'])
                && descriptor.exact_origin == format!("{scheme}://{authority}"),
            "invalid exact event origin"
        );
        HeaderValue::from_str(&descriptor.exact_origin).context("invalid event origin header")?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        ensure!(
            now < u128::from(descriptor.expires_unix_ms),
            "Event connection expired"
        );
        Ok((descriptor, SocketAddr::from((Ipv4Addr::LOCALHOST, port))))
    }
}

struct Peer {
    socket: WebSocketStream<TcpStream>,
    ready: EventReady,
    state: ClientState,
    subscription: Option<mux_application_sdk::SubscriptionId>,
    retired_subscription: Option<mux_application_sdk::SubscriptionId>,
    next_request: u64,
    terminal: bool,
    needs_refresh: bool,
    last_issue: Option<&'static str>,
}

fn validate_reported_window(window: &WindowInfo) -> Result<()> {
    ensure!(window.bounds.valid_region(), "Invalid native-window bounds");
    window
        .metadata
        .validate()
        .map_err(|error| anyhow::anyhow!("Invalid native-window {error}"))?;
    ensure!(
        matches!(
            window.observation.source,
            WindowObservationSource::NativeClientReported
        ),
        "Window observation did not come from the native publisher"
    );
    Ok(())
}

fn window_only_admission(ready: &EventReady) -> bool {
    ready.version == PROTOCOL_VERSION
        && ready.epoch != 0
        && ready.events.owned_windows
        && !ready.controls.window_publication
        && !ready.controls.window_identity_metadata
        && !ready.events.pointer_events
        && !ready.events.key_events
        && !ready.controls.delegated_users
        && !ready.controls.user_lifecycle
        && !ready.controls.cursor
        && !ready.controls.pointer_input
        && !ready.controls.key_input
        && matches!(&ready.events.users, UserScope::Only(users) if users.is_empty())
        && matches!(&ready.controls.users, UserScope::Only(users) if users.is_empty())
}

impl Peer {
    fn fence_protocol(&mut self) {
        self.terminal = true;
        self.last_issue = Some("invalid window response");
        self.state.disconnect();
    }

    async fn connect(path: &PathBuf) -> Result<Self> {
        let (mut descriptor, address) = Descriptor::read(path)?;
        timeout(Duration::from_secs(5), async move {
            let stream = TcpStream::connect(address)
                .await
                .context("connect to event listener")?;
            let mut request = descriptor.endpoint.as_str().into_client_request()?;
            request
                .headers_mut()
                .insert("Origin", HeaderValue::from_str(&descriptor.exact_origin)?);
            let config = WebSocketConfig::default()
                .max_message_size(Some(MAX_RESPONSE_BYTES))
                .max_frame_size(Some(MAX_RESPONSE_BYTES));
            let (mut socket, _) = client_async_with_config(request, stream, Some(config))
                .await
                .context("event listener rejected connection")?;
            let hello = EventHello {
                version: PROTOCOL_VERSION,
                credential: std::mem::take(&mut descriptor.credential),
            };
            socket
                .send(Message::Text(serde_json::to_string(&hello)?.into()))
                .await
                .context("send event admission")?;
            drop(hello);
            drop(descriptor);
            let ServerMessage::Ready { ready } = Self::read_frame(&mut socket).await? else {
                bail!("Event listener did not establish an approved receiver");
            };
            ensure!(
                window_only_admission(&ready),
                "This receiver is not a window-only agent observer"
            );
            let mut peer = Self {
                socket,
                ready,
                state: ClientState::default(),
                subscription: None,
                retired_subscription: None,
                next_request: 0,
                terminal: false,
                needs_refresh: false,
                last_issue: None,
            };
            let ClientResponse::Subscribed {
                subscription,
                snapshot,
            } = peer
                .command(ClientCommand::Subscribe { capacity: 128 }, false)
                .await?
            else {
                bail!("Window subscription was not acknowledged");
            };
            peer.state.install_snapshot(snapshot, peer.ready.epoch)?;
            peer.subscription = Some(subscription);
            Ok(peer)
        })
        .await
        .context("Event connection timed out")?
    }

    async fn read_frame(socket: &mut WebSocketStream<TcpStream>) -> Result<ServerMessage> {
        loop {
            let frame = socket.next().await.context("Event connection ended")??;
            match frame {
                Message::Text(text) if text.len() <= MAX_RESPONSE_BYTES => {
                    return Ok(serde_json::from_str(text.as_str())?);
                }
                Message::Ping(_) | Message::Pong(_) => {}
                Message::Close(_) => bail!("Event authority ended"),
                _ => bail!("Invalid event frame"),
            }
        }
    }

    async fn command(
        &mut self,
        command: ClientCommand,
        action_sent: bool,
    ) -> Result<ClientResponse> {
        ensure!(
            !self.terminal,
            "Event connection ended; inspect status and reconnect through a fresh GUI approval"
        );
        self.next_request = self
            .next_request
            .checked_add(1)
            .context("Request IDs exhausted")?;
        let request_id = self.next_request;
        let frame = serde_json::to_string(&ClientMessage {
            request_id,
            request: ClientRequest {
                version: PROTOCOL_VERSION,
                command,
            },
        })?;
        ensure!(
            frame.len() <= MAX_REQUEST_BYTES,
            "Window request exceeds size bound"
        );
        let mut rejected = false;
        let received = timeout(CALL_TIMEOUT, async {
            // This timeout covers the send as well as the receipt. If it fires,
            // the socket is fenced and never used again: bytes may have left.
            self.socket.send(Message::Text(frame.into())).await?;
            loop {
                match Self::read_frame(&mut self.socket).await? {
                    ServerMessage::Response { request_id: id, reply } if id == request_id => {
                        return match reply {
                            Reply::Ok { version, response } if version == PROTOCOL_VERSION => Ok(response),
                            Reply::Error { version, error } if version == PROTOCOL_VERSION => {
                                if error.code == FailureCode::Uncertain {
                                    self.state.disconnect();
                                    self.needs_refresh = true;
                                    rejected = true;
                                    bail!("Window action outcome uncertain; refresh after a new owner observation")
                                }
                                rejected = true;
                                bail!("Window request rejected ({:?})", error.code)
                            }
                            _ => bail!("Event reply version mismatch"),
                        };
                    }
                    ServerMessage::EventStream { subscription, stream }
                        if Some(subscription) == self.subscription =>
                    {
                        match self.state.fold_poll(stream)? {
                            FoldOutcome::Applied(_) => {}
                            FoldOutcome::NeedsSnapshot => self.needs_refresh = true,
                            FoldOutcome::Ended(_) => bail!("Event authority ended"),
                        }
                    }
                    ServerMessage::EventStream { subscription, .. }
                        if Some(subscription) == self.retired_subscription => {}
                    ServerMessage::Disconnected { .. } => bail!("Event authority ended"),
                    ServerMessage::WindowApply { .. }
                    | ServerMessage::OwnerResponse { .. }
                    | ServerMessage::Ready { .. }
                    | ServerMessage::Response { .. }
                    | ServerMessage::EventStream { .. } => bail!("Unexpected event frame"),
                }
            }
        })
        .await;
        match received {
            Ok(Err(error)) if rejected => Err(error),
            Ok(Err(error)) => {
                self.terminal = true;
                self.last_issue = Some("transport or protocol failure");
                self.state.disconnect();
                if action_sent {
                    bail!("Window action outcome uncertain; event connection fenced ({error})")
                }
                bail!("Event connection fenced after request failure ({error})")
            }
            Ok(result) => result,
            Err(_) => {
                self.terminal = true;
                self.last_issue = Some("request deadline reached");
                self.state.disconnect();
                if action_sent {
                    bail!("Window action outcome uncertain; event connection fenced")
                }
                bail!("Event request timed out; connection fenced")
            }
        }
    }

    async fn snapshot(&mut self) -> Result<Value> {
        if !self.terminal {
            // Snapshot is diagnostic. Only the current subscription or an
            // explicit Resubscribe can install authoritative tracked state.
            let ClientResponse::Snapshot { snapshot } =
                self.command(ClientCommand::Snapshot, false).await?
            else {
                self.fence_protocol();
                bail!("Unexpected window snapshot reply");
            };
            let mut diagnostic = ClientState::default();
            if diagnostic
                .install_snapshot(snapshot, self.ready.epoch)
                .is_err()
            {
                self.fence_protocol();
                bail!("Invalid window snapshot");
            }
        }
        Ok(json!({
            "epoch": self.ready.epoch,
            "windows": if self.terminal || self.needs_refresh { Vec::new() } else { self.state.windows().values().collect::<Vec<_>>() },
            "stream_state": format!("{:?}", self.state.state()),
            "available": !self.terminal,
            "needs_refresh": self.needs_refresh,
            "issue": self.last_issue,
            "observation": "native-client reported; freshness is per window",
        }))
    }

    async fn refresh_subscription(&mut self) -> Result<Value> {
        ensure!(
            !self.terminal,
            "Event connection ended; inspect status and reconnect through a fresh GUI approval"
        );
        let subscription = self
            .subscription
            .context("Window subscription unavailable")?;
        let ClientResponse::Subscribed {
            subscription: fresh,
            snapshot,
        } = self
            .command(
                ClientCommand::Resubscribe {
                    subscription,
                    capacity: 128,
                },
                false,
            )
            .await?
        else {
            self.fence_protocol();
            bail!("Unexpected refresh subscription reply");
        };
        if self
            .state
            .install_snapshot(snapshot, self.ready.epoch)
            .is_err()
        {
            self.fence_protocol();
            bail!("Invalid refreshed window snapshot");
        }
        self.retired_subscription = Some(subscription);
        self.subscription = Some(fresh);
        self.needs_refresh = false;
        self.snapshot().await
    }
}

#[derive(Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum WindowOperation {
    Inspect,
    Refresh,
    Query,
    Track,
    Untrack,
    Control,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct WindowRequest {
    operation: WindowOperation,
    #[serde(default)]
    epoch: Option<u64>,
    #[serde(default)]
    window_id: Option<u64>,
    #[serde(default)]
    control: Option<Value>,
}

#[derive(Clone)]
struct WindowAgentServer {
    peer: Arc<Mutex<Peer>>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl WindowAgentServer {
    #[tool(
        description = "Inspect GUI-approved native windows, explicitly refresh a subscription, or query, track, untrack, or control one exact window ID. Control requires a fresh native-client report and exact GUI approval. A lost action receipt is uncertain and never replayed. This tool cannot grant access, select a native handle, create virtual users, or change Setup.",
        annotations(open_world_hint = false)
    )]
    async fn native_windows(&self, Parameters(args): Parameters<WindowRequest>) -> CallToolResult {
        let result = async {
            let mut peer = self.peer.lock().await;
            ensure!(
                !matches!(
                    args.operation,
                    WindowOperation::Inspect | WindowOperation::Refresh
                ) || (args.window_id.is_none() && args.epoch.is_none()),
                "Inspect and Refresh accept no window reference"
            );
            ensure!(
                matches!(args.operation, WindowOperation::Control) || args.control.is_none(),
                "Only Control accepts an action"
            );
            if matches!(args.operation, WindowOperation::Inspect) {
                return peer.snapshot().await;
            }
            if matches!(args.operation, WindowOperation::Refresh) {
                return peer.refresh_subscription().await;
            }
            let id = args.window_id.context("Select one approved window ID")?;
            let window: WindowId = serde_json::from_value(json!(id))?;
            let epoch = args.epoch.context("Use the epoch returned by Inspect")?;
            ensure!(
                epoch == peer.ready.epoch,
                "Window reference belongs to a stale session"
            );
            let reference = RemoteWindowRef { epoch, window };
            let response = match args.operation {
                WindowOperation::Query => {
                    ensure!(
                        peer.ready.events.owned_windows,
                        "Window observation is unavailable"
                    );
                    let ClientResponse::Window { window: observed } = peer
                        .command(ClientCommand::RemoteWindowQuery { reference }, false)
                        .await?
                    else {
                        peer.fence_protocol();
                        bail!("Unexpected native-window query reply");
                    };
                    if observed.id != window || validate_reported_window(&observed).is_err() {
                        peer.fence_protocol();
                        bail!("Native-window query observation mismatch");
                    }
                    return Ok(json!({"window": observed}));
                }
                WindowOperation::Track => {
                    ensure!(
                        peer.ready.controls.window_tracking,
                        "Window tracking is unavailable"
                    );
                    let ClientResponse::Window { window: observed } = peer
                        .command(ClientCommand::RemoteWindowTrack { reference }, false)
                        .await?
                    else {
                        peer.fence_protocol();
                        bail!("Unexpected native-window tracking reply");
                    };
                    if observed.id != window || validate_reported_window(&observed).is_err() {
                        peer.fence_protocol();
                        bail!("Native-window tracking observation mismatch");
                    }
                    return Ok(json!({"window":observed,"refresh_available":true}));
                }
                WindowOperation::Untrack => {
                    ensure!(
                        peer.ready.controls.window_tracking,
                        "Window tracking is unavailable"
                    );
                    let response = peer
                        .command(ClientCommand::RemoteWindowUntrack { reference }, false)
                        .await?;
                    if !matches!(&response, ClientResponse::Acknowledged) {
                        peer.fence_protocol();
                        bail!("Unexpected native-window untracking reply");
                    }
                    response
                }
                WindowOperation::Control => {
                    ensure!(
                        peer.ready.controls.window_tracking,
                        "Window tracking is unavailable"
                    );
                    ensure!(
                        peer.ready.controls.window_control,
                        "Window control is unavailable"
                    );
                    ensure!(
                        !peer.needs_refresh,
                        "Refresh the window subscription before another action"
                    );
                    let ClientResponse::Window { window: current } = peer
                        .command(ClientCommand::RemoteWindowQuery { reference }, false)
                        .await?
                    else {
                        peer.fence_protocol();
                        bail!("Unexpected current native-window query reply");
                    };
                    if current.id != window || validate_reported_window(&current).is_err() {
                        peer.fence_protocol();
                        bail!("Invalid current native-window report");
                    }
                    ensure!(
                        !current.observation.stale && !peer.needs_refresh,
                        "Native-window observation is stale; refresh the owner and subscription"
                    );
                    let control: WindowControl = serde_json::from_value(
                        args.control.context("Provide one typed window action")?,
                    )?;
                    let action = control.action();
                    let ClientResponse::WindowControlled { acknowledgement } = peer
                        .command(
                            ClientCommand::RemoteWindowControl { reference, control },
                            true,
                        )
                        .await?
                    else {
                        peer.fence_protocol();
                        bail!("Unexpected native-window action reply; outcome uncertain");
                    };
                    if acknowledgement.window != window
                        || acknowledgement.observed.id != window
                        || acknowledgement.action != action
                        || acknowledgement.observed.observation.stale
                        || validate_reported_window(&acknowledgement.observed).is_err()
                    {
                        peer.fence_protocol();
                        bail!("Native-window action receipt mismatch; outcome uncertain");
                    }
                    return Ok(json!({"acknowledgement": acknowledgement}));
                }
                WindowOperation::Inspect | WindowOperation::Refresh => unreachable!(),
            };
            Ok(json!({"response":response}))
        }
        .await;
        match result {
            Ok(value) => CallToolResult::structured(value),
            Err(error) => {
                CallToolResult::error(vec![rmcp::model::ContentBlock::text(format!("{error:#}"))])
            }
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for WindowAgentServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("protoinputmultiplex-window-agent", env!("CARGO_PKG_VERSION"))
                    .with_title(crate::APP_NAME),
            )
            .with_instructions("Use only the exact native window IDs approved in the GUI. Inspect and track before control. Window state and metadata are client-reported and may be stale. A lost action receipt is uncertain; do not replay. This connection cannot grant access or use virtual routing or Setup tools.".to_string())
    }
}

pub(crate) fn run(path: PathBuf) -> Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async move {
            let peer = Peer::connect(&path).await?;
            let server = WindowAgentServer {
                peer: Arc::new(Mutex::new(peer)),
                tool_router: WindowAgentServer::tool_router(),
            };
            server
                .serve(rmcp::transport::stdio())
                .await?
                .waiting()
                .await?;
            Ok(())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_window_router_has_no_setup_or_virtual_routing_tools() {
        let tools = WindowAgentServer::tool_router().list_all();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "native_windows");
        assert!(
            serde_json::from_value::<WindowRequest>(json!({
                "operation": "control",
                "epoch": 7,
                "window_id": 9,
                "control": {"kind": "restore"},
                "grant": true
            }))
            .is_err()
        );
    }

    #[test]
    fn query_only_window_receiver_is_admitted_without_control_authority() {
        let events = mux_application_sdk::Grant {
            owned_windows: true,
            ..Default::default()
        };
        let ready = EventReady {
            version: PROTOCOL_VERSION,
            epoch: 71,
            recipient: serde_json::from_value(json!(1)).unwrap(),
            events,
            controls: mux_application_sdk::ControlGrant::default(),
            capabilities: Vec::new(),
            limits: mux_application_sdk::EventLimits::default(),
        };
        assert!(window_only_admission(&ready));
        let mut broad = ready.clone();
        broad.controls.pointer_input = true;
        assert!(!window_only_admission(&broad));
        broad = ready;
        broad.controls.window_publication = true;
        assert!(!window_only_admission(&broad));
    }
}
