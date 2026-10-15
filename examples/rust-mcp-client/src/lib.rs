//! A deliberately small workflow client. Remote strings are never diagnostics.
use serde_json::{Value, json};
use std::{collections::BTreeMap, future::Future};

pub mod transport;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Denied,
    Revoked,
    Timeout,
    Transport,
    Protocol,
    Schema,
    Scope,
    Verification,
    Cleanup,
}
pub type Result<T> = std::result::Result<T, Failure>;

pub trait Rpc {
    fn discover(&mut self) -> impl Future<Output = Result<Vec<Value>>>;
    fn call(&mut self, name: &str, arguments: Value) -> impl Future<Output = Result<Value>>;
}

pub trait Disconnect: Rpc {
    fn disconnect(&mut self) -> impl Future<Output = Result<()>>;
}

/// Explicit owner approval is a prerequisite, never inferred from a timeout.
/// A new process must rediscover and inspect; this function never replays input.
pub async fn reconnect_after_approval<R: Disconnect, F, Fut>(
    old: &mut Client<R>,
    approved: bool,
    connect: F,
) -> Result<Client<R>>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<R>>,
{
    if !approved {
        return Err(Failure::Scope);
    }
    match old.call("routing_state", json!({})).await {
        Err(Failure::Denied | Failure::Revoked | Failure::Transport) => {}
        Err(error) => return Err(error),
        Ok(_) => return Err(Failure::Verification),
    }
    old.rpc.disconnect().await?;
    let mut fresh = Client::discover(connect().await?).await?;
    let session = fresh.inspect().await?;
    if session["mode"] != "gui_connection"
        || session["connected"] != true
        || session["connection"]["connected"] != true
        || session["connection"]["policy"]["enabled"] != true
    {
        return Err(Failure::Scope);
    }
    Ok(fresh)
}

