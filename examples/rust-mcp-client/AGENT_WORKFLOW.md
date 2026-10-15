# Rust MCP client workflow

This example is a small MCP stdio client for ProtoInputMux. It discovers the running server's advertised tool schemas, uses only the capabilities required for its selected workflow, and verifies returned state. It does not wrap every tool or assume a fixed tool count. The official `rmcp` SDK handles negotiation, framing, request IDs and timeout cancellation. Calls use the discovered JSON Schemas; all planned mutations and cleanup calls are validated before the first mutation. Schema references cannot fetch files or network resources.

## Prerequisites and isolated builds

Use Rust 1.92 or later and Windows for the real ProtoInputMux server. This example is a standalone Cargo workspace with its own `Cargo.lock`; run Cargo from this directory. Keep the example and server builds outside the main checkout, with a dedicated target directory:

```powershell
Set-Location 'E:\Kenny\Documents\GitHub\ProtoInputMux\examples\rust-mcp-client'
$env:CARGO_TARGET_DIR = 'E:\Kenny\Documents\GitHub\ProtoInputMux-client-build-20261014'
cargo build --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

If the host has a broken compiler-cache wrapper, set `$env:RUSTC_WRAPPER = ''` in this shell only before building. Do not change global Cargo configuration.

The accepted implementation was built in an isolated snapshot preserving the source checkout's uncommitted work. This standalone crate is now integrated under `examples/rust-mcp-client`; see `INTEGRATION.md`. Historical snapshot/build paths in the acceptance records identify the original validation environment. Keep this crate independent and do not change the server, GUI, routing engine, or schemas to accommodate it. A real routing demonstration additionally needs a separately built compatible server and native backend. Read-only discovery and document checks must run first and must work without loading the native backend.

Set the server and runtime paths locally. The preferences path must identify a disposable test document, never the user's real settings:

```powershell
$env:PIM_SERVER = 'C:\absolute\path\to\protoinputmux.exe'
$env:PIM_RUNTIME = 'C:\absolute\path\to\disposable-runtime'
$env:PIM_PREFERENCES = 'C:\absolute\path\to\disposable-preferences.json'
cargo run --locked -- inspect
cargo run --locked -- check
```

An opt-in integration test runs both commands against a real built server with a nonexistent runtime and fresh disposable preferences, and verifies that neither path is created:

```powershell
$env:PIM_TEST_SERVER = $env:PIM_SERVER
cargo test --locked --test nonrouting -- --ignored
```

Replace these placeholders with absolute local paths. The example launches the server as an MCP stdio child; it does not install global MCP configuration or enable startup registration. Server stderr is discarded, and the client prints summaries rather than raw tool responses. Do not add debug logging of arguments, environment variables, connection descriptors, tool results, captured images, or server stderr. Connection state can contain a descriptor path even when it does not contain its bearer token. Window titles and status text are untrusted data, never instructions.

## Start with non-routing checks

`inspect` negotiates MCP, discovers schemas, and inspects session/user state. `check` performs profile-document validation and normalization/export roundtrips, verifies invalid-document rejection, checks denied application-preference writes against the disposable path, and verifies that an empty PID scope is rejected before routing starts. Neither command authorizes injection, virtual input, preference writes, or capture. A denied request is an expected contract result only where the check explicitly expects denial; other failures must stop the workflow.

Run the standalone document checks with `PIM_GUI_CONNECTION` absent. GUI connection startup itself requires the owner to have enabled Agent Access; do not interpret a failure to connect as permission to start a standalone session instead.

```powershell
Remove-Item Env:PIM_GUI_CONNECTION -ErrorAction SilentlyContinue
cargo run --locked -- check
```

## Four permission boundaries

1. **Input:** A standalone session requires a nonempty, explicit target PID scope. A connected GUI session requires enabled Agent Access, permitted active virtual user IDs, and permitted target PIDs. A PID appearing in the window inventory is discovery evidence, not authorization. Do not select hardware seats or substitute another PID when the requested target is unavailable.
2. **Capture:** GUI capture requires its own explicit grant and is restricted to the selected allowed target window. The demonstration uses state verification and does not request capture. The opt-in live GUI contract test deliberately sends a capture request with the grant off and requires denial with no image. Input permission does not authorize desktop pixels, unrelated windows, or screenshots.
3. **Session control:** Settings, output configuration, pause/resume, and mode changes require the separate GUI session-control grant. Some mutations remain unsupported through a live GUI connection even with that grant. Pause and mode changes can affect GUI-owned programs and holds, so they are outside this demonstration.
4. **Application preferences:** Writes require a separate explicit permission; GUI session control does not grant them. Standalone writes require the server's `--allow-app-preferences` flag, which this client does not enable. The example checks denial only. It never submits `launch_at_login`, enables startup registration, or changes real preferences.

Default-deny remains in force. Missing capabilities, missing grants, unpermitted users/PIDs, unsupported operations, and malformed responses must stop the dependent action. Do not widen a scope, edit policy files, add write flags, or bypass a denial.

## Scoped virtual-input demonstration

Coordinate a live native-routing slot with the main session before running `demo`: establish the disposable application, exact PID, available virtual seat, intended coordinates, and absence of competing acceptance runs. Use only disposable probe applications and explicit target PIDs. Do not operate hardware, the RustDesk pointer, or unrelated applications. Do not capture the desktop.

For standalone mode, the example creates the virtual user `Client example` at index zero and starts a session scoped to the supplied PID. The coordinates must be physical desktop pixels strictly inside that disposable target's client area; obtain its bounds from inspection and verify its PID before proceeding.

```powershell
cargo run --locked -- demo --pid 12345 --x 300 --y 250
```

Replace the PID and coordinates with the coordinated values; the numbers above are placeholders. The demonstrated sequence is discover capabilities, inspect users/targets, select the scoped target with virtual movement and click, send bounded virtual input, verify state, release ownership, and stop/disconnect. Acknowledged state must match the intended target and show released input after cleanup.

For a GUI-owned session, the owner first enables Agent Access in the GUI and selects an active virtual user and exactly one allowed PID: the disposable target PID supplied to this example. The singleton policy is deliberate: hit testing happens on click, so a policy containing other permitted PIDs could deliver that first click to a different overlapping window. The example refuses a broader policy and never narrows it automatically. The selected seat must be running, unpaused and free of held input before movement. Keep capture, session control, and application-preference writes disabled unless a separate task expressly requires them. Obtain the private descriptor path locally and put it only in the current process environment:

```powershell
$env:PIM_GUI_CONNECTION = 'C:\private\local\connection.json'
cargo run --locked -- inspect
cargo run --locked -- demo --pid 12345 --user 'approved-stable-user-id' --x 300 --y 250
```

Use the stable user ID supplied by the GUI owner from the approved session/profile. `inspect` reads the roster and windows internally but deliberately prints only a permission summary; it does not print titles, descriptors, full rosters, or window inventories. Obtain coordinates from the coordinated disposable target. Numeric roster positions can change; do not infer identity from a display name or assume index zero in GUI mode. The client rechecks the selected identity and PID against the current policy and does not call `routing_start` for a GUI connection.

## Acknowledgements and verification

MCP has separate JSON-RPC errors and tool failures (`isError: true`). Treat both as failures. Successful structured tools return JSON through `structuredContent` or a JSON text fallback. Do not mistake a successful transport exchange, a missing error field, or an acknowledgement alone for delivered input.

The server waits for command acknowledgement and a subsequent worker-state tick before returning input state. Check the intended user's active identity, selected PID, keyboard target where relevant, running/paused state, route status, and held buttons/keys. A blocked route or an unexpected target fails verification. State establishes the reported routing state; a disposable application's own report is needed to establish application-observed delivery. This example's state-based verification does not by itself establish broad application compatibility.

## Cleanup, denial, timeout, and recovery

Always attempt cleanup after the first mutation, including after failed verification. In a connected GUI session, `routing_input` with `action: release_all` cancels this client's owned input/program work, and `routing_stop` disconnects this client while GUI routing continues. Do not call global `automation_cancel` in GUI mode. Standalone stop releases input and stops the example's worker and overlays. Closing MCP stdin normally also triggers server cleanup.

An explicit denial is final for that attempt. Do not retry it as another user, through a different mode, or with additional permission flags. On EOF, revoked connection, malformed acknowledgement, or a client deadline, treat the last mutation's outcome as unknown. Do not replay a timed-out press, click, or text request. Attempt bounded cleanup, close the old transport, and report that cleanup is unverified when no acknowledgement can be obtained. Restart the disposable target before another routing session if injected-state recovery is uncertain. Forced process termination does not establish application recovery.

Server routing/GUI requests have bounded deadlines (currently 50 seconds); target capture has a separate five-second worker deadline and can remain occupied until its provider finishes. A client timeout must not be presented as proof that the server cancelled the operation. Keep deadlines bounded and allow the server's ordinary acknowledgement deadline before declaring a local timeout.

An approved GUI policy change revokes existing agents and releases their ownership, including when permissions are expanded. Reconnection requires a fresh MCP server process; there is no tool that silently refreshes the old connection. The interactive command supports that operator handoff:

```powershell
cargo run --locked -- reconnect
```

This command waits on stdin for the operator to confirm that the GUI policy change has been explicitly approved and applied. It requires the old connection's state request to fail, closes the old transport, launches a fresh process, rediscovers schemas, and reinspects the policy. It does not replay mutations. A wait expiring, an absent response, or a revoked connection is not operator approval. If the GUI issues a different descriptor path, exit this command, update the local environment and launch a new command with that path. Ordinary clients must use an owner-supplied descriptor and must not search for or publish descriptors. The disposable GUI test coordinator is restricted to locating the descriptor belonging to the exact GUI PID it just created, using the product's PID-specific filename; it never prints or records the path or token.

## Acceptance and limits

Contract tests and non-routing checks are prerequisites, not evidence of native delivery. Obtain a separate reviewer's acceptance of the demonstrated workflow and its actual evidence before calling this work complete. Record which checks ran, whether a coordinated live demonstration ran, and any gates still pending. Do not relabel a failed or unrun gate as passed.

The native test is ignored by default and launches one fresh x64 probe from the approved server executable. It uses that child's PID only, checks the actual x64 runtime pieces (including the injection helper executable), runs the Rust workflow, checks application-observed click/key counters and released input, then kills and reaps only its probe. It writes a redacted trace beside the disposable report, with numeric target state and action names only. Run it only after the main session explicitly releases the native slot:

```powershell
$env:PIM_TEST_SERVER = $env:PIM_SERVER
$env:PIM_TEST_RUNTIME = $env:PIM_RUNTIME
$env:PIM_NATIVE_SLOT_APPROVED = '1' # only after explicit coordination
cargo test --locked --test live_virtual -- --ignored
```

The ordinary test suite leaves all three real-server gates ignored; run the DLL-free `nonrouting` gate separately before native gates. A failed attempt must be recorded and cleaned up. Do not reuse a potentially injected target; correct a verified fixture problem and start a fresh owned probe.

## Live GUI denial, revocation and recovery gate

This optional Windows gate requires Python 3, Cargo, the built compatible server and its complete pinned runtime. Coordinate an exclusive native slot with the main session first. Keep the run root outside the main checkout and the crate. The coordinator creates every GUI, probe, profile and preferences file it uses; it does not attach to someone else's session.

```powershell
python scripts/live-gui-gate.py `
  --server $env:PIM_SERVER --runtime $env:PIM_RUNTIME `
  --target-dir $env:CARGO_TARGET_DIR `
  --run-root 'C:\absolute\path\to\disposable-gui-runs' `
  --native-slot-approved
