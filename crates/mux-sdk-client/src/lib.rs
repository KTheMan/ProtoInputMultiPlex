//! Rust client for ProtoInputMultiPlex's opt-in loopback WebSocket command SDK.
//!
//! Each connection has independent GUI grants and input ownership. A failed or
//! timed-out mutation is never replayed: reconnect and inspect before deciding
//! what to do next. Closing a socket requests server-side client cleanup; it is
//! not an acknowledgement that native releases have finished.
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{fmt, path::Path, time::Duration};
use tokio::{io::AsyncReadExt, net::TcpStream, time::timeout};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};

pub const MESSAGE_LIMIT: usize = 17 * 1024 * 1024;
// MCP structured results also include a JSON text copy. Pretty profile JSON
// can therefore expand through nested escaping and duplicated content.
pub const RESULT_LIMIT: usize = 128 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(15);
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Endpoint(String),
    Protocol(&'static str),
    Transport(String),
    /// The server may have accepted the mutation before the response was lost.
    Uncertain(String),
    /// A confirmed JSON-RPC error; tool-level errors remain in the tool result.
    Remote(Value),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Endpoint(message) => write!(f, "SDK endpoint: {message}"),
            Self::Protocol(message) => write!(f, "SDK protocol: {message}"),
            Self::Transport(message) => write!(f, "SDK transport: {message}"),
            Self::Uncertain(message) => write!(
                f,
                "SDK outcome uncertain: {message}; inspect before retrying"
            ),
            Self::Remote(value) => write!(f, "SDK JSON-RPC error: {value}"),
        }
    }
}
impl std::error::Error for Error {}

// Intentionally no Debug implementation: the private bearer must not enter logs.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Endpoint {
    protoinputmultiplex_sdk: u8,
    port: u16,
    token: String,
}

impl Endpoint {
    fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 4096 {
            return Err(Error::Protocol("endpoint is too large"));
        }
        let endpoint: Self = serde_json::from_slice(bytes)
            .map_err(|_| Error::Protocol("invalid endpoint document"))?;
        if endpoint.protoinputmultiplex_sdk != 1
            || endpoint.port == 0
            || endpoint.token.len() != 64
            || !endpoint.token.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::Protocol("unsupported or invalid endpoint"));
        }
        Ok(endpoint)
    }
}

/// A sequential connection. This type never reconnects or retries a command.
pub struct Client {
    socket: Option<Socket>,
    next_id: u64,
}

