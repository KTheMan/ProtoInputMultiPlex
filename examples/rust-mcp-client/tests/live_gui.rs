//! Opt-in GUI policy acceptance. The coordinator owns the GUI and disposable
//! probe; this test starts only MCP transports and never launches a target.
use protoinputmux_mcp_example::{
    Client, Failure, Result, Rpc, Scope, reconnect_after_approval, tool_value,
    transport::{Config, StdioRpc},
    verify,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

const FIRST: &str = "rust-client-gui-a";
const SECOND: &str = "rust-client-gui-b";
const COUNTERS: [&str; 8] = [
    "key_event_count",
    "mouse_event_count",
    "text_events",
    "wheel_events",
    "horizontal_wheel_events",
    "native_event_count",
    "wm_print_count",
    "wm_printclient_count",
];

struct Settings {
    config: Config,
    pid: u32,
    x: i32,
    y: i32,
    report: PathBuf,
    sync: PathBuf,
}

fn require(condition: bool, failure: Failure) -> Result<()> {
    if condition { Ok(()) } else { Err(failure) }
}

fn path(name: &str) -> Result<PathBuf> {
    let value = PathBuf::from(std::env::var_os(name).ok_or(Failure::Scope)?);
    require(value.is_absolute(), Failure::Scope)?;
    Ok(value)
}

impl Settings {
    fn load() -> Result<Self> {
        require(
            std::env::var("PIM_GUI_SLOT_APPROVED").as_deref() == Ok("1"),
            Failure::Scope,
        )?;
        require(
            std::env::var("PIM_GUI_TEST_USER").as_deref() == Ok(FIRST),
            Failure::Scope,
        )?;
        let config = Config {
            executable: path("PIM_TEST_SERVER")?,
            runtime: path("PIM_TEST_RUNTIME")?,
            preferences: path("PIM_TEST_PREFERENCES")?,
            gui_connection: Some(path("PIM_GUI_CONNECTION")?),
        };
        let pid = std::env::var("PIM_GUI_TEST_PID")
            .ok()
            .and_then(|v| v.parse().ok())
            .ok_or(Failure::Scope)?;
        require(pid != 0, Failure::Scope)?;
        let x = std::env::var("PIM_GUI_TEST_X")
            .ok()
            .and_then(|v| v.parse().ok())
            .ok_or(Failure::Scope)?;
        let y = std::env::var("PIM_GUI_TEST_Y")
            .ok()
            .and_then(|v| v.parse().ok())
            .ok_or(Failure::Scope)?;
        let sync = path("PIM_GUI_TEST_SYNC")?;
        require(sync.is_dir(), Failure::Scope)?;
        for name in ["held.json", "approved-applied.json", "result.json"] {
            require(!sync.join(name).exists(), Failure::Scope)?;
        }
        Ok(Self {
            config,
            pid,
            x,
            y,
            report: path("PIM_GUI_TEST_REPORT")?,
            sync,
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Native {
    frames: u64,
    held_keys: Vec<u64>,
    buttons: u64,
    test_keys: u64,
    counts: [u64; 8],
}

impl Native {
    fn read(file: &Path, pid: u32) -> Option<Self> {
        let value: Value = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
        if value["pid"].as_u64()? != u64::from(pid) {
            return None;
        }
        let mut counts = [0; 8];
        for (index, name) in COUNTERS.iter().enumerate() {
            counts[index] = value[*name].as_u64()?;
        }
        Some(Self {
            frames: value["frames"].as_u64()?,
            held_keys: value["held_keys"]
                .as_array()?
                .iter()
                .map(Value::as_u64)
                .collect::<Option<Vec<_>>>()?,
            buttons: value["buttons"].as_u64()?,
            test_keys: value["test_keys"].as_u64()?,
            counts,
        })
    }
    fn released(&self) -> bool {
        self.held_keys.is_empty() && self.buttons == 0 && self.test_keys == 0
    }
    fn unchanged(&self, before: &Self) -> bool {
        self.held_keys == before.held_keys
            && self.buttons == before.buttons
            && self.test_keys == before.test_keys
            && self.counts == before.counts
    }
}

async fn observe(settings: &Settings, accept: impl Fn(&Native) -> bool) -> Result<Native> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(snapshot) = Native::read(&settings.report, settings.pid)
                && accept(&snapshot)
            {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| Failure::Timeout)
}

fn preferences(file: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(file) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(Failure::Transport),
    }
}

fn write_json(file: &Path, value: Value) -> Result<()> {
    let temporary = file.with_extension("pending");
    std::fs::write(
        &temporary,
        serde_json::to_vec(&value).map_err(|_| Failure::Protocol)?,
    )
    .map_err(|_| Failure::Transport)?;
    std::fs::rename(temporary, file).map_err(|_| Failure::Transport)
}

fn policy(session: &Value, pid: u32, expanded: bool) -> Result<u64> {
    require(
        session["mode"] == "gui_connection"
            && session["connected"] == true
            && session["connection"]["connected"] == true
            && session["running"] == true,
        Failure::Scope,
    )?;
    let policy = &session["connection"]["policy"];
    let ids = policy["user_ids"].as_array().ok_or(Failure::Protocol)?;
    require(
        policy["enabled"] == true
            && policy["allowed_pids"] == json!([pid])
            && policy["capture"] == false
            && policy["session_control"] == false
            && policy["app_preferences"] == false
            && ids.len() == if expanded { 2 } else { 1 }
            && ids.iter().any(|id| id == FIRST)
            && (!expanded || ids.iter().any(|id| id == SECOND)),
        Failure::Scope,
    )?;
    session["connection"]["client"]
        .as_u64()
        .filter(|id| *id != 0)
        .ok_or(Failure::Protocol)
}

fn identities(state: &Value) -> Result<()> {
    let identities = state["identities"].as_array().ok_or(Failure::Protocol)?;
    for (id, user) in [(FIRST, 0), (SECOND, 1)] {
        require(
            identities
                .iter()
                .any(|seat| seat["id"] == id && seat["user"] == user && seat["active"] == true),
            Failure::Scope,
        )?;
    }
    Ok(())
}

async fn approved(settings: &Settings) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Ok(bytes) = std::fs::read(settings.sync.join("approved-applied.json"))
                && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
                && value["approved"] == true
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| Failure::Timeout)
}

async fn exercise(
    settings: &Settings,
    old: &mut Option<Client<StdioRpc>>,
    fresh: &mut Option<Client<StdioRpc>>,
    evidence: &mut Value,
) -> Result<()> {
    *old = Some(Client::discover(StdioRpc::connect(&settings.config).await?).await?);
    let client = old.as_mut().ok_or(Failure::Protocol)?;
    let session = client.inspect().await?;
    let old_id = policy(&session, settings.pid, false)?;
    evidence["old_client_id"] = json!(old_id);
    let initial = client.call("routing_state", json!({})).await?;
    identities(&initial)?;
    verify(&initial, 0, settings.pid)?;
    let before_preferences = preferences(&settings.config.preferences)?;
    let mut before = observe(settings, Native::released).await?;
    let configure = json!({"operation":"configure","settings":{
        "launch_minimized":false,"minimize_to_tray":false,"close_to_tray":false,"shortcuts":[]}});
    for (name, args) in [
        ("routing_capture", json!({"user":0})),
        ("routing_input", json!({"action":"pause"})),
        ("app_preferences", configure),
        (
            "routing_input",
            json!({"action":"move_to","user":1,"x":settings.x,"y":settings.y}),
        ),
    ] {
        client.validate(name, &args)?;
        let reply = client.rpc.call(name, args).await?;
        require(
            !reply["content"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["type"] == "image")),
            Failure::Verification,
        )?;
        require(
            tool_value(reply) == Err(Failure::Denied),
            Failure::Verification,
        )?;
        let state = client.call("routing_state", json!({})).await?;
        require(
            state["running"] == true && state["paused"] == false,
            Failure::Verification,
        )?;
        let after = observe(settings, |value| value.frames > before.frames).await?;
        require(
            after.unchanged(&before)
                && preferences(&settings.config.preferences)? == before_preferences,
            Failure::Verification,
        )?;
        before = after;
    }
    evidence["denials"] = json!({"capture":true,"session_control":true,"app_preferences":true,"unpermitted_user":true,"no_image":true,"unchanged_preferences":true,"unchanged_native":true});
    // Preflight release before dispatching a hold which may need cleanup.
    client.validate("routing_input", &json!({"action":"release_all"}))?;
    client.validate("routing_stop", &json!({}))?;
    client
        .call(
            "routing_input",
            json!({"action":"key","user":0,"key":"F9","pressed":true}),
        )
        .await?;
    let held = observe(settings, |value| {
        value.held_keys == [120] && value.buttons == 0 && value.counts[0] > before.counts[0]
    })
    .await?;
    evidence["hold_observed"] = json!(true);
    write_json(
        &settings.sync.join("held.json"),
        json!({"old_client_id":old_id,
        "denials":evidence["denials"],"hold_observed":true,"key_event_count":held.counts[0]}),
    )?;
    approved(settings).await?;
    let released = observe(settings, |value| {
        value.frames > held.frames && value.released()
    })
    .await?;
    evidence["policy_change_released_hold"] = json!(true);
    // This is the sole reconnect factory call. It never writes policy or replays
    // the uncertain key; fresh discovery and inspection happen in the helper.
    *fresh =
        Some(reconnect_after_approval(client, true, || StdioRpc::connect(&settings.config)).await?);
    let client = fresh.as_mut().ok_or(Failure::Protocol)?;
    let session = client.inspect().await?;
    let new_id = policy(&session, settings.pid, true)?;
    require(new_id != old_id, Failure::Verification)?;
    evidence["new_client_id"] = json!(new_id);
    let state = client.call("routing_state", json!({})).await?;
    identities(&state)?;
    verify(&state, 0, settings.pid)?;
    let after_reconnect = observe(settings, |value| value.frames > released.frames).await?;
    require(after_reconnect.unchanged(&released), Failure::Verification)?;
    evidence["reconnect_no_replay"] = json!(true);
    client
        .demo(&Scope {
            pid: settings.pid,
            user_id: Some(FIRST.to_owned()),
            x: settings.x,
            y: settings.y,
        })
        .await?;
    let completed = observe(settings, |value| {
        value.released()
            && value.counts[0] > after_reconnect.counts[0]
            && value.counts[1] > after_reconnect.counts[1]
    })
    .await?;
    let disconnected = client.call("agent_session", json!({})).await?;
    require(
        disconnected["running"] == true
            && disconnected["connected"] == false
            && disconnected["connection"]["connected"] == false,
        Failure::Verification,
    )?;
    require(
        preferences(&settings.config.preferences)? == before_preferences,
        Failure::Verification,
    )?;
    evidence["gui_running_after_disconnect"] = json!(true);
    evidence["key_event_count_before"] = json!(before.counts[0]);
    evidence["key_event_count_after"] = json!(completed.counts[0]);
    evidence["mouse_event_count_before"] = json!(before.counts[1]);
    evidence["mouse_event_count_after"] = json!(completed.counts[1]);
    evidence["held_keys_empty"] = json!(completed.held_keys.is_empty());
    evidence["buttons_empty"] = json!(completed.buttons == 0);
    Ok(())
}

