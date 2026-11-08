//! `call ENDPOINT [TOOL [ARGUMENTS_JSON]]`; no command retries.
use mux_sdk_client::Client;
use serde_json::{Value, json};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.len() > 3 {
        return Err("usage: call ENDPOINT [TOOL [ARGUMENTS_JSON]]".into());
    }
    let mut client = Client::connect(&args[0]).await?;
    let result = if let Some(name) = args.get(1) {
        let arguments: Value = args
            .get(2)
            .map(|text| serde_json::from_str(text))
            .transpose()?
            .unwrap_or_else(|| json!({}));
        client.call(name, arguments).await?
    } else {
        client.list_commands().await?
    };
    println!("{}", serde_json::to_string(&result)?);
    client.disconnect().await?;
    Ok(())
}