impl Client {
    pub async fn connect(endpoint_file: impl AsRef<Path>) -> Result<Self> {
        let file = tokio::fs::File::open(endpoint_file)
            .await
            .map_err(|error| Error::Endpoint(error.to_string()))?;
        let mut bytes = Vec::new();
        file.take(4097)
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| Error::Endpoint(error.to_string()))?;
        let endpoint = Endpoint::decode(&bytes)?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(RESULT_LIMIT))
            .max_frame_size(Some(RESULT_LIMIT))
            .write_buffer_size(64 * 1024)
            .max_write_buffer_size(MESSAGE_LIMIT + 64 * 1024);
        // A numeric loopback URL is constructed locally; endpoint files cannot
        // redirect the bearer to a hostname, remote address, or alternate path.
        let url = format!("ws://127.0.0.1:{}", endpoint.port);
        let (mut socket, _) = timeout(
            DEADLINE,
            connect_async_with_config(url, Some(config), false),
        )
        .await
        .map_err(|_| Error::Transport("connection timeout".into()))?
        .map_err(|_| Error::Transport("connection failed".into()))?;
        let hello = json!({"protoinputmultiplex_sdk":1,"type":"hello","token":endpoint.token});
        timeout(
            DEADLINE,
            socket.send(Message::Text(hello.to_string().into())),
        )
        .await
        .map_err(|_| Error::Transport("hello timeout".into()))?
        .map_err(|_| Error::Transport("hello failed".into()))?;
        let ready = timeout(DEADLINE, socket.next())
            .await
            .map_err(|_| Error::Transport("authentication timeout".into()))?
            .ok_or_else(|| Error::Transport("authentication connection closed".into()))?
            .map_err(|_| Error::Transport("authentication failed".into()))?;
        if !matches!(ready, Message::Text(ref text)
            if text.len() <= 4096 && serde_json::from_str::<Value>(text).ok()
                == Some(json!({"protoinputmultiplex_sdk":1,"type":"ready"})))
        {
            return Err(Error::Protocol("expected SDK ready acknowledgement"));
        }
        let mut client = Self {
            socket: Some(socket),
            next_id: 1,
        };
        client.rpc("initialize", json!({
            "protocolVersion":"2025-11-25", "capabilities":{},
            "clientInfo":{"name":"protoinputmultiplex-rust-sdk","version":env!("CARGO_PKG_VERSION")}
        }), false).await?;
        let initialized = Message::Text(
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
                .to_string()
                .into(),
        );
        timeout(
            DEADLINE,
            client
                .socket
                .as_mut()
                .expect("connected socket")
                .send(initialized),
        )
        .await
        .map_err(|_| Error::Transport("initialization timeout".into()))?
        .map_err(|_| Error::Transport("initialization failed".into()))?;
        Ok(client)
    }

    pub async fn list_commands(&mut self) -> Result<Value> {
        self.rpc("tools/list", json!({}), false).await
    }

    /// Call any discovered tool with its existing MCP arguments and permissions.
    pub async fn call(&mut self, name: &str, arguments: Value) -> Result<Value> {
        self.rpc(
            "tools/call",
            json!({"name":name,"arguments":arguments}),
            true,
        )
        .await
    }

    pub async fn inspect(&mut self) -> Result<Value> {
        self.call("routing_state", json!({})).await
    }

    /// In GUI mode this retires this client, not the GUI-owned routing session.
    pub async fn stop(&mut self) -> Result<Value> {
        self.call("routing_stop", json!({})).await
    }

    pub async fn disconnect(mut self) -> Result<()> {
        let Some(mut socket) = self.socket.take() else {
            return Ok(());
        };
        timeout(DEADLINE, socket.close(None))
            .await
            .map_err(|_| Error::Transport("close timeout; cleanup unconfirmed".into()))?
            .map_err(|_| Error::Transport("close failed; cleanup unconfirmed".into()))
    }

    async fn rpc(&mut self, method: &str, params: Value, mutation: bool) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(Error::Protocol("request ID exhausted"))?;
        let text = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string();
        if text.len() > MESSAGE_LIMIT {
            return Err(Error::Protocol(
                "encoded command exceeds the SDK frame limit",
            ));
        }
        let socket = self
            .socket
            .as_mut()
            .ok_or(Error::Protocol("disconnected; reconnect and inspect"))?;
        let exchange = async {
            socket
                .send(Message::Text(text.into()))
                .await
                .map_err(|_| "request write failed")?;
            for _ in 0..64 {
                let frame = socket
                    .next()
                    .await
                    .ok_or("connection closed")?
                    .map_err(|_| "response read failed")?;
                match frame {
                    Message::Text(text) => {
                        let response: Value =
                            serde_json::from_str(&text).map_err(|_| "invalid response JSON")?;
                        if response.get("method").is_some() && response.get("id").is_none() {
                            continue; // Bounded notification handling, no automatic commands.
                        }
                        if response.get("jsonrpc") != Some(&json!("2.0"))
                            || response.get("id") != Some(&json!(id))
                        {
                            return Err("unexpected response ID or version");
                        }
                        return Ok(response);
                    }
                    Message::Ping(_) | Message::Pong(_) => continue,
                    _ => return Err("unexpected frame or closed connection"),
                }
            }
            Err("too many frames without a matching response")
        };
        let outcome = match timeout(DEADLINE, exchange).await {
            Ok(result) => result,
            Err(_) => Err("response timeout"),
        };
        match outcome {
            Ok(response) => match decode_response(response) {
                Err(error @ Error::Uncertain(_)) => {
                    self.socket.take();
                    Err(error)
                }
                Err(Error::Protocol(message)) => {
                    self.socket.take();
                    if mutation {
                        Err(Error::Uncertain(message.into()))
                    } else {
                        Err(Error::Protocol(message))
                    }
                }
                result => result,
            },
            Err(message) => {
                // Drop this transport so a late response cannot be mistaken for
                // a future command. Accepted server work may still finish.
                self.socket.take();
                if mutation {
                    Err(Error::Uncertain(message.into()))
                } else {
                    Err(Error::Transport(message.into()))
                }
            }
        }
    }
}