async fn cleanup(client: &mut Option<Client<StdioRpc>>, failed: bool) -> Result<()> {
    let Some(client) = client else {
        return Ok(());
    };
    if failed {
        // Only this connection's owned input is released; stop disconnects this
        // GUI client. Both attempts remain bounded even after revocation.
        for (name, args) in [
            ("routing_input", json!({"action":"release_all"})),
            ("routing_stop", json!({})),
        ] {
            let _ = tokio::time::timeout(Duration::from_secs(4), client.call(name, args)).await;
        }
    }
    tokio::time::timeout(Duration::from_secs(10), client.rpc.close())
        .await
        .map_err(|_| Failure::Cleanup)?
}

#[tokio::test]
#[ignore = "native GUI policy acceptance: coordinator-owned disposable probe/GUI, private connection, PIM_GUI_SLOT_APPROVED=1 required"]
async fn coordinated_gui_denial_revocation_and_approved_reconnect() {
    let settings =
        Settings::load().expect("live GUI test prerequisites not satisfied (details withheld)");
    let mut old = None;
    let mut fresh = None;
    let mut evidence = json!({"pid":settings.pid});
    let mut outcome = exercise(&settings, &mut old, &mut fresh, &mut evidence).await;
    let failed = outcome.is_err();
    let fresh_closed = cleanup(&mut fresh, failed).await;
    let old_closed = cleanup(&mut old, failed).await;
    if outcome.is_ok() && (fresh_closed.is_err() || old_closed.is_err()) {
        outcome = Err(Failure::Cleanup);
    }
    evidence["result"] = json!(if outcome.is_ok() { "PASS" } else { "FAIL" });
    if let Err(failure) = outcome {
        evidence["failure"] = json!(format!("{failure:?}"));
    }
    let recorded = write_json(&settings.sync.join("result.json"), evidence);
    assert!(
        outcome.is_ok() && recorded.is_ok(),
        "live GUI contract failed; sanitized result recorded when available"
    );
}
