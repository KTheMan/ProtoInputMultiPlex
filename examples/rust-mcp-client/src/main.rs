use protoinputmultiplex_mcp_example::{
    Client, Failure, Result, Scope, reconnect_after_approval,
    transport::{Config, StdioRpc},
};
use serde_json::Value;
use std::{env, path::PathBuf};
use tokio::io::AsyncBufReadExt;

fn path(name: &str) -> Result<PathBuf> {
    env::var_os(name).map(PathBuf::from).ok_or(Failure::Scope)
}
fn config() -> Result<Config> {
    Ok(Config {
        executable: path("PIM_SERVER")?,
        runtime: path("PIM_RUNTIME")?,
        preferences: path("PIM_PREFERENCES")?,
        gui_connection: env::var_os("PIM_GUI_CONNECTION").map(PathBuf::from),
    })
}
fn summary(session: &Value, count: usize) {
    let gui = session["mode"] == "gui_connection";
    let policy = &session["connection"]["policy"];
    println!(
        "Discovered {count} tools; mode={}; connected={}; capture={}; session_control={}; app_preferences={}",
        if gui { "gui_connection" } else { "standalone" },
        session["connected"] == true,
        policy["capture"] == true,
        policy["session_control"] == true,
        policy["app_preferences"] == true
    );
}
fn scope(args: &[String]) -> Result<Scope> {
    let mut pid = None;
    let mut user_id = None;
    let mut x = None;
    let mut y = None;
    let (pairs, remainder) = args.as_chunks::<2>();
    for pair in pairs {
        match pair[0].as_str() {
            "--pid" if pid.is_none() => pid = pair[1].parse().ok(),
            "--user" if user_id.is_none() => user_id = Some(pair[1].clone()),
            "--x" if x.is_none() => x = pair[1].parse().ok(),
            "--y" if y.is_none() => y = pair[1].parse().ok(),
            _ => return Err(Failure::Scope),
        }
    }
    if !remainder.is_empty() {
        return Err(Failure::Scope);
    }
    Ok(Scope {
        pid: pid.ok_or(Failure::Scope)?,
        user_id,
        x: x.ok_or(Failure::Scope)?,
        y: y.ok_or(Failure::Scope)?,
    })
}
async fn run() -> Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    let command = args.first().map(String::as_str).ok_or(Failure::Scope)?;
    if !["inspect", "check", "demo", "reconnect"].contains(&command)
        || (command != "demo" && args.len() != 1)
    {
        return Err(Failure::Scope);
    }
    let target = if command == "demo" {
        Some(scope(&args[1..])?)
    } else {
        None
    };
    let config = config()?;
    if command == "check" && config.gui_connection.is_some() {
        return Err(Failure::Scope);
    }
    let rpc = StdioRpc::connect(&config).await?;
    let mut client = Client::discover(rpc).await?;
    let result = async {
        let session = client.inspect().await?;
        summary(&session, client.tool_count());
        match command {
            "inspect" => Ok(()),
            "check" => client.document_checks().await,
            "demo" => client.demo(target.as_ref().ok_or(Failure::Scope)?).await,
            "reconnect" => {
                if session["mode"] != "gui_connection" { return Err(Failure::Scope); }
                println!("After the GUI owner approves and applies the policy change, press Enter. No input will be replayed.");
                let mut line = String::new();
                if tokio::io::BufReader::new(tokio::io::stdin()).read_line(&mut line).await.map_err(|_| Failure::Transport)? == 0 { return Err(Failure::Scope); }
                client = reconnect_after_approval(&mut client, true, || StdioRpc::connect(&config)).await?;
                // The helper already inspected the fresh connection; no writes
                // or replay are performed as part of recovery.
                println!("Fresh GUI connection rediscovered and current enabled policy inspected.");
                Ok(())
            },
            _ => Err(Failure::Scope),
        }
    }.await;
    let closed = client.rpc.close().await;
    result.and(closed)
}
#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => {
            println!("Workflow passed within its stated scope.");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            // Enum only: no remote errors, titles, credentials, arguments or paths.
            eprintln!(
                "Workflow stopped: {error:?}. Do not replay uncertain input; review policy and cleanup before a fresh connection."
            );
            std::process::ExitCode::FAILURE
        }
    }
}