fn decode_response(mut response: Value) -> Result<Value> {
    match (response.get("result"), response.get("error")) {
        (Some(_), None) => Ok(response["result"].take()),
        (None, Some(error))
            if error["code"] == -32001 && error["data"]["outcome_uncertain"] == true =>
        {
            Err(Error::Uncertain(
                "accepted command result exceeded the SDK response limit".into(),
            ))
        }
        (None, Some(_)) => Err(Error::Remote(response["error"].take())),
        _ => Err(Error::Protocol(
            "response must contain exactly one result or error",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_cannot_redirect_bearer_or_add_authority() {
        let valid = json!({"protoinputmultiplex_sdk":1,"port":43210,"token":"a".repeat(64)});
        assert!(Endpoint::decode(valid.to_string().as_bytes()).is_ok());
        for (field, value) in [
            ("host", json!("example.com")),
            ("port", json!(0)),
            ("token", json!("invalid")),
            ("protoinputmultiplex_sdk", json!(2)),
        ] {
            let mut bad = valid.clone();
            bad[field] = value;
            assert!(Endpoint::decode(bad.to_string().as_bytes()).is_err());
        }
    }

    #[test]
    fn result_error_and_uncertain_outcomes_stay_distinct() {
        assert_eq!(
            decode_response(json!({"result":{"isError":true}})).unwrap(),
            json!({"isError":true})
        );
        assert!(matches!(
            decode_response(json!({"error":{"code":-1}})),
            Err(Error::Remote(_))
        ));
        assert!(matches!(
            decode_response(json!({"result":{},"error":{}})),
            Err(Error::Protocol(_))
        ));
        assert!(matches!(
            decode_response(json!({"error":{"code":-32001,"data":{"outcome_uncertain":true}}})),
            Err(Error::Uncertain(_))
        ));
        assert!(
            Error::Uncertain("connection closed".into())
                .to_string()
                .contains("inspect before retrying")
        );
    }

    #[tokio::test]
    async fn lost_mutation_response_is_uncertain_and_never_replayed() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use tokio::net::TcpListener;
        use tokio_tungstenite::accept_async;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&accepted);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let hello = socket.next().await.unwrap().unwrap().into_text().unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&hello).unwrap()["type"],
                "hello"
            );
            socket
                .send(Message::Text(
                    json!({"protoinputmultiplex_sdk":1,"type":"ready"})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            let initialize = socket.next().await.unwrap().unwrap().into_text().unwrap();
            let initialize: Value = serde_json::from_str(&initialize).unwrap();
            socket
                .send(Message::Text(
                    json!({"jsonrpc":"2.0","id":initialize["id"],"result":{}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            let notification = socket.next().await.unwrap().unwrap().into_text().unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&notification).unwrap()["method"],
                "notifications/initialized"
            );
            let mutation = socket.next().await.unwrap().unwrap().into_text().unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&mutation).unwrap()["method"],
                "tools/call"
            );
            counted.fetch_add(1, Ordering::SeqCst);
            // Accepted work, deliberately no response. Drop closes this socket
            // and listener; the client must not invent a confirmed outcome.
        });
        let path = std::env::temp_dir().join(format!(
            "mux-sdk-owned-test-{}-{port}.json",
            std::process::id()
        ));
        let document = json!({"protoinputmultiplex_sdk":1,"port":port,"token":"a".repeat(64)});
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut file, document.to_string().as_bytes()).unwrap();
        drop(file);
        let connected = Client::connect(&path).await;
        std::fs::remove_file(&path).unwrap();
        let mut client = connected.unwrap();
        assert!(matches!(
            client.call("routing_input", json!({"fixture":true})).await,
            Err(Error::Uncertain(_))
        ));
        assert!(matches!(
            client.call("routing_input", json!({"fixture":true})).await,
            Err(Error::Protocol(_))
        ));
        server.await.unwrap();
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
    }
}
