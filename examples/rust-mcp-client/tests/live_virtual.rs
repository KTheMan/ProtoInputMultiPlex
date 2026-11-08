//! Explicitly gated native acceptance using one fresh disposable probe only.
//! Coordinate the native slot before enabling this test. No capture or hardware.
use protoinputmultiplex_mcp_example::{
    Client, Result, Rpc, Scope, tool_value,
    transport::{Config, StdioRpc},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

struct OwnedProbe(Child);
impl Drop for OwnedProbe {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Observed {
    inner: StdioRpc,
    trace: Vec<Value>,
}
impl Rpc for Observed {
    async fn discover(&mut self) -> Result<Vec<Value>> {
        self.inner.discover().await
    }
    async fn call(&mut self, name: &str, arguments: Value) -> Result<Value> {
        let action = arguments.get("action").cloned().unwrap_or(Value::Null);
        let reply = self.inner.call(name, arguments).await;
        let snapshot = reply
            .as_ref()
            .ok()
            .cloned()
            .and_then(|v| tool_value(v).ok())
            .unwrap_or(Value::Null);
        let seat = &snapshot["users"][0];
        self.trace.push(json!({"tool":name,"action":action,"success":reply.is_ok(),
            "running":snapshot["running"],"paused":snapshot["paused"],
            "no_last_error":snapshot.get("last_error")==Some(&Value::Null),
            "pid":seat["pid"],"keyboard_pid":seat["keyboard_pid"],
            "x":seat["overlay"]["x"],"y":seat["overlay"]["y"],
            "buttons":seat["buttons"].as_array().map(Vec::len),"keys":seat["keys"].as_array().map(Vec::len),
            "known_failed_status":seat["status"].as_str().is_some_and(|s| s.contains("unavailable")||s.contains("Blocked"))}));
        reply
    }
}
fn path(name: &str) -> PathBuf {
    let value = PathBuf::from(std::env::var_os(name).expect("set the required test path locally"));
    assert!(value.is_absolute());
    value
}
async fn report(file: &Path) -> Option<Value> {
    std::fs::read(file)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

#[tokio::test]
#[ignore = "native routing: requires coordinated slot, approved PIM_TEST_SERVER/PIM_TEST_RUNTIME and PIM_NATIVE_SLOT_APPROVED=1"]
async fn coordinated_owned_probe_observes_scoped_virtual_input() {
    assert_eq!(
        std::env::var("PIM_NATIVE_SLOT_APPROVED").as_deref(),
        Ok("1")
    );
    let executable = path("PIM_TEST_SERVER");
    let runtime = path("PIM_TEST_RUNTIME");
    // This acceptance target is x64. Check its actual pinned runtime pieces
    // before creating any application; a DLL-only directory is incomplete.
    for name in [
        "ProtoInputLoader64.dll",
        "ProtoInputHooks64.dll",
        "ProtoInputIJP64.dll",
        "ProtoInputIJ64.exe",
    ] {
        assert!(
            runtime.join(name).is_file(),
            "incomplete approved x64 test runtime"
        );
    }
    let root = std::env::temp_dir().join(format!("pim-client-virtual-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let report_path = root.join("owned-probe.json");
    let preferences = root.join("disposable-preferences.json");
    assert!(!report_path.exists() && !preferences.exists());
    let probe = OwnedProbe(
        Command::new(&executable)
            .args(["probe", "--report"])
            .arg(&report_path)
            .current_dir(&root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let pid = probe.0.id();
    let config = Config {
        executable,
        runtime,
        preferences: preferences.clone(),
        gui_connection: None,
    };
    let rpc = StdioRpc::connect(&config).await.unwrap();
    let mut client = Client::discover(Observed {
        inner: rpc,
        trace: vec![],
    })
    .await
    .unwrap();
    let (before, rect) = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let windows = client.call("routing_windows", json!({})).await.unwrap();
            let owned = windows["windows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|window| window["pid"].as_u64() == Some(u64::from(pid)))
                .cloned();
            if let (Some(before), Some(owned)) = (report(&report_path).await, owned) {
                break (before, owned["client"].clone());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    let x = (rect["left"].as_i64().unwrap() + rect["right"].as_i64().unwrap()) / 2;
    let y = (rect["top"].as_i64().unwrap() + rect["bottom"].as_i64().unwrap()) / 2;
    let outcome = client
        .demo(&Scope {
            pid,
            user_id: None,
            x: i32::try_from(x).unwrap(),
            y: i32::try_from(y).unwrap(),
        })
        .await;
    let closed = client.rpc.inner.close().await;
    std::fs::write(root.join("redacted-trace.json"), serde_json::to_vec_pretty(&json!({
        "pid":pid,"expected_x":x,"expected_y":y,"outcome":format!("{outcome:?}"),"closed":format!("{closed:?}"),"trace":client.rpc.trace
    })).unwrap()).unwrap();
    assert_eq!(
        outcome,
        Ok(()),
        "client workflow failed; do not retry uncertain input"
    );
    assert_eq!(closed, Ok(()));
    let observed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(after) = report(&report_path).await
                && after["mouse_clicks"].as_u64() > before["mouse_clicks"].as_u64()
                && after["key_presses"].as_u64() > before["key_presses"].as_u64()
                && after["buttons"] == 0
                && after["test_keys"] == 0
            {
                break true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert_eq!(
        observed,
        Ok(true),
        "target observation or release was not established"
    );
    assert!(!preferences.exists());
    // Drop kills and reaps only the process created above. No descriptor, raw
    // MCP response, application title or pixels are written to evidence.
    drop(probe);
}
