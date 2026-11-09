//! Separate window/host and explicit user-toolkit MCP consumers of GUI-issued event leases.
//! This router cannot create users, grant itself access, or use stopped Setup.
use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use mux_application_sdk::{
    ClientCommand, ClientMessage, ClientRequest, ClientResponse, ClientState, EventHello,
    EventReady, FailureCode, FoldOutcome, HostRevision, HostViewportAction, HostedAction,
    HostedMode, HostedPhase, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, PROTOCOL_VERSION,
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
    hosted_receipt: mux_application_sdk::client::HostedReceipt,
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
        && (ready.events.owned_windows || ready.events.host_state)
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

fn toolkit_only_admission(ready: &EventReady) -> bool {
    let UserScope::Only(users) = &ready.events.users else {
        return false;
    };
    let UserScope::Only(control_users) = &ready.controls.users else {
        return false;
    };
    let no_controls = mux_application_sdk::ControlGrant {
        users: ready.controls.users.clone(),
        ..Default::default()
    };
    ready.version == PROTOCOL_VERSION
        && ready.epoch != 0
        && !users.is_empty()
        && users.len() <= mux_application_sdk::MAX_USERS
        && users
            .iter()
            .all(|user| user.epoch == ready.epoch && user.local != 0)
        && (ready.events.pointer_events || ready.events.key_events)
        && !ready.events.owned_windows
        && !ready.events.host_state
        && control_users.is_subset(users)
        && ready.controls == no_controls
}

impl Peer {
    fn fence_protocol(&mut self) {
        self.terminal = true;
        self.last_issue = Some("invalid window response");
        self.state.disconnect();
        self.hosted_receipt.clear();
    }

    async fn connect(path: &PathBuf) -> Result<Self> {
        Self::connect_scoped(path, window_only_admission).await
    }

    async fn connect_scoped(path: &PathBuf, admission: fn(&EventReady) -> bool) -> Result<Self> {
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
                admission(&ready),
                "This receiver does not match the requested event mode"
            );
            let mut peer = Self {
                socket,
                ready,
                state: ClientState::default(),
                hosted_receipt: mux_application_sdk::client::HostedReceipt::default(),
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
            peer.hosted_receipt.clear();
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
        let hosted_action = matches!(
            &command,
            ClientCommand::HostedSetMode { .. } | ClientCommand::HostedViewport { .. }
        );
        let deadline = if hosted_action {
            Duration::from_secs(60)
        } else {
            CALL_TIMEOUT
        };
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
        let received = timeout(deadline, async {
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
                                    self.hosted_receipt.clear();
                                    self.needs_refresh = true;
                                    if hosted_action {
                                        self.terminal = true;
                                        self.last_issue = Some("hosted action outcome uncertain; do not replay");
                                        rejected = true;
                                        bail!("Action outcome uncertain; inspect fresh authority without replay")
                                    }
                                    rejected = true;
                                    bail!("Window action outcome uncertain; refresh after a new owner observation without replay")
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
                        if self.needs_refresh
                            && self.state.state() == &mux_application_sdk::StreamState::Disconnected
                        {
                            // An uncertain window action cleared tracked authority.
                            // Drain this superseded stream until explicit Resubscribe;
                            // it must neither restore state nor fence diagnostic Inspect.
                            if matches!(stream, mux_application_sdk::Poll::Ended { .. }) {
                                bail!("Event authority ended");
                            }
                            continue;
                        }
                        match self.state.fold_poll(stream)? {
                            FoldOutcome::Applied(_) => self.hosted_receipt.reconcile(&self.state)?,
                            FoldOutcome::NeedsSnapshot => {
                                self.hosted_receipt.clear();
                                self.needs_refresh = true;
                            }
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
                self.hosted_receipt.clear();
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
                self.hosted_receipt.clear();
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
        self.resubscribe().await?;
        self.snapshot().await
    }

    async fn resubscribe(&mut self) -> Result<()> {
        ensure!(
            !self.terminal,
            "Event connection ended; inspect status and reconnect through a fresh GUI approval"
        );
        let subscription = self
            .subscription
            .context("Window subscription unavailable")?;
        self.hosted_receipt.clear();
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
        self.hosted_receipt.clear();
        self.retired_subscription = Some(subscription);
        self.subscription = Some(fresh);
        self.needs_refresh = false;
        Ok(())
    }

    async fn toolkit_barrier(&mut self) -> Result<()> {
        ensure!(
            !self.terminal
                && !self.needs_refresh
                && self.state.epoch() == Some(self.ready.epoch)
                && self.state.state() == &mux_application_sdk::StreamState::Live,
            "Refresh the event subscription explicitly before local toolkit access"
        );
        let subscription = self
            .subscription
            .context("Toolkit subscription unavailable")?;
        let response = match self
            .command(ClientCommand::Stats { subscription }, false)
            .await
        {
            Ok(response) => response,
            Err(_) => {
                self.fence_protocol();
                bail!("Toolkit authority barrier failed; obtain fresh GUI admission");
            }
        };
        let ClientResponse::Stats { stats } = response else {
            self.fence_protocol();
            bail!("Unexpected toolkit subscription receipt");
        };
        if stats.capacity != 128 || stats.queued > stats.capacity {
            self.fence_protocol();
            bail!("Invalid toolkit subscription statistics");
        }
        if stats.overflowed {
            self.state.disconnect();
            self.hosted_receipt.clear();
            self.needs_refresh = true;
            bail!("Toolkit stream overflowed; explicitly refresh before local access");
        }
        // HostBridge applies Stats before polling this turn's event backlog. Earlier
        // stream frames precede its reply in the same output FIFO and command folds
        // them first. A nonempty backlog therefore cannot authorize cached access.
        ensure!(
            stats.queued == 0,
            "Toolkit stream has queued events; inspect again after delivery"
        );
        ensure!(
            self.subscription == Some(subscription)
                && !self.terminal
                && !self.needs_refresh
                && self.state.epoch() == Some(self.ready.epoch)
                && self.state.state() == &mux_application_sdk::StreamState::Live,
            "Toolkit subscription is no longer live"
        );
        Ok(())
    }

    async fn local_toolkit(&mut self, request: &str) -> Result<String> {
        self.toolkit_barrier().await?;
        self.state
            .local_toolkit_json(request)
            .map_err(|_| anyhow::anyhow!("Local toolkit request rejected"))
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

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct HostedRequest {
    /// inspect, set_mode, minimize, or restore
    operation: String,
    epoch: u64,
    revision: Option<u64>,
    /// native, switched, or concurrent; accepted only for set_mode
    mode: Option<String>,
}

#[derive(Clone)]
struct WindowAgentServer {
    peer: Arc<Mutex<Peer>>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl WindowAgentServer {
    #[tool(
        description = "Inspect the GUI hosted session or explicitly set native/switched/concurrent mode, minimize, or restore. Observation and each action require separate current GUI grants. Supply exact epoch and observed revision. Lost receipts are uncertain and never replayed. Setup is read-only.",
        annotations(open_world_hint = false)
    )]
    async fn hosted_session(&self, Parameters(args): Parameters<HostedRequest>) -> CallToolResult {
        let result: Result<Value> = async {
            let mut guard = self.peer.lock().await;
            let peer = &mut *guard;
            ensure!(!peer.terminal && !peer.needs_refresh && peer.state.state() == &mux_application_sdk::StreamState::Live, "Refresh the event subscription first");
            ensure!(peer.ready.events.host_state && args.epoch == peer.ready.epoch, "Hosted observation unavailable or epoch stale");
            if args.operation == "inspect" {
                ensure!(args.revision.is_none() && args.mode.is_none(), "Inspect accepts no action");
                let response = peer.command(ClientCommand::HostedStateQuery { epoch: args.epoch }, false).await?;
                if let ClientResponse::HostedState { state } = response {
                    ensure!(state.revision != 0, "Invalid hosted revision");
                    ensure!(!peer.needs_refresh && peer.state.state() == &mux_application_sdk::StreamState::Live, "Refresh the event subscription first");
                    if peer.hosted_receipt.observe(args.epoch, &state, &peer.state).is_err() {
                        peer.fence_protocol();
                        bail!("Hosted query conflicted with the current stream");
                    }
                    ensure!(peer.hosted_receipt.current(&peer.state, args.epoch)
                        == Some(HostRevision { epoch: args.epoch, revision: state.revision }),
                        "Hosted observation changed; inspect again");
                    return Ok(json!({"epoch":args.epoch,"hosted":state}));
                }
                peer.fence_protocol();
                bail!("Invalid hosted observation response");
            }
            let reference = HostRevision { epoch: args.epoch, revision: args.revision.context("Supply the observed hosted revision")? };
            ensure!(peer.hosted_receipt.permits(&peer.state, reference), "Hosted revision stale; inspect current subscription");
            let action = match args.operation.as_str() {
                "set_mode" => {
                    ensure!(peer.ready.controls.host_mode_control, "Hosted mode control was not approved");
                    let mode = match args.mode.as_deref() {
                        Some("native") => HostedMode::Native,
                        Some("switched") => HostedMode::Switched,
                        Some("concurrent") => HostedMode::Concurrent,
                        _ => bail!("Choose native, switched, or concurrent"),
                    };
                    HostedAction::SetMode(mode)
                }
                "minimize" | "restore" => {
                    ensure!(peer.ready.controls.host_viewport_control && args.mode.is_none(), "Hosted viewport control was not approved or mode supplied");
                    HostedAction::Viewport(if args.operation == "minimize" { HostViewportAction::Minimize } else { HostViewportAction::Restore })
                }
                _ => bail!("Unknown hosted operation"),
            };
            let command = match action {
                HostedAction::SetMode(mode) => ClientCommand::HostedSetMode { reference, mode },
                HostedAction::Viewport(action) => ClientCommand::HostedViewport { reference, action },
            };
            let response = peer.command(command, true).await?;
            if let ClientResponse::HostedAction { action: observed, state } = response {
                let applied = match action {
                    HostedAction::SetMode(mode) => state.mode == Some(mode) && state.phase == if mode == HostedMode::Native { HostedPhase::Native } else { HostedPhase::Ready },
                    HostedAction::Viewport(action) => state.minimized == Some(matches!(action, HostViewportAction::Minimize)),
                };
                if observed == action && state.revision >= reference.revision && applied {
                    if !peer.needs_refresh && peer.state.state() == &mux_application_sdk::StreamState::Live
                        && peer.hosted_receipt.observe(args.epoch, &state, &peer.state).is_err()
                    {
                        peer.fence_protocol();
                        bail!("Hosted action receipt conflicted with the current stream; outcome uncertain");
                    }
                    return Ok(json!({"epoch":args.epoch,"action":observed,"hosted":state}));
                }
            }
            peer.fence_protocol();
            bail!("Hosted action outcome uncertain; invalid receipt and connection fenced")
        }.await;
        match result {
            Ok(value) => CallToolResult::structured(value),
            Err(error) => {
                CallToolResult::error(vec![rmcp::model::ContentBlock::text(error.to_string())])
            }
        }
    }
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
            .with_instructions("Use native_windows only for exact GUI-approved windows; inspect, track and refresh before control. Window reports are client-reported and may be stale. Use hosted_session only under its separate host-state and mode or viewport grants, with the exact epoch and observed hosted revision. Native means routing Stop; starting uses the configured automatic profile. A lost action receipt is uncertain; do not replay. This connection cannot grant access or use Setup or general virtual-input routing tools.".to_string())
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct LocalToolkitRequest {
    /// Version-1 SDK toolkit JSON text, preserving exact u64 identity integers; maximum 8192 UTF-8 bytes.
    request_json: String,
}

#[derive(Clone)]
struct ToolkitAgentServer {
    peer: Arc<Mutex<Peer>>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl ToolkitAgentServer {
    #[tool(
        description = "Use the shared consumer-local user directory, opaque-source lookup, application JSON data and badge palette for the current GUI-approved selected users. Pass version-1 SDK toolkit JSON as request_json text to preserve exact u64 identities. No physical identity, grants, user creation, Host/input controls or persistence. Every operation first checks the current event subscription through an acknowledged server Stats barrier. Gaps require explicit refresh; terminal authority requires fresh GUI admission.",
        annotations(open_world_hint = false)
    )]
    async fn local_user_toolkit(
        &self,
        Parameters(args): Parameters<LocalToolkitRequest>,
    ) -> CallToolResult {
        if args.request_json.len() > mux_application_sdk::user_toolkit::MAX_TOOLKIT_REQUEST_BYTES {
            return CallToolResult::error(vec![rmcp::model::ContentBlock::text(
                "Local toolkit request exceeds its size bound",
            )]);
        }
        let result = {
            let mut peer = self.peer.lock().await;
            peer.local_toolkit(&args.request_json).await
        };
        match result {
            // Keep the SDK result as exact JSON text rather than rounding identities
            // through an MCP wrapper or converting it to a second numeric schema.
            Ok(text) => CallToolResult::success(vec![rmcp::model::ContentBlock::text(text)]),
            Err(_) => CallToolResult::error(vec![rmcp::model::ContentBlock::text(
                "Local toolkit unavailable or request rejected; inspect the event subscription and refresh explicitly after a gap. Never replay a mutation automatically.",
            )]),
        }
    }

    #[tool(
        description = "Explicitly replace this toolkit receiver's current subscription after a gap. This is not a GUI grant, reconnect, Snapshot query or automatic replay. A terminal receiver must obtain fresh GUI admission. Gap/terminal state clears application data and resets the badge palette; refresh cannot recover those cleared values.",
        annotations(open_world_hint = false)
    )]
    async fn refresh_user_toolkit(&self) -> CallToolResult {
        let mut peer = self.peer.lock().await;
        match peer.resubscribe().await {
            Ok(()) => CallToolResult::structured(json!({"refreshed": true})),
            Err(_) => CallToolResult::error(vec![rmcp::model::ContentBlock::text(
                "Toolkit subscription refresh failed; terminal authority requires fresh GUI admission.",
            )]),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ToolkitAgentServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("protoinputmultiplex-user-toolkit-agent", env!("CARGO_PKG_VERSION"))
                    .with_title(crate::APP_NAME),
            )
            .with_instructions("This explicit receiver exposes only local_user_toolkit and refresh_user_toolkit for GUI-selected user observation. Values and badge colors are private consumer-local memory, not Host cursor changes or files. Keep u64 identities lossless in SDK JSON text. An acknowledged server barrier precedes each local operation; it is not instantaneous knowledge of later revocation. Do not replay mutations after an error. No Setup, window/host controls or routing/input tools are available.".to_string())
    }
}

pub(crate) fn run_toolkit(path: PathBuf) -> Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async move {
            let peer = Peer::connect_scoped(&path, toolkit_only_admission).await?;
            let server = ToolkitAgentServer {
                peer: Arc::new(Mutex::new(peer)),
                tool_router: ToolkitAgentServer::tool_router(),
            };
            server
                .serve(rmcp::transport::stdio())
                .await?
                .waiting()
                .await?;
            Ok(())
        })
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

    fn recovery_snapshot(revision: u64) -> mux_application_sdk::Snapshot {
        let bounds = mux_core::desktop::Bounds {
            left: 0,
            top: 0,
            right: 640,
            bottom: 480,
        };
        mux_application_sdk::Snapshot {
            version: PROTOCOL_VERSION,
            epoch: 71,
            through_sequence: revision,
            users: Vec::new(),
            screens: mux_application_sdk::ScreenState {
                revision,
                desktop: bounds,
                screens: vec![bounds],
            },
            windows: Vec::new(),
            capabilities: Vec::new(),
            mode: mux_application_sdk::ApplicationMode::Cooperative,
            hosted: None,
        }
    }

    #[tokio::test]
    async fn uncertain_window_drains_old_stream_until_explicit_refresh_but_hosted_is_terminal() {
        timeout(Duration::from_secs(5), async {
            for hosted in [false, true] {
                let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                    .await
                    .unwrap();
                let address = listener.local_addr().unwrap();
                let subscription = serde_json::from_value(json!(1)).unwrap();
                let fresh = serde_json::from_value(json!(2)).unwrap();
                let server = tokio::spawn(async move {
                    let (stream, _) = listener.accept().await.unwrap();
                    let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                    for step in 1..=if hosted { 1 } else { 5 } {
                        let Message::Text(text) = socket.next().await.unwrap().unwrap() else {
                            panic!("Expected one request without action replay");
                        };
                        let message: ClientMessage = serde_json::from_str(text.as_str()).unwrap();
                        assert_eq!(message.request_id, step);
                        let reply = if step == 1 {
                            assert!(matches!(
                                message.request.command,
                                ClientCommand::RemoteWindowControl { .. }
                                    | ClientCommand::HostedViewport { .. }
                            ));
                            Reply::Error {
                                version: PROTOCOL_VERSION,
                                error: mux_application_sdk::Failure {
                                    code: FailureCode::Uncertain,
                                    message: "owner receipt deadline".into(),
                                },
                            }
                        } else {
                            let stream = match step {
                                2 => mux_application_sdk::Poll::Events {
                                    events: vec![mux_application_sdk::Envelope {
                                        version: PROTOCOL_VERSION,
                                        epoch: 71,
                                        sequence: 2,
                                        event: mux_application_sdk::Event::ScreensChanged {
                                            screens: recovery_snapshot(2).screens,
                                        },
                                    }],
                                },
                                3 | 4 => mux_application_sdk::Poll::Gap {
                                    after_sequence: 2,
                                    lost_through: 3,
                                },
                                _ => mux_application_sdk::Poll::Ended {
                                    reason: mux_application_sdk::EndReason::Revoked,
                                },
                            };
                            socket
                                .send(Message::Text(
                                    serde_json::to_string(&ServerMessage::EventStream {
                                        subscription: if step == 5 { fresh } else { subscription },
                                        stream,
                                    })
                                    .unwrap()
                                    .into(),
                                ))
                                .await
                                .unwrap();
                            let response = if step == 3 {
                                assert!(matches!(
                                    message.request.command,
                                    ClientCommand::Resubscribe { subscription: old, .. } if old == subscription
                                ));
                                ClientResponse::Subscribed {
                                    subscription: fresh,
                                    snapshot: recovery_snapshot(3),
                                }
                            } else {
                                assert_eq!(message.request.command, ClientCommand::Snapshot);
                                ClientResponse::Snapshot {
                                    snapshot: recovery_snapshot(4),
                                }
                            };
                            if step == 5 {
                                break;
                            }
                            Reply::Ok {
                                version: PROTOCOL_VERSION,
                                response,
                            }
                        };
                        socket
                            .send(Message::Text(
                                serde_json::to_string(&ServerMessage::Response {
                                    request_id: message.request_id,
                                    reply,
                                })
                                .unwrap()
                                .into(),
                            ))
                            .await
                            .unwrap();
                    }
                });
                let stream = TcpStream::connect(address).await.unwrap();
                let (socket, _) =
                    tokio_tungstenite::client_async(format!("ws://{address}"), stream)
                        .await
                        .unwrap();
                let mut state = ClientState::default();
                state.install_snapshot(recovery_snapshot(1), 71).unwrap();
                let mut peer = Peer {
                    socket,
                    ready: EventReady {
                        version: PROTOCOL_VERSION,
                        epoch: 71,
                        recipient: serde_json::from_value(json!(1)).unwrap(),
                        events: mux_application_sdk::Grant::default(),
                        controls: mux_application_sdk::ControlGrant::default(),
                        capabilities: Vec::new(),
                        limits: mux_application_sdk::EventLimits::default(),
                    },
                    state,
                    hosted_receipt: Default::default(),
                    subscription: Some(subscription),
                    retired_subscription: None,
                    next_request: 0,
                    terminal: false,
                    needs_refresh: false,
                    last_issue: None,
                };
                let command = if hosted {
                    ClientCommand::HostedViewport {
                        reference: HostRevision {
                            epoch: 71,
                            revision: 1,
                        },
                        action: HostViewportAction::Restore,
                    }
                } else {
                    ClientCommand::RemoteWindowControl {
                        reference: RemoteWindowRef {
                            epoch: 71,
                            window: serde_json::from_value(json!(9)).unwrap(),
                        },
                        control: WindowControl::Restore,
                    }
                };
                assert!(peer.command(command, true).await.is_err());
                assert!(peer.needs_refresh && peer.state.windows().is_empty());
                let inspected = peer.snapshot().await.unwrap();
                assert_eq!(inspected["available"], !hosted);
                assert_eq!(inspected["needs_refresh"], true);
                assert_eq!(inspected["windows"], json!([]));
                assert!(peer.state.screens().is_none());
                if hosted {
                    assert!(peer.terminal);
                    assert!(peer.refresh_subscription().await.is_err());
                    assert_eq!(peer.next_request, 1);
                } else {
                    let refreshed = peer.refresh_subscription().await.unwrap();
                    assert_eq!(refreshed["available"], true);
                    assert_eq!(refreshed["needs_refresh"], false);
                    assert_eq!(peer.state.screens().unwrap().revision, 3);
                    assert_eq!(peer.state.last_sequence(), 3);
                    assert_eq!(peer.state.state(), &mux_application_sdk::StreamState::Live);
                    assert_eq!(peer.retired_subscription, Some(subscription));
                    assert!(peer.snapshot().await.is_err());
                    assert!(peer.terminal && peer.state.screens().is_none());
                    assert_eq!(peer.next_request, 5);
                }
                server.await.unwrap();
            }
        })
        .await
        .unwrap();
    }

    #[test]
    fn event_router_exposes_only_window_and_hosted_session_tools() {
        let tools = WindowAgentServer::tool_router().list_all();
        let mut names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        names.sort_unstable();
        assert_eq!(names, ["hosted_session", "native_windows"]);
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

    fn toolkit_ready() -> EventReady {
        EventReady {
            version: PROTOCOL_VERSION,
            epoch: 71,
            recipient: serde_json::from_value(json!(1)).unwrap(),
            events: mux_application_sdk::Grant {
                users: UserScope::Only(std::collections::BTreeSet::from([
                    mux_application_sdk::UserId {
                        epoch: 71,
                        local: 1,
                    },
                ])),
                key_events: true,
                ..Default::default()
            },
            controls: mux_application_sdk::ControlGrant::default(),
            capabilities: Vec::new(),
            limits: mux_application_sdk::EventLimits::default(),
        }
    }

    fn toolkit_snapshot(revision: u64) -> mux_application_sdk::Snapshot {
        let mut snapshot = recovery_snapshot(revision);
        snapshot.users.push(mux_application_sdk::UserView {
            id: mux_application_sdk::UserId {
                epoch: 71,
                local: 1,
            },
            source: mux_application_sdk::SourceId(9007199254740993),
            name: "Selected user".into(),
            owned_by_recipient: false,
            pointer: None,
            cursor: None,
            held_keys: Some(Vec::new()),
        });
        snapshot
    }

    fn stats_reply(queued: usize, overflowed: bool) -> Reply<ClientResponse> {
        Reply::Ok {
            version: PROTOCOL_VERSION,
            response: ClientResponse::Stats {
                stats: mux_application_sdk::SubscriberStats {
                    capacity: 128,
                    queued,
                    delivered: 0,
                    overflowed,
                },
            },
        }
    }

    type ToolkitStep = (
        ClientCommand,
        Vec<ServerMessage>,
        Option<Reply<ClientResponse>>,
    );

    async fn toolkit_script(steps: Vec<ToolkitStep>) -> (Peer, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            for (index, (expected, prior_frames, reply)) in steps.into_iter().enumerate() {
                let Message::Text(text) = socket.next().await.unwrap().unwrap() else {
                    panic!("Expected one explicit request");
                };
                let message: ClientMessage = serde_json::from_str(text.as_str()).unwrap();
                assert_eq!(message.request_id, index as u64 + 1);
                assert_eq!(message.request.command, expected);
                for frame in prior_frames {
                    socket
                        .send(Message::Text(serde_json::to_string(&frame).unwrap().into()))
                        .await
                        .unwrap();
                }
                let Some(reply) = reply else {
                    socket.close(None).await.unwrap();
                    return;
                };
                socket
                    .send(Message::Text(
                        serde_json::to_string(&ServerMessage::Response {
                            request_id: message.request_id,
                            reply,
                        })
                        .unwrap()
                        .into(),
                    ))
                    .await
                    .unwrap();
            }
        });
        let stream = TcpStream::connect(address).await.unwrap();
        let (socket, _) = tokio_tungstenite::client_async(format!("ws://{address}"), stream)
            .await
            .unwrap();
        let mut state = ClientState::default();
        state.install_snapshot(toolkit_snapshot(1), 71).unwrap();
        state.local_toolkit_json(r#"{"version":1,"operation":{"kind":"set_data","user":{"epoch":71,"local":1},"data":"before"}}"#).unwrap();
        let subscription = serde_json::from_value(json!(1)).unwrap();
        (
            Peer {
                socket,
                ready: toolkit_ready(),
                state,
                hosted_receipt: Default::default(),
                subscription: Some(subscription),
                retired_subscription: None,
                next_request: 0,
                terminal: false,
                needs_refresh: false,
                last_issue: None,
            },
            server,
        )
    }

    #[test]
    fn toolkit_mode_has_distinct_tools_and_observation_only_admission() {
        let tools = ToolkitAgentServer::tool_router().list_all();
        let mut names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        names.sort_unstable();
        assert_eq!(names, ["local_user_toolkit", "refresh_user_toolkit"]);
        let ready = toolkit_ready();
        assert!(toolkit_only_admission(&ready));
        assert!(!window_only_admission(&ready));
        let mut gui_ready = ready.clone();
        gui_ready.controls.users = gui_ready.events.users.clone();
        assert!(toolkit_only_admission(&gui_ready));
        for field in [
            "delegated_users",
            "user_lifecycle",
            "cursor",
            "pointer_input",
            "key_input",
            "window_publication",
            "window_identity_metadata",
            "window_tracking",
            "window_control",
            "host_mode_control",
            "host_viewport_control",
        ] {
            let mut value = serde_json::to_value(&ready).unwrap();
            value["controls"][field] = json!(true);
            assert!(!toolkit_only_admission(
                &serde_json::from_value(value).unwrap()
            ));
        }
        for scope in [UserScope::Only(Default::default()), UserScope::AllOwned] {
            let mut broad = ready.clone();
            broad.events.users = scope;
            assert!(!toolkit_only_admission(&broad));
        }
        for field in ["owned_windows", "host_state"] {
            let mut value = serde_json::to_value(&ready).unwrap();
            value["events"][field] = json!(true);
            assert!(!toolkit_only_admission(
                &serde_json::from_value(value).unwrap()
            ));
        }
        let mut controls_all_owned = ready.clone();
        controls_all_owned.controls.users = UserScope::AllOwned;
        assert!(!toolkit_only_admission(&controls_all_owned));
        let mut pointer_only = ready.clone();
        pointer_only.events.key_events = false;
        pointer_only.events.pointer_events = true;
        assert!(toolkit_only_admission(&pointer_only));
        gui_ready.events.key_events = false;
        assert!(!toolkit_only_admission(&gui_ready));
    }

    #[tokio::test]
    async fn toolkit_stats_barrier_rejects_backlog_overflow_wrong_variant_revoked_lease_and_eof() {
        timeout(Duration::from_secs(5), async {
            let subscription = serde_json::from_value(json!(1)).unwrap();
            let replies = [
                Some(stats_reply(1, false)),
                Some(stats_reply(0, true)),
                Some(stats_reply(129, false)),
                Some(Reply::Ok { version: PROTOCOL_VERSION, response: ClientResponse::Snapshot { snapshot: toolkit_snapshot(9) } }),
                Some(Reply::Error { version: PROTOCOL_VERSION, error: mux_application_sdk::Failure {
                    code: FailureCode::Denied, message: "retired lease".into(),
                } }),
                None,
            ];
            for (index, reply) in replies.into_iter().enumerate() {
                let (mut peer, server) = toolkit_script(vec![(ClientCommand::Stats { subscription }, Vec::new(), reply)]).await;
                assert!(peer.local_toolkit(r#"{"version":1,"operation":{"kind":"set_data","user":{"epoch":71,"local":1},"data":"after"}}"#).await.is_err());
                if index == 0 {
                    // Backlog is not a mutation receipt; the rejected local write never ran.
                    let before = peer.state.local_toolkit_json(r#"{"version":1,"operation":{"kind":"user","user":{"epoch":71,"local":1}}}"#).unwrap();
                    assert!(before.contains("before") && !before.contains("after"));
                } else if index == 1 {
                    assert!(peer.needs_refresh);
                    assert_eq!(peer.state.state(), &mux_application_sdk::StreamState::Disconnected);
                } else {
                    assert!(peer.terminal);
                    assert!(peer.state.users().is_empty());
                }
                assert_eq!(peer.next_request, 1);
                server.await.unwrap();
            }
        }).await.unwrap();
    }

    #[tokio::test]
    async fn toolkit_folds_prior_disposal_before_zero_backlog_receipt_and_preserves_large_sources()
    {
        timeout(Duration::from_secs(5), async {
            let subscription = serde_json::from_value(json!(1)).unwrap();
            let (mut peer, server) = toolkit_script(vec![
                (
                    ClientCommand::Stats { subscription },
                    Vec::new(),
                    Some(stats_reply(0, false)),
                ),
                (
                    ClientCommand::Stats { subscription },
                    vec![ServerMessage::EventStream {
                        subscription,
                        stream: mux_application_sdk::Poll::Events {
                            events: vec![mux_application_sdk::Envelope {
                                version: PROTOCOL_VERSION,
                                epoch: 71,
                                sequence: 2,
                                event: mux_application_sdk::Event::UserDisposed {
                                    user: mux_application_sdk::UserId {
                                        epoch: 71,
                                        local: 1,
                                    },
                                },
                            }],
                        },
                    }],
                    Some(stats_reply(0, false)),
                ),
            ])
            .await;
            let found = peer
                .local_toolkit(
                    r#"{"version":1,"operation":{"kind":"source","source":9007199254740993}}"#,
                )
                .await
                .unwrap();
            assert!(found.contains("9007199254740993") && found.contains("before"));
            let absent = peer
                .local_toolkit(
                    r#"{"version":1,"operation":{"kind":"user","user":{"epoch":71,"local":1}}}"#,
                )
                .await
                .unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&absent).unwrap()["result"]["user"],
                Value::Null
            );
            assert!(peer.state.users().is_empty());
            assert_eq!(peer.next_request, 2);
            server.await.unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn toolkit_gap_requires_explicit_resubscribe_and_never_installs_diagnostic_snapshot() {
        timeout(Duration::from_secs(5), async {
            let subscription = serde_json::from_value(json!(1)).unwrap();
            let fresh = serde_json::from_value(json!(2)).unwrap();
            let (mut peer, server) = toolkit_script(vec![
                (
                    ClientCommand::Stats { subscription },
                    vec![ServerMessage::EventStream {
                        subscription,
                        stream: mux_application_sdk::Poll::Gap {
                            after_sequence: 1,
                            lost_through: 2,
                        },
                    }],
                    Some(stats_reply(0, false)),
                ),
                (
                    ClientCommand::Resubscribe {
                        subscription,
                        capacity: 128,
                    },
                    Vec::new(),
                    Some(Reply::Ok {
                        version: PROTOCOL_VERSION,
                        response: ClientResponse::Subscribed {
                            subscription: fresh,
                            snapshot: toolkit_snapshot(2),
                        },
                    }),
                ),
                (
                    ClientCommand::Stats {
                        subscription: fresh,
                    },
                    Vec::new(),
                    Some(stats_reply(0, false)),
                ),
            ])
            .await;
            assert!(
                peer.local_toolkit(r#"{"version":1,"operation":{"kind":"list"}}"#)
                    .await
                    .is_err()
            );
            assert!(peer.needs_refresh);
            assert!(
                peer.local_toolkit(r#"{"version":1,"operation":{"kind":"list"}}"#)
                    .await
                    .is_err()
            );
            assert_eq!(peer.next_request, 1); // No automatic refresh, Snapshot, or replay.
            peer.resubscribe().await.unwrap();
            let clean = peer
                .local_toolkit(
                    r#"{"version":1,"operation":{"kind":"user","user":{"epoch":71,"local":1}}}"#,
                )
                .await
                .unwrap();
            assert!(!clean.contains("before"));
            assert_eq!(peer.next_request, 3);
            server.await.unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn toolkit_revocation_stream_precedes_and_denies_even_a_zero_backlog_reply() {
        timeout(Duration::from_secs(5), async {
            let subscription = serde_json::from_value(json!(1)).unwrap();
            let (mut peer, server) = toolkit_script(vec![(
                ClientCommand::Stats { subscription },
                vec![ServerMessage::EventStream {
                    subscription,
                    stream: mux_application_sdk::Poll::Ended {
                        reason: mux_application_sdk::EndReason::Revoked,
                    },
                }],
                Some(stats_reply(0, false)),
            )])
            .await;
            assert!(
                peer.local_toolkit(r#"{"version":1,"operation":{"kind":"list"}}"#)
                    .await
                    .is_err()
            );
            assert!(peer.terminal && peer.state.users().is_empty());
            server.await.unwrap();
        })
        .await
        .unwrap();
    }
}
