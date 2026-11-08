//! Opt-in loopback transport. Its retained worker owns every accepted original
//! and GUI client until dispatch and client-scoped release cleanup have settled.
use crate::sdk_wire;
use anyhow::{Context, Result, bail, ensure};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use rmcp::model::{ClientJsonRpcMessage, JsonRpcMessage, RequestId, ServerJsonRpcMessage};
use std::{
    io,
    net::{Ipv4Addr, TcpListener as StdTcpListener},
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context as TaskContext, Poll},
    thread::{self, JoinHandle},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    task::JoinSet,
    time::{sleep, timeout},
};
use tokio_tungstenite::{
    WebSocketStream, accept_hdr_async_with_config,
    tungstenite::{
        Message,
        handshake::server::{Request, Response},
        protocol::{Role, WebSocketConfig},
    },
};

const UPGRADE_LIMIT: usize = 16 * 1024;
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
const SEND_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_POLL: Duration = Duration::from_millis(50);

type RpcSink = Pin<Box<dyn Sink<ServerJsonRpcMessage, Error = io::Error> + Send>>;
type RpcStream = Pin<Box<dyn Stream<Item = ClientJsonRpcMessage> + Send>>;
pub(crate) type RpcTransport =
    rmcp::transport::sink_stream::SinkStreamTransport<RpcSink, RpcStream>;

#[derive(Clone)]
pub(crate) struct Config {
    pub endpoint_path: PathBuf,
    pub gui_connection: PathBuf,
    pub runtime: PathBuf,
    pub preferences: PathBuf,
    pub allowed_origins: Vec<String>,
    pub stop: Arc<AtomicBool>,
}

pub(crate) struct Service {
    port: u16,
    endpoint_path: PathBuf,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<()>>>,
    failure: Option<String>,
}

impl Service {
    /// This method is called on the GUI's SDK actor, never the GUI thread.
    pub fn start(config: Config) -> Result<Self> {
        validate_origins(&config.allowed_origins)?;
        ensure!(
            !config.stop.load(Ordering::Acquire),
            "SDK startup was cancelled"
        );
        ensure!(
            !config.endpoint_path.exists(),
            "SDK endpoint file already exists"
        );
        let listener = StdTcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let token = crate::gui_bridge::random_token()?;
        let endpoint = sdk_wire::Endpoint {
            protoinputmultiplex_sdk: sdk_wire::VERSION,
            port,
            token: token.clone(),
        };
        crate::gui_bridge::write_private_descriptor(
            &config.endpoint_path,
            &serde_json::to_vec(&endpoint)?,
        )?;
        let stop = Arc::clone(&config.stop);
        let endpoint_path = config.endpoint_path.clone();
        let worker = thread::Builder::new()
            .name("sdk-loopback".into())
            .spawn(move || {
                runtime.block_on(async move {
                    let listener = TcpListener::from_std(listener)?;
                    listen(listener, config, token).await
                })
            });
        match worker {
            Ok(worker) => Ok(Self {
                port,
                endpoint_path,
                stop,
                worker: Some(worker),
                failure: None,
            }),
            Err(error) => {
                let _ = crate::gui_bridge::remove_descriptor_file(&endpoint_path);
                Err(error).context("start SDK loopback worker")
            }
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// True while accepting OR draining; a blocked original remains owned.
    pub fn is_running(&self) -> bool {
        self.worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
    }

    /// Background-only. No timeout can declare an accepted original retired.
    pub fn stop(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            match worker.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => self.failure = Some(format!("{error:#}")),
                Err(_) => self.failure = Some("SDK worker failed; cleanup is unconfirmed".into()),
            }
        }
        if let Some(error) = &self.failure {
            bail!("{error}");
        }
        crate::gui_bridge::remove_descriptor_file(&self.endpoint_path)
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

pub(crate) fn validate_origins(origins: &[String]) -> Result<()> {
    ensure!(
        origins.len() <= 16,
        "Use at most 16 trusted browser origins"
    );
    ensure!(
        origins.iter().map(String::len).sum::<usize>() <= 4096,
        "Browser origin list is too large"
    );
    for origin in origins {
        ensure!(
            !origin.contains('*'),
            "Trusted origins cannot contain wildcards"
        );
        let url =
            url::Url::parse(origin).context("Trusted origin must be an exact HTTP(S) origin")?;
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && origin == &url.origin().ascii_serialization(),
            "Trusted origin must be an exact HTTP(S) origin without a path, credentials, query, or fragment"
        );
    }
    Ok(())
}

fn origin_allowed(request: &Request, origins: &[String]) -> bool {
    let mut values = request.headers().get_all("origin").iter();
    match (values.next(), values.next()) {
        (None, None) => true,
        (Some(origin), None) => origin
            .to_str()
            .ok()
            .is_some_and(|origin| origins.iter().any(|allowed| allowed == origin)),
        _ => false,
    }
}

async fn listen(listener: TcpListener, config: Config, token: String) -> Result<()> {
    let mut sockets = JoinSet::new();
    let mut failure = None;
    while !config.stop.load(Ordering::Acquire) {
        tokio::select! {
            _ = sleep(STOP_POLL) => {},
            Some(completed) = sockets.join_next(), if !sockets.is_empty() => {
                if let Err(error) = completed {
                    failure = Some(anyhow::anyhow!("SDK socket owner failed; cleanup is unconfirmed: {error}"));
                    break;
                }
            },
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, peer)) if peer.ip().is_loopback() && sockets.len() < sdk_wire::MAX_CONNECTIONS => {
                        if config.stop.load(Ordering::Acquire) { drop(stream); continue; }
                        let config = config.clone();
                        let token = token.clone();
                        sockets.spawn(async move { let _ = connection(stream, config, &token).await; });
                    },
                    Ok((stream, _)) => drop(stream),
                    Err(error) => { failure = Some(error.into()); break; },
                }
            }
        }
    }
    // Fence admission before waiting, including a listener failure.
    config.stop.store(true, Ordering::Release);
    drop(listener);
    while let Some(completed) = sockets.join_next().await {
        if let Err(error) = completed {
            failure = Some(anyhow::anyhow!(
                "SDK socket owner failed; cleanup is unconfirmed: {error}"
            ));
        }
    }
    if let Some(error) = failure {
        return Err(error);
    }
    // Endpoint retirement is the final publication, after dispatch AND cleanup.
    crate::gui_bridge::remove_descriptor_file(&config.endpoint_path)?;
    Ok(())
}

