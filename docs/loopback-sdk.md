# ProtoInputMultiPlex loopback command SDK

The isolated x64 build passed [bounded independent acceptance](loopback-sdk-assessment-2026-10-29.json)
with Rust and browser clients, concurrent client ownership, a 55-second silent
idle interval, policy revocation, SDK Disable, routing Stop and normal Quit.
The 58-action workflow uses virtual devices and one disposable owned target.
It does not approve the separate Rust-port work or establish full Ultra parity.

The compact Access/SDK layout also passed [independent native acceptance](agent-sdk-layout-assessment-2026-10-29.json):
30 recorded UI actions and 11 captures at 320×360 and 680×620, using the isolated build with
610 passing workspace tests (one ignored). This is a separate UI scope from
the earlier Rust/browser transport assessment.

Open **Settings → Agent access**. The **Access** pane contains stopped Setup
access or live user/process grants; **SDK** contains the command listener.
Enable stopped Setup access or acknowledge a live Agent Access policy, then
select SDK and explicitly enable
the listener. It binds `127.0.0.1` on an ephemeral port. Its private endpoint
file contains the version, port and a fresh bearer token. Treat that file and
the optional **Copy private SDK connection** output as credentials.

Each pane has a scrolling form body and a separate action footer. Narrow windows
stack actions vertically; connection paths and help sit under **Connection
details**. Switch to Access to edit and Apply policy, then return to SDK.
Switching panes preserves the listener and grants. Applying policy retires
affected SDK access and requires explicit re-enable with its fresh endpoint.
The accepted layout removes the earlier collapse-SDK/Apply/reopen workaround;
the previous development bundle retains that older layout.

The [normal Access view](agent-access-compact-2026-10-29.png) and
[minimum-size scrolled form](agent-access-minimum-2026-10-29.png) show the accepted
layout. Technical details scroll and are not all visible at once. The generic
footer can retain “Preparing SDK connections…” or “Closing…” after the phase
changes; the SDK pane's phase status is authoritative. This wording debt remains.

For browser clients, enter the exact trusted HTTP(S) origin before enabling.
The origin includes its nondefault port, with no path, trailing slash,
credentials, wildcard, query or fragment. Browser origins are denied by default.
A native Rust client sends no Origin header and still needs the bearer token.
An allowed origin and token do not grant routing permissions.

For a live GUI connection:

```rust,no_run
use mux_sdk_client::Client;
use serde_json::json;

async fn inspect(endpoint: &std::path::Path) -> mux_sdk_client::Result<()> {
    let mut client = Client::connect(endpoint).await?;
    let commands = client.list_commands().await?;
    println!("{commands}");
    let response = client.call("routing_state", json!({})).await?;
    println!("{response}");
    client.disconnect().await
}
```

The development example accepts an endpoint file and an optional tool name
and arguments JSON:

```powershell
cargo run -p mux-sdk-client --example call -- 'C:\private\sdk-endpoint.json'
cargo run -p mux-sdk-client --example call -- 'C:\private\sdk-endpoint.json' routing_state '{}'
```

For stopped Setup access, call `profile_setup` with
`{"operation":"inspect"}` instead of `routing_state`.

Discover tool names and schemas with `list_commands()`. Results retain the MCP
tool envelope, including `isError`, `content` and optional `structuredContent`.
Check that envelope before assuming a tool succeeded. A stopped Setup connection
exposes `profile_setup` only. A live connection exposes the existing 23 tools
under the GUI's current user, PID, session-change and capture permissions.
GUI Start remains a GUI action. Connected `routing_stop` retires that client;
it does not stop the GUI-owned session.

The wire starts with one JSON text hello:
`{"protoinputmultiplex_sdk":1,"type":"hello","token":"<private bearer>"}`.
After the SDK ready acknowledgement, use MCP initialization and one MCP JSON-RPC
message per text frame. The protocol allows one outstanding request per socket.
The command frame limit is 17 MiB; results have a separate 128 MiB limit because
MCP structured results include a JSON text copy. Existing document and pipe
validators retain their limits. A document near its 16 MiB decoder limit can
exceed an encoded transport envelope and receive an explicit error. Nothing is
silently truncated.

The Rust client never reconnects or replays a command automatically. A lost,
malformed or timed-out mutation response is uncertain. The same connection is
retired; reconnect and inspect current state before deciding what to do next.
A result-size error after execution is also uncertain. A tool rejection is
distinct from a confirmed native effect. Socket close requests client-scoped
release cleanup; it does not confirm all native releases finished.

SDK Disable retires its listener and sockets while ordinary MCP access retains
its existing lifecycle. Policy changes, broker retirement, routing Stop and
Quit retire affected SDK access. Re-enable explicitly and use the fresh endpoint;
old credentials do not restore revoked grants. Shutdown waits for accepted work
and cleanup on a background owner while the GUI remains available.

An authenticated connection may remain idle without periodic commands to retain
its client ownership. The GUI bridge starts its partial-message deadline when
the first byte arrives; authentication and response deadlines remain bounded.
EOF, Stop and access revocation still retire the affected connection. The native
acceptance check for this behavior deliberately leaves a client silent beyond
the former 50-second idle boundary; it does not send a keepalive to hide expiry.

Standalone SDK CLI hosting, raw physical-device event subscriptions and native
Win32 message SDK interfaces remain future work. This is an original command
transport, with no MouseMux wire compatibility or proprietary assets. Hardware,
secure desktops, arbitrary application compatibility and full Ultra parity
remain separate requirements.
