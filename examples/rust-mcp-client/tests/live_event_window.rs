//! Opt-in real GUI event-window route. The hosted coordinator supplies a live
//! native publisher, an exact-window agent receiver and its private descriptor.
use protoinputmultiplex_mcp_example::{Client, Failure, Result, transport::StdioRpc};
use serde_json::{Value, json};
use std::path::PathBuf;

fn path(name: &str) -> Result<PathBuf> {
    let value = PathBuf::from(std::env::var_os(name).ok_or(Failure::Scope)?);
    if !value.is_absolute() {
        return Err(Failure::Scope);
    }
    Ok(value)
}

fn number(name: &str) -> Result<u64> {
    std::env::var(name)
        .map_err(|_| Failure::Scope)?
        .parse::<u64>()
        .map_err(|_| Failure::Scope)
}

fn require(condition: bool) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Failure::Verification)
    }
}

#[tokio::test]
#[ignore = "requires a hosted GUI-approved window receiver and live native owner"]
async fn real_native_window_agent_query_track_control_untrack() -> Result<()> {
    require(std::env::var("PIM_EVENT_WINDOW_APPROVED").as_deref() == Ok("1"))?;
    let executable = path("PIM_TEST_SERVER")?;
    let descriptor = path("PIM_EVENT_CONNECTION")?;
    let epoch = number("PIM_EVENT_WINDOW_EPOCH")?;
    let window_id = number("PIM_EVENT_WINDOW_ID")?;
    let rpc = StdioRpc::connect_event(executable, descriptor).await?;
    let mut client = Client::discover(rpc).await?;
    require(client.tool_count() == 1)?;
    let inspect = client
        .call("native_windows", json!({"operation":"inspect"}))
        .await?;
    require(inspect["epoch"] == epoch)?;
    let reference = json!({"epoch":epoch,"window_id":window_id});
    let query = client
        .call(
            "native_windows",
            json!({"operation":"query","epoch":epoch,"window_id":window_id}),
        )
        .await?;
    require(query["window"]["id"] == window_id)?;
    require(query["window"]["observation"]["source"] == "native_client_reported")?;
    client
        .call(
            "native_windows",
            json!({"operation":"track","epoch":epoch,"window_id":window_id}),
        )
        .await?;
    let tracked = client
        .call("native_windows", json!({"operation":"refresh"}))
        .await?;
    require(
        tracked["windows"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["id"] == window_id)),
    )?;
    let mut action = reference.as_object().cloned().ok_or(Failure::Protocol)?;
    action.insert("operation".into(), Value::String("control".into()));
    action.insert("control".into(), json!({"kind":"restore"}));
    let controlled = client.call("native_windows", Value::Object(action)).await?;
    require(controlled["acknowledgement"]["window"] == window_id)?;
    require(controlled["acknowledgement"]["observed"]["observation"]["stale"] == false)?;
    client
        .call(
            "native_windows",
            json!({"operation":"untrack","epoch":epoch,"window_id":window_id}),
        )
        .await?;
    let after = client
        .call("native_windows", json!({"operation":"refresh"}))
        .await?;
    require(
        after["windows"]
            .as_array()
            .is_some_and(|rows| rows.iter().all(|row| row["id"] != window_id)),
    )?;
    client.rpc.close().await?;
    Ok(())
}