/// All tool errors are terminal for the current workflow. Text only classifies
/// recovery; it never authorizes retry or changes to policy.
pub fn tool_value(reply: Value) -> Result<Value> {
    if reply.get("isError").and_then(Value::as_bool) == Some(true) {
        let text = reply["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| item["text"].as_str())
            .collect::<String>()
            .to_lowercase();
        return Err(
            if text.contains("revok")
                || text.contains("connection is closed")
                || text.contains("connection ended")
                || text.contains("not connected")
            {
                Failure::Revoked
            } else {
                Failure::Denied
            },
        );
    }
    if let Some(value) = reply.get("structuredContent") {
        return Ok(value.clone());
    }
    let content = reply["content"].as_array().ok_or(Failure::Protocol)?;
    if content.len() != 1 || content[0]["type"] != "text" {
        return Err(Failure::Protocol);
    }
    serde_json::from_str(content[0]["text"].as_str().ok_or(Failure::Protocol)?)
        .map_err(|_| Failure::Protocol)
}

pub struct Client<R> {
    pub rpc: R,
    schemas: BTreeMap<String, jsonschema::Validator>,
}
impl<R: Rpc> Client<R> {
    pub async fn discover(mut rpc: R) -> Result<Self> {
        let mut schemas = BTreeMap::new();
        for tool in rpc.discover().await? {
            let name = tool["name"].as_str().ok_or(Failure::Protocol)?;
            let schema = tool.get("inputSchema").ok_or(Failure::Schema)?;
            // No external HTTP/file resolution is enabled. Unsupported schema
            // dialects/references fail closed rather than weakening validation.
            let validator = jsonschema::validator_for(schema).map_err(|_| Failure::Schema)?;
            if schemas.insert(name.to_owned(), validator).is_some() {
                return Err(Failure::Protocol);
            }
        }
        Ok(Self { rpc, schemas })
    }
    pub fn tool_count(&self) -> usize {
        self.schemas.len()
    }
    pub fn validate(&self, name: &str, args: &Value) -> Result<()> {
        let schema = self.schemas.get(name).ok_or(Failure::Schema)?;
        if !schema.is_valid(args) {
            return Err(Failure::Schema);
        }
        Ok(())
    }
    pub async fn call(&mut self, name: &str, args: Value) -> Result<Value> {
        self.validate(name, &args)?;
        tool_value(self.rpc.call(name, args).await?)
    }
    pub async fn inspect(&mut self) -> Result<Value> {
        let session = self.call("agent_session", json!({})).await?;
        self.call("routing_windows", json!({})).await?;
        self.call("routing_state", json!({})).await?;
        Ok(session)
    }
    pub async fn document_checks(&mut self) -> Result<()> {
        let before = self.call("routing_state", json!({})).await?;
        if before["running"] != false {
            return Err(Failure::Scope);
        }
        let document = r#"{"version":1,"seats":[]}"#;
        let checked = self
            .call(
                "profile_document",
                json!({"operation":"validate", "document":document}),
            )
            .await?;
        if checked["valid"] != true || checked["seat_count"] != 0 {
            return Err(Failure::Verification);
        }
        let normalized = self
            .call(
                "profile_document",
                json!({"operation":"normalize", "document":document}),
            )
            .await?;
        let exported = self
            .call(
                "profile_document",
                json!({"operation":"export", "document":normalized["json"]}),
            )
            .await?;
        let left: Value =
            serde_json::from_str(normalized["json"].as_str().ok_or(Failure::Protocol)?)
                .map_err(|_| Failure::Protocol)?;
        let right: Value =
            serde_json::from_str(exported["json"].as_str().ok_or(Failure::Protocol)?)
                .map_err(|_| Failure::Protocol)?;
        if left != right {
            return Err(Failure::Verification);
        }
        for (name, arguments) in [
            (
                "profile_document",
                json!({"operation":"validate","document":"{\"version\":999,\"seats\":[]}"}),
            ),
            (
                "routing_start",
                json!({"users":["Client example"],"allowed_pids":[],"mode":"concurrent","multi_keyboard":true}),
            ),
            (
                "app_preferences",
                json!({"operation":"configure","settings":{"launch_minimized":false,"minimize_to_tray":false,"close_to_tray":false,"shortcuts":[]}}),
            ),
        ] {
            if self.call(name, arguments).await != Err(Failure::Denied) {
                return Err(Failure::Verification);
            }
        }
        if self.call("routing_state", json!({})).await?["running"] != false {
            return Err(Failure::Verification);
        }
        Ok(())
    }

    /// Only the GUI owner can change policy. This method never does so.
    pub async fn demo(&mut self, scope: &Scope) -> Result<()> {
        // Mark cleanup necessary before dispatch: a timed-out Start may have run.
        let mut cleanup_needed = false;
        let result = self.demo_inner(scope, &mut cleanup_needed).await;
        if cleanup_needed {
            let released = self
                .call("routing_input", json!({"action":"release_all"}))
                .await;
            let stopped = self.call("routing_stop", json!({})).await;
            // Cleanup is attempted even after a denial/revocation. Never replay input.
            let stop_confirmed = stopped.as_ref().is_ok_and(|state| {
                if scope.user_id.is_some() {
                    state["running"] == true && state["agent_session"]["connected"] == false
                } else {
                    state["running"] == false
                }
            });
            if !released.as_ref().is_ok_and(|state| state.is_object()) || !stop_confirmed {
                return Err(Failure::Cleanup);
            }
        }
        result
    }
    async fn demo_inner(&mut self, scope: &Scope, cleanup_needed: &mut bool) -> Result<()> {
        if scope.pid == 0 {
            return Err(Failure::Scope);
        }
        let session = self.call("agent_session", json!({})).await?;
        let gui = match session["mode"].as_str() {
            Some("gui_connection") => true,
            Some("standalone") => false,
            _ => return Err(Failure::Protocol),
        };
        let windows = self.call("routing_windows", json!({})).await?;
        let eligible = windows["windows"]
            .as_array()
            .ok_or(Failure::Protocol)?
            .iter()
            .any(|w| {
                w["pid"].as_u64() == Some(u64::from(scope.pid))
                    && inside(&w["client"], scope.x, scope.y)
            });
        if !eligible {
            return Err(Failure::Scope);
        }
        let state = self.call("routing_state", json!({})).await?;
        let user = if gui {
            let id = scope.user_id.as_deref().ok_or(Failure::Scope)?;
            let policy = &session["connection"]["policy"];
            if session["connected"] != true
                || session["connection"]["connected"] != true
                || policy["enabled"] != true
                || !policy["user_ids"]
                    .as_array()
                    .is_some_and(|ids| ids.iter().any(|v| v == id))
                || !policy["allowed_pids"].as_array().is_some_and(|pids| {
                    pids.len() == 1 && pids[0].as_u64() == Some(u64::from(scope.pid))
                })
            {
                return Err(Failure::Scope);
            }
            let identity = state["identities"]
                .as_array()
                .ok_or(Failure::Protocol)?
                .iter()
                .find(|v| v["id"] == id && v["active"] == true)
                .ok_or(Failure::Scope)?;
            let user = identity["user"].as_u64().ok_or(Failure::Protocol)?;
            let seat = state["users"]
                .as_array()
                .and_then(|seats| usize::try_from(user).ok().and_then(|i| seats.get(i)))
                .ok_or(Failure::Scope)?;
            if state["running"] != true
                || state["paused"] != false
                || state.get("last_error") != Some(&Value::Null)
                || seat["buttons"].as_array().is_none_or(|v| !v.is_empty())
                || seat["keys"].as_array().is_none_or(|v| !v.is_empty())
                || seat["captured"] == true
            {
                return Err(Failure::Scope);
            }
            user
        } else {
            if scope.user_id.is_some() || state["running"] != false {
                return Err(Failure::Scope);
            }
            0
        };
        // Validate every planned mutation and cleanup against this connection's
        // discovered schema before dispatching the first mutation.
        let start = json!({"users":["Client example"],"allowed_pids":[scope.pid],"mode":"concurrent","multi_keyboard":true});
        if !gui {
            self.validate("routing_start", &start)?;
        }
        self.validate("routing_stop", &json!({}))?;
        for args in [
            json!({"action":"release_all"}),
            json!({"action":"move_to","user":user,"x":scope.x,"y":scope.y}),
            json!({"action":"button","user":user,"button":"primary","pressed":true}),
            json!({"action":"button","user":user,"button":"primary","pressed":false}),
            json!({"action":"key","user":user,"key":"F9","pressed":true}),
            json!({"action":"key","user":user,"key":"F9","pressed":false}),
        ] {
            self.validate("routing_input", &args)?;
        }
        *cleanup_needed = true;
        if !gui {
            self.call("routing_start", start).await?;
        }
        let moved = self
            .call(
                "routing_input",
                json!({"action":"move_to","user":user,"x":scope.x,"y":scope.y}),
            )
            .await?;
        let moved_seat = moved["users"]
            .as_array()
            .and_then(|seats| usize::try_from(user).ok().and_then(|i| seats.get(i)))
            .ok_or(Failure::Verification)?;
        if moved["running"] != true
            || moved["paused"] != false
            || moved.get("last_error") != Some(&Value::Null)
            || moved_seat["overlay"]["x"] != scope.x
            || moved_seat["overlay"]["y"] != scope.y
            || moved_seat["buttons"]
                .as_array()
                .is_none_or(|v| !v.is_empty())
            || moved_seat["keys"].as_array().is_none_or(|v| !v.is_empty())
        {
            return Err(Failure::Verification);
        }
        self.call(
            "routing_input",
            json!({"action":"button","user":user,"button":"primary","pressed":true}),
        )
        .await?;
        let clicked = self
            .call(
                "routing_input",
                json!({"action":"button","user":user,"button":"primary","pressed":false}),
            )
            .await?;
        verify(&clicked, user, scope.pid)?;
        self.call(
            "routing_input",
            json!({"action":"key","user":user,"key":"F9","pressed":true}),
        )
        .await?;
        let acknowledged = self
            .call(
                "routing_input",
                json!({"action":"key","user":user,"key":"F9","pressed":false}),
            )
            .await?;
        verify(&acknowledged, user, scope.pid)?;
        self.call("routing_input", json!({"action":"release_all"}))
            .await?;
        verify(
            &self.call("routing_state", json!({})).await?,
            user,
            scope.pid,
        )
    }
}

pub struct Scope {
    pub pid: u32,
    pub user_id: Option<String>,
    pub x: i32,
    pub y: i32,
}
fn inside(rect: &Value, x: i32, y: i32) -> bool {
    let [Some(l), Some(t), Some(r), Some(b)] = [
        rect["left"].as_i64(),
        rect["top"].as_i64(),
        rect["right"].as_i64(),
        rect["bottom"].as_i64(),
    ] else {
        return false;
    };
    l <= i64::from(x) && i64::from(x) < r && t <= i64::from(y) && i64::from(y) < b
}
pub fn verify(state: &Value, user: u64, pid: u32) -> Result<()> {
    let seat = state["users"]
        .as_array()
        .and_then(|v| usize::try_from(user).ok().and_then(|i| v.get(i)))
        .ok_or(Failure::Verification)?;
    if state["running"] != true
        || state["paused"] != false
        || state.get("last_error") != Some(&Value::Null)
        || seat["pid"].as_u64() != Some(u64::from(pid))
        || seat["keyboard_pid"].as_u64() != Some(u64::from(pid))
        || seat["buttons"].as_array().is_none_or(|v| !v.is_empty())
        || seat["keys"].as_array().is_none_or(|v| !v.is_empty())
    {
        return Err(Failure::Verification);
    }
    // Routing status is free-form diagnostic text; inspect only known failure
    // markers, never display it or treat it as an instruction.
    let status = seat["status"]
        .as_str()
        .ok_or(Failure::Verification)?
        .to_lowercase();
    if [
        "block",
        "denied",
        "error",
        "failed",
        "waiting",
        "stopped",
        "unavailable",
        "exited",
        "closed",
        "hidden",
        "no eligible",
    ]
    .iter()
    .any(|v| status.contains(v))
    {
        return Err(Failure::Verification);
    }
    Ok(())
}
