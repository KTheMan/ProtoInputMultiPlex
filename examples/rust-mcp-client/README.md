# ProtoInputMux Rust MCP client example

A standalone Rust crate with its own Cargo workspace and lockfile. It adds no
dependencies or behavior to the application workspace.

The accepted crate is integrated at `examples/rust-mcp-client`. See
[INTEGRATION.md](INTEGRATION.md) for repository check commands and provenance.
Historical acceptance records retain the original isolated snapshot paths.

Read [AGENT_WORKFLOW.md](AGENT_WORKFLOW.md) before running the client. Start with
`inspect` and `check`. `demo` requires a coordinated disposable target PID and
physical client-area coordinates. `reconnect` demonstrates an approved GUI
policy handoff with a fresh process and no input replay.

The example implements a small generic schema-validated MCP caller rather than
one wrapper per advertised tool. It uses state verification and never captures
images, configures preferences, or enables startup registration. No native
devices or global pointer APIs are used.

Run `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets --locked -- -D warnings`, and
`cargo test --workspace --locked` with a separate `CARGO_TARGET_DIR`.
The dedicated `rust-mcp-client.yml` CI workflow runs the client checks separately
from the product workspace and selects only library/contract tests for execution.
The ignored `nonrouting` test requires an approved built server; it does not
require native DLLs. Deterministic tests use scripted contracts and the official
SDK over in-memory duplex streams. They do not prove native target delivery.

The ignored `live_gui` gate and `scripts/live-gui-gate.py` validate real default
denials, policy-change revocation and approved reconnection against a fresh
disposable GUI/target. Coordinate the native slot first; prerequisites and the
finite controls are described in `AGENT_WORKFLOW.md`. The live GUI evidence is
separate from the earlier standalone acceptance and retains failed harness runs.

See `BASELINE.json` for isolation/source metadata and `VALIDATION.md` for the
actual checks, demonstrated scope, independent review and remaining limits.

Protocol references: [MCP tools](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2026-07-28/server/tools.mdx)
and [stdio framing/cancellation](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/draft/basic/transports/stdio.mdx).