/// The HTTP upgrade has a hard read budget as well as a deadline. Once upgraded,
/// tungstenite supplies independent hello and command message/frame bounds.
struct UpgradeStream {
    stream: TcpStream,
    prefetched: Vec<u8>,
    consumed: usize,
    upgrade_end: Option<usize>,
    read_chunk: usize,
}

impl UpgradeStream {
    async fn prefetch(mut stream: TcpStream) -> Result<Self> {
        let mut prefetched = Vec::with_capacity(UPGRADE_LIMIT + 1);
        let mut chunk = [0u8; 4096];
        loop {
            let available = (UPGRADE_LIMIT + 1 - prefetched.len()).min(chunk.len());
            ensure!(available != 0, "SDK upgrade exceeds 16 KiB");
            let count = stream.read(&mut chunk[..available]).await?;
            ensure!(count != 0, "SDK upgrade connection closed");
            prefetched.extend_from_slice(&chunk[..count]);
            if let Some(offset) = prefetched.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let end = offset + 4;
                ensure!(end <= UPGRADE_LIMIT, "SDK upgrade exceeds 16 KiB");
                return Ok(Self {
                    stream,
                    prefetched,
                    consumed: 0,
                    upgrade_end: Some(end),
                    read_chunk: usize::MAX,
                });
            }
        }
    }
}

