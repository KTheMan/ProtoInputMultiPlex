//! The official SDK owns negotiation, request IDs, framing and cancellation.
use crate::{Disconnect, Failure, Result, Rpc};
use rmcp::{
    RoleClient, ServiceExt,
    model::ClientRequest,
    service::{PeerRequestOptions, RunningService, ServiceError},
    transport::TokioChildProcess,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::PathBuf, process::Stdio, time::Duration};
use tokio::process::Command;

/// Do not derive Debug: this configuration contains a private descriptor path.
pub struct Config {
    pub executable: PathBuf,
    pub runtime: PathBuf,
    pub preferences: PathBuf,
    pub gui_connection: Option<PathBuf>,
}
pub struct StdioRpc {
    service: RunningService<RoleClient, ()>,
    deadline: Duration,
}
impl StdioRpc {
    /// Connect only to a GUI-approved window-only event descriptor. This is a
    /// separate MCP process from the ordinary routing and stopped-Setup modes.
    pub async fn connect_event(executable: PathBuf, descriptor: PathBuf) -> Result<Self> {
        if !executable.is_absolute() || !descriptor.is_absolute() {
            return Err(Failure::Scope);
        }
        let mut command = Command::new(executable);
        command
            .arg("mcp")
            .arg("--event-connection")
            .arg(descriptor)
            .kill_on_drop(true);
        let (transport, _) = TokioChildProcess::builder(command)
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| Failure::Transport)?;
        let service = tokio::time::timeout(Duration::from_secs(8), ().serve(transport))
            .await
            .map_err(|_| Failure::Timeout)?
            .map_err(|_| Failure::Transport)?;
        Ok(Self {
            service,
            deadline: Duration::from_secs(8),
        })
    }
    pub async fn connect(config: &Config) -> Result<Self> {
        if [&config.executable, &config.runtime, &config.preferences]
            .iter()
            .any(|p| !p.is_absolute())
            || config
                .gui_connection
                .as_ref()
                .is_some_and(|p| !p.is_absolute())
        {
            return Err(Failure::Scope);
        }
        let mut command = Command::new(&config.executable);
        command
            .arg("mcp")
            .arg("--runtime")
            .arg(&config.runtime)
            .arg("--preferences")
            .arg(&config.preferences)
            .kill_on_drop(true);
        if let Some(descriptor) = &config.gui_connection {
            command.arg("--gui-connection").arg(descriptor);
        }
        // Server stderr and SDK errors may include connection paths or tokens.
        // No tracing subscriber, inherited stderr, command dump or raw error log.
        let (transport, _) = TokioChildProcess::builder(command)
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| Failure::Transport)?;
        let service = tokio::time::timeout(Duration::from_secs(60), ().serve(transport))
            .await
            .map_err(|_| Failure::Timeout)?
            .map_err(|_| Failure::Transport)?;
        Ok(Self {
            service,
            deadline: Duration::from_secs(60),
        })
    }
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let request: ClientRequest =
            serde_json::from_value(json!({"method":method,"params":params}))
                .map_err(|_| Failure::Protocol)?;
        let response = self
            .service
            .peer()
            .send_request_with_option(request, PeerRequestOptions::with_timeout(self.deadline))
            .await
            .map_err(classify)?
            .await_response()
            .await
            .map_err(classify)?;
        serde_json::to_value(response).map_err(|_| Failure::Protocol)
    }
    pub async fn close(&mut self) -> Result<()> {
        match self
            .service
            .close_with_timeout(Duration::from_secs(8))
            .await
        {
            Ok(Some(_)) => Ok(()),
            _ => Err(Failure::Cleanup),
        }
    }
}
fn classify(error: ServiceError) -> Failure {
    match error {
        ServiceError::Timeout { .. } => Failure::Timeout,
        ServiceError::TransportClosed | ServiceError::TransportSend(_) => Failure::Transport,
        ServiceError::McpError(_) => Failure::Protocol,
        _ => Failure::Protocol,
    }
}
impl Disconnect for StdioRpc {
    async fn disconnect(&mut self) -> Result<()> {
        self.close().await
    }
}
impl Rpc for StdioRpc {
    async fn discover(&mut self) -> Result<Vec<Value>> {
        let mut tools = Vec::new();
        let mut cursor = None;
        let mut seen = BTreeSet::new();
        // Resource bounds, not a claim about the server's capability count.
        for _ in 0..64 {
            let params = cursor
                .as_ref()
                .map_or(json!({}), |value| json!({"cursor":value}));
            let page = self.request("tools/list", params).await?;
            tools.extend(
                page["tools"]
                    .as_array()
                    .ok_or(Failure::Protocol)?
                    .iter()
                    .cloned(),
            );
            if tools.len() > 2048 {
                return Err(Failure::Protocol);
            }
            match page.get("nextCursor").filter(|v| !v.is_null()) {
                None => return Ok(tools),
                Some(value) => {
                    let value = value.as_str().ok_or(Failure::Protocol)?.to_owned();
                    if !seen.insert(value.clone()) {
                        return Err(Failure::Protocol);
                    }
                    cursor = Some(value);
                }
            }
        }
        Err(Failure::Protocol)
    }
    async fn call(&mut self, name: &str, arguments: Value) -> Result<Value> {
        self.request("tools/call", json!({"name":name,"arguments":arguments}))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::{
        ErrorData, RoleServer, ServerHandler,
        model::{
            CallToolRequestParams, CallToolResponse, CallToolResult, CancelledNotificationParam,
            ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
        },
        service::{NotificationContext, RequestContext},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    #[derive(Clone)]
    struct Fixture {
        looping: bool,
        calls: Arc<AtomicUsize>,
        cancelled: Arc<AtomicBool>,
    }
    impl ServerHandler for Fixture {
        fn get_info(&self) -> ServerConfig {
            ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
        }
        async fn list_tools(
            &self,
            request: Option<PaginatedRequestParams>,
            _: RequestContext<RoleServer>,
        ) -> std::result::Result<ListToolsResult, ErrorData> {
            let second = request.and_then(|r| r.cursor).is_some();
            serde_json::from_value(json!({
                "tools":[{"name":if second {"second"} else {"first"},"inputSchema":{"type":"object"}}],
                "nextCursor":if second && !self.looping {Value::Null} else {json!("page-2")}
            })).map_err(|_| ErrorData::internal_error("fixture", None))
        }
        async fn call_tool(
            &self,
            request: CallToolRequestParams,
            _: RequestContext<RoleServer>,
        ) -> std::result::Result<CallToolResponse, ErrorData> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match request.name.as_ref() {
                "timeout" => {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    Ok(CallToolResult::structured(json!({"late":true})).into())
                }
                "protocol" => Err(ErrorData::internal_error("private-descriptor-token", None)),
                "deny" => Ok(CallToolResult::error(vec![rmcp::model::ContentBlock::text(
                    "private-descriptor-token denied",
                )])
                .into()),
                _ => Ok(CallToolResult::structured(json!({"acknowledged":true})).into()),
            }
        }
        async fn on_cancelled(
            &self,
            _: CancelledNotificationParam,
            _: NotificationContext<RoleServer>,
        ) {
            self.cancelled.store(true, Ordering::SeqCst);
        }
    }
    async fn pair(looping: bool) -> (StdioRpc, RunningService<RoleServer, Fixture>, Fixture) {
        let fixture = Fixture {
            looping,
            calls: Arc::default(),
            cancelled: Arc::default(),
        };
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let service_fixture = fixture.clone();
        let server = tokio::spawn(async move { service_fixture.serve(server_io).await.unwrap() });
        let client = ().serve(client_io).await.unwrap();
        (
            StdioRpc {
                service: client,
                deadline: Duration::from_millis(80),
            },
            server.await.unwrap(),
            fixture,
        )
    }
    #[tokio::test]
    async fn sdk_negotiates_and_discovers_paginated_tools() {
        let (mut client, mut server, _) = pair(false).await;
        let tools = client.discover().await.unwrap();
        assert_eq!(
            tools
                .iter()
                .map(|v| v["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        let reply = client.call("ack", json!({})).await.unwrap();
        assert_eq!(crate::tool_value(reply).unwrap()["acknowledged"], true);
        client.close().await.unwrap();
        server.close().await.unwrap();
    }
    #[tokio::test]
    async fn repeated_pagination_cursor_fails_closed() {
        let (mut client, mut server, _) = pair(true).await;
        assert_eq!(client.discover().await, Err(Failure::Protocol));
        client.close().await.unwrap();
        server.close().await.unwrap();
    }
    #[tokio::test]
    async fn sdk_keeps_tool_denial_separate_from_protocol_error() {
        let (mut client, mut server, _) = pair(false).await;
        assert_eq!(
            crate::tool_value(client.call("deny", json!({})).await.unwrap()),
            Err(Failure::Denied)
        );
        assert_eq!(
            client.call("protocol", json!({})).await,
            Err(Failure::Protocol)
        );
        client.close().await.unwrap();
        server.close().await.unwrap();
    }
    #[tokio::test]
    async fn sdk_timeout_cancels_request_without_replaying_it() {
        let (mut client, mut server, fixture) = pair(false).await;
        assert_eq!(
            client.call("timeout", json!({})).await,
            Err(Failure::Timeout)
        );
        tokio::time::timeout(Duration::from_secs(1), async {
            while !fixture.cancelled.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
        client.close().await.unwrap();
        server.close().await.unwrap();
    }
    #[tokio::test]
    async fn closed_connection_does_not_fall_back_or_retry() {
        let (mut client, mut server, fixture) = pair(false).await;
        server.close().await.unwrap();
        assert_eq!(client.call("ack", json!({})).await, Err(Failure::Transport));
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
        client.close().await.unwrap();
    }
}