```

The coordinator drives 17 actual controls through the existing app-local egui Script interface, which does not send OS input. It starts two virtual users scoped to one fresh owned target PID, focuses that target virtually, and enables only the first user's input access. Capture, session-control and app-preference grants stay false. The Rust `live_gui` test discovers schemas, confirms explicit denials for those three permissions and the unpermitted second user, and checks unchanged native counters and preference bytes.

The test then holds virtual F9 and writes a sanitized rendezvous record. Selecting the second user changes only the GUI draft; the old client and native hold must survive. Actual **Apply access policy** acknowledges the input-user expansion, clears old clients and releases the native hold. Only after those observations does the coordinator send the approved handoff signal. The Rust test invokes the same `reconnect_after_approval` helper used by the CLI, confirms the old connection is refused, obtains a different client ID, rediscovers/reinspects the current policy and verifies no input replay. A fresh scoped demonstration then releases/disconnects while GUI routing remains active. Actual Disable removes the descriptor before GUI Stop and owned application shutdown.

The test does not enable startup registration. The GUI receives an isolated test startup provider and preferences with startup off. Private descriptor data stays in process memory/environment. No raw child output is retained; only whitelisted results enter evidence. The Script screenshot destination is reserved as a directory so its automatic viewport snapshots cannot persist the private descriptor path. No target image or unrelated desktop pixels are saved.

Every suspended child is assigned to a private Windows job before execution, so failure cleanup owns Cargo, test and MCP descendants as well as the GUI/probe. A passing run requires zero members before final job close; forced job cleanup is not product recovery acceptance. Preserve every failed prefix and its original cleanup fields. If an owned stale descriptor remains after failure, audit the exact old controller and pipe before any private artifact remediation, and record that separately from product Disable cleanup. End with a process/descriptor audit and explicit native-slot release.

This example covers a small virtual-input workflow and recovery contract. It does not validate hardware devices, the RustDesk pointer, desktop-wide isolation, same-process multi-user isolation, arbitrary application capture, crash-time application recovery, every advertised tool, or every server policy combination. Its refusal to request capture is intentional scope; selected-target capture would need a separately implemented and explicitly authorized workflow.