impl AsyncRead for UpgradeStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if output.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let buffered_end = self.upgrade_end.unwrap_or(self.prefetched.len());
        if self.consumed < buffered_end {
            let count = (buffered_end - self.consumed)
                .min(output.remaining())
                .min(self.read_chunk);
            output.put_slice(&self.prefetched[self.consumed..self.consumed + count]);
            self.consumed += count;
            return Poll::Ready(Ok(()));
        }
        if self.upgrade_end.is_some() {
            return Poll::Ready(Err(io::Error::other(
                "SDK HTTP upgrade header was not accepted",
            )));
        }
        let available = output.remaining().min(self.read_chunk);
        let mut limited = ReadBuf::new(output.initialize_unfilled_to(available));
        match Pin::new(&mut self.stream).poll_read(cx, &mut limited) {
            Poll::Ready(Ok(())) => {
                let count = limited.filled().len();
                output.advance(count);
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

impl AsyncWrite for UpgradeStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

async fn authenticate(
    stream: TcpStream,
    origins: &[String],
    token: &str,
    stop: &AtomicBool,
) -> Result<WebSocketStream<UpgradeStream>> {
    let websocket_config = WebSocketConfig::default()
        .max_message_size(Some(sdk_wire::HELLO_LIMIT))
        .max_frame_size(Some(sdk_wire::HELLO_LIMIT))
        .write_buffer_size(0)
        .max_write_buffer_size(sdk_wire::RESULT_LIMIT + 1024);
    let origins = origins.to_vec();
    let mut websocket = timeout(AUTH_TIMEOUT, async move {
        // Accumulate bounded HTTP bytes under this same upgrade deadline.
        // Deliver the complete header in normal reads: one-byte HTTP reads
        // trigger tungstenite's small-packet attack protection after 64 reads.
        let stream = UpgradeStream::prefetch(stream).await?;
        // Tungstenite's Callback trait requires this unboxed HTTP error type.
        #[allow(clippy::result_large_err)]
        let validate_origin = move |request: &Request, response: Response| {
            if origin_allowed(request, &origins) {
                Ok(response)
            } else {
                Err(tokio_tungstenite::tungstenite::http::Response::builder()
                    .status(403)
                    .body(Some("SDK origin denied".into()))
                    .expect("constant response"))
            }
        };
        Ok::<_, anyhow::Error>(
            accept_hdr_async_with_config(stream, validate_origin, Some(websocket_config)).await?,
        )
    })
    .await??;
    // HTTP delivery stopped exactly at CRLFCRLF; coalesced frame bytes remain
    // in UpgradeStream. Single-byte hello reads now prevent WebSocket read-ahead
    // before replacing tungstenite's immutable 4 KiB pre-auth configuration.
    websocket.get_mut().upgrade_end = None;
    websocket.get_mut().read_chunk = 1;
    let hello = timeout(AUTH_TIMEOUT, websocket.next())
        .await?
        .context("SDK hello is required")??;
    let Message::Text(hello) = hello else {
        bail!("SDK hello must be a text frame");
    };
    sdk_wire::validate_hello(hello.as_bytes(), token)?;
    ensure!(!stop.load(Ordering::Acquire), "SDK is stopping");
    timeout(
        SEND_TIMEOUT,
        websocket.send(Message::Text(sdk_wire::ready()?.into())),
    )
    .await??;
    let mut stream = websocket.into_inner();
    stream.read_chunk = usize::MAX;
    Ok(WebSocketStream::from_raw_socket(
        stream,
        Role::Server,
        Some(
            WebSocketConfig::default()
                .max_message_size(Some(sdk_wire::MESSAGE_LIMIT))
                .max_frame_size(Some(sdk_wire::MESSAGE_LIMIT))
                .write_buffer_size(0)
                .max_write_buffer_size(sdk_wire::RESULT_LIMIT + 1024),
        ),
    )
    .await)
}

async fn connection(stream: TcpStream, config: Config, token: &str) -> Result<()> {
    let websocket = authenticate(stream, &config.allowed_origins, token, &config.stop).await?;
    // Authentication and origin checks above precede ALL Control/bridge creation.
    let (inbound, input) = mpsc::channel(1);
    let (output, outbound) = mpsc::channel(1);
    let sink: RpcSink = Box::pin(futures_util::sink::unfold(
        output,
        |output, message| async move {
            output
                .send(message)
                .await
                .map_err(|_| io::Error::other("SDK output closed"))?;
            Ok(output)
        },
    ));
    let stream: RpcStream = Box::pin(futures_util::stream::unfold(
        input,
        |mut input| async move { input.recv().await.map(|message| (message, input)) },
    ));
    let transport = RpcTransport::new(sink, stream);
    let backend = crate::mcp::serve_sdk(
        config.runtime,
        config.gui_connection,
        config.preferences,
        transport,
    );
    let network = pump(websocket, inbound, outbound, config.stop);
    let (backend, network) = tokio::join!(backend, network);
    backend?;
    network
}

#[derive(Default)]
struct Admission {
    in_flight: Option<RequestId>,
    initialize_seen: bool,
    initialized: bool,
}

impl Admission {
    fn accept(&mut self, message: &ClientJsonRpcMessage) -> Result<()> {
        ensure!(
            self.in_flight.is_none(),
            "SDK allows one in-flight request per connection"
        );
        match message {
            JsonRpcMessage::Request(request) => {
                let initialize = matches!(
                    &request.request,
                    rmcp::model::ClientRequest::InitializeRequest(_)
                );
                ensure!(
                    (!self.initialize_seen && initialize) || (self.initialized && !initialize),
                    "SDK requires MCP initialization once before commands"
                );
                self.initialize_seen |= initialize;
                self.in_flight = Some(request.id.clone());
            }
            JsonRpcMessage::Notification(notification)
                if matches!(
                    &notification.notification,
                    rmcp::model::ClientNotification::InitializedNotification(_)
                ) =>
            {
                ensure!(
                    self.initialize_seen && !self.initialized,
                    "Unexpected initialized notification"
                );
                self.initialized = true;
            }
            _ => bail!("Unsolicited SDK message"),
        }
        Ok(())
    }

    fn complete(&mut self, message: &ServerJsonRpcMessage) -> bool {
        let id = match message {
            JsonRpcMessage::Response(response) => Some(&response.id),
            JsonRpcMessage::Error(error) => error.id.as_ref(),
            _ => None,
        };
        if id.is_some() && id == self.in_flight.as_ref() {
            self.in_flight = None;
            true
        } else {
            false
        }
    }
}

async fn pump(
    mut websocket: WebSocketStream<UpgradeStream>,
    inbound: mpsc::Sender<ClientJsonRpcMessage>,
    mut outbound: mpsc::Receiver<ServerJsonRpcMessage>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let mut admission = Admission::default();
    let mut closed = false;
    let mut failure = None;
    loop {
        if stop.load(Ordering::Acquire) {
            closed = true;
        }
        if closed && admission.in_flight.is_none() {
            break;
        }
        tokio::select! {
            _ = sleep(STOP_POLL) => {},
            message = outbound.recv() => {
                let Some(message) = message else { break; };
                let matching = admission.complete(&message);
                if !closed {
                    match sdk_wire::encode_result(&message) {
                        Ok(text) => {
                            if !matches!(timeout(SEND_TIMEOUT, websocket.send(Message::Text(text.into()))).await, Ok(Ok(()))) { closed = true; }
                        },
                        Err(error) => {
                            let id = match &message { JsonRpcMessage::Response(response) => Some(response.id.clone()), JsonRpcMessage::Error(error) => error.id.clone(), _ => None };
                            let text = serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32001,"message":"SDK result exceeds the 128 MiB limit; the command was accepted and its outcome is uncertain. Reconnect and inspect before deciding whether to retry.","data":{"outcome_uncertain":true}}}).to_string();
                            let _ = timeout(SEND_TIMEOUT, websocket.send(Message::Text(text.into()))).await;
                            failure = Some(error);
                            closed = true;
                        },
                    }
                }
                // An unmatched output can never free the request slot.
                if !matching && !matches!(message, JsonRpcMessage::Notification(_)) {
                    failure = Some(anyhow::anyhow!("Unmatched SDK response"));
                    closed = true;
                }
            },
            message = websocket.next(), if !closed => {
                let parsed = match message {
                    Some(Ok(Message::Text(text))) if text.len() <= sdk_wire::MESSAGE_LIMIT => serde_json::from_str::<ClientJsonRpcMessage>(&text).map_err(anyhow::Error::from),
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                    _ => { closed = true; continue; },
                };
                match parsed.and_then(|message| { admission.accept(&message)?; Ok(message) }) {
                    Ok(message) => {
                        let Ok(permit) = inbound.reserve().await else { break; };
                        // No await between the stop fence and bounded dispatch.
                        if stop.load(Ordering::Acquire) { closed = true; admission.in_flight = None; continue; }
                        permit.send(message);
                    },
                    Err(error) => { failure = Some(error); closed = true; },
                }
            },
        }
    }
    // Keep inbound open while an accepted original is outstanding, even after
    // EOF, invalid pipelining, Stop, or a lost socket. rmcp's 5s drain is not proof.
    drop(inbound);
    let _ = timeout(SEND_TIMEOUT, websocket.close(None)).await;
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::AsyncWriteExt;
    use tokio_tungstenite::{
        client_async,
        tungstenite::protocol::frame::{
            Frame,
            coding::{Data, OpCode},
        },
    };

    fn network_test(test: impl std::future::Future<Output = ()>) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                timeout(Duration::from_secs(15), test)
                    .await
                    .expect("bounded SDK network test");
            });
    }

    async fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let (client, server) = tokio::join!(TcpStream::connect(address), listener.accept());
        (client.unwrap(), server.unwrap().0)
    }

    fn hello(token: &str) -> String {
        json!({"protoinputmultiplex_sdk":1,"type":"hello","token":token}).to_string()
    }

    fn raw_header(padding: usize) -> Vec<u8> {
        format!("GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nX-Pad: {}\r\n\r\n", "x".repeat(padding)).into_bytes()
    }

    fn masked_text(text: String) -> Vec<u8> {
        let mut frame = Frame::message(text.into_bytes(), OpCode::Data(Data::Text), true);
        frame.header_mut().mask = Some([1, 2, 3, 4]);
        let mut bytes = Vec::new();
        frame.format(&mut bytes).unwrap();
        bytes
    }

    async fn read_raw_upgrade(client: &mut TcpStream) {
        let mut response = Vec::new();
        while !response.ends_with(b"\r\n\r\n") {
            assert!(response.len() < UPGRADE_LIMIT);
            response.push(client.read_u8().await.unwrap());
        }
        assert!(response.starts_with(b"HTTP/1.1 101 "));
    }

    #[test]
    fn actual_upgrade_authenticates_normal_client_and_reconfigures_command_bound() {
        network_test(async {
            let (client, server) = tcp_pair().await;
            let token = "a".repeat(64);
            let stop = AtomicBool::new(false);
            let command = "x".repeat(sdk_wire::HELLO_LIMIT + 1);
            let server = async {
                let mut socket = authenticate(server, &[], &token, &stop).await.unwrap();
                assert_eq!(
                    socket.next().await.unwrap().unwrap(),
                    Message::Text(command.clone().into())
                );
            };
            let client = async {
                let url = format!("ws://{}/", client.peer_addr().unwrap());
                let (mut socket, _) = client_async(url, client).await.unwrap();
                socket
                    .send(Message::Text(hello(&token).into()))
                    .await
                    .unwrap();
                assert_eq!(
                    socket.next().await.unwrap().unwrap(),
                    Message::Text(sdk_wire::ready().unwrap().into())
                );
                socket
                    .send(Message::Text(command.clone().into()))
                    .await
                    .unwrap();
            };
            tokio::join!(server, client);
        });
    }

    #[test]
    fn actual_upgrade_preserves_coalesced_hello_and_next_frame() {
        network_test(async {
            let (mut client, server) = tcp_pair().await;
            let token = "a".repeat(64);
            let stop = AtomicBool::new(false);
            let command = "tail".repeat(sdk_wire::HELLO_LIMIT);
            let mut packet = raw_header(0);
            packet.extend(masked_text(hello(&token)));
            packet.extend(masked_text(command.clone()));
            client.write_all(&packet).await.unwrap();
            let server = async {
                let mut socket = authenticate(server, &[], &token, &stop).await.unwrap();
                assert_eq!(
                    socket.next().await.unwrap().unwrap(),
                    Message::Text(command.into())
                );
            };
            let client = async {
                read_raw_upgrade(&mut client).await;
                let mut socket = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
                assert_eq!(
                    socket.next().await.unwrap().unwrap(),
                    Message::Text(sdk_wire::ready().unwrap().into())
                );
            };
            tokio::join!(server, client);
        });
    }

    #[test]
    fn actual_upgrade_enforces_exact_header_byte_limit() {
        network_test(async {
            let token = "a".repeat(64);
            let stop = AtomicBool::new(false);
            let padding = UPGRADE_LIMIT - raw_header(0).len();
            let (mut client, server) = tcp_pair().await;
            let mut packet = raw_header(padding);
            assert_eq!(packet.len(), UPGRADE_LIMIT);
            packet.extend(masked_text(hello(&token)));
            let server = async {
                authenticate(server, &[], &token, &stop)
                    .await
                    .expect("exact header limit must authenticate");
            };
            let client = async {
                client.write_all(&packet).await.unwrap();
                read_raw_upgrade(&mut client).await;
                let mut socket = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
                assert_eq!(
                    socket.next().await.unwrap().unwrap(),
                    Message::Text(sdk_wire::ready().unwrap().into())
                );
            };
            tokio::join!(server, client);

            let (mut client, server) = tcp_pair().await;
            let oversized = raw_header(padding + 1);
            assert_eq!(oversized.len(), UPGRADE_LIMIT + 1);
            client.write_all(&oversized).await.unwrap();
            let error = authenticate(server, &[], &token, &stop)
                .await
                .err()
                .expect("oversized upgrade must fail");
            assert!(error.to_string().contains("upgrade exceeds 16 KiB"));
        });
    }

    #[test]
    fn actual_upgrade_rejects_wrong_token_and_oversized_hello_before_ready() {
        network_test(async {
            let token = "a".repeat(64);
            let stop = AtomicBool::new(false);
            for oversized in [false, true] {
                let (client, server) = tcp_pair().await;
                let server = async {
                    let error = authenticate(server, &[], &token, &stop)
                        .await
                        .err()
                        .expect("invalid hello must fail");
                    if oversized {
                        assert!(matches!(
                            error.downcast_ref::<tokio_tungstenite::tungstenite::Error>(),
                            Some(tokio_tungstenite::tungstenite::Error::Capacity(_))
                        ));
                    } else {
                        assert!(error.to_string().contains("SDK authentication failed"));
                    }
                };
                let client = async {
                    let url = format!("ws://{}/", client.peer_addr().unwrap());
                    let (mut socket, _) = client_async(url, client).await.unwrap();
                    let text = if oversized {
                        "x".repeat(sdk_wire::HELLO_LIMIT + 1)
                    } else {
                        hello(&"b".repeat(64))
                    };
                    if socket.send(Message::Text(text.into())).await.is_ok() {
                        assert!(matches!(
                            socket.next().await,
                            None | Some(Err(_)) | Some(Ok(Message::Close(_)))
                        ));
                    }
                };
                tokio::join!(server, client);
            }
        });
    }

    fn request(id: u32, method: &str, params: serde_json::Value) -> ClientJsonRpcMessage {
        serde_json::from_value(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .unwrap()
    }

    #[test]
    fn origins_are_default_denied_exact_and_http_only() {
        let request = |origin: Option<&str>| {
            let mut request = Request::builder().uri("/");
            if let Some(origin) = origin {
                request = request.header("Origin", origin);
            }
            request.body(()).unwrap()
        };
        assert!(origin_allowed(&request(None), &[]));
        assert!(!origin_allowed(&request(Some("https://trusted.test")), &[]));
        let origins = vec!["https://trusted.test".into()];
        validate_origins(&origins).unwrap();
        assert!(origin_allowed(
            &request(Some("https://trusted.test")),
            &origins
        ));
        for bad in [
            "null",
            "https://trusted.test.evil",
            "https://trusted.test:443",
            "https://trusted.test/",
        ] {
            assert!(!origin_allowed(&request(Some(bad)), &origins));
        }
        for bad in [
            "*",
            "https://*.example.test",
            "null",
            "file:///tmp",
            "https://a/path",
            "https://a/",
            "https://user@a",
            "https://a?x=1",
        ] {
            assert!(validate_origins(&[bad.into()]).is_err());
        }
        let duplicate = Request::builder()
            .header("Origin", "https://trusted.test")
            .header("Origin", "https://trusted.test")
            .body(())
            .unwrap();
        assert!(!origin_allowed(&duplicate, &origins));
    }

    #[test]
    fn admission_is_one_request_until_matching_response() {
        let mut admission = Admission::default();
        let initialize = request(
            1,
            "initialize",
            json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}),
        );
        admission.accept(&initialize).unwrap();
        assert!(admission.accept(&initialize).is_err());
        let wrong = serde_json::from_value(
            json!({"jsonrpc":"2.0","id":2,"error":{"code":-32600,"message":"wrong"}}),
        )
        .unwrap();
        assert!(!admission.complete(&wrong));
        assert!(admission.in_flight.is_some());
        let right = serde_json::from_value(
            json!({"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"done"}}),
        )
        .unwrap();
        assert!(admission.complete(&right));
        let initialized =
            serde_json::from_value(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                .unwrap();
        admission.accept(&initialized).unwrap();
        assert!(admission.accept(&initialized).is_err());
        admission
            .accept(&request(3, "tools/list", json!({})))
            .unwrap();
        assert!(
            admission
                .accept(&request(4, "tools/list", json!({})))
                .is_err()
        );
    }
}
