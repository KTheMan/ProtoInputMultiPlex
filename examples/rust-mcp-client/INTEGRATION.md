# Repository integration

The independently accepted Rust MCP example, focused tests and instructions
are integrated under `examples/rust-mcp-client`. The main session assigned this
directory exclusively for the integration. Existing uncommitted work, including
`examples/two-seats.json`, is preserved. Product code, MCP schemas, root Cargo
manifests/lockfile and root documentation are outside the integration scope.

This remains a separate Cargo workspace with its own `Cargo.lock`. Root Cargo
commands do not include this workspace. The dedicated
`.github/workflows/rust-mcp-client.yml` workflow checks it on pushes and pull
requests; the existing product workflow stays separate. From this directory,
use the explicit separate local build directory:

```powershell
$env:CARGO_TARGET_DIR = 'E:\Kenny\Documents\GitHub\ProtoInputMux-client-build-20261014'
$env:RUSTC_WRAPPER = '' # process-local workaround for this host's broken cache
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

The default suite runs 31 deterministic tests and leaves all three real-server
gates ignored. This integration does not launch native targets, GUI sessions,
capture, or routing. Real-server gates require separate coordination as described
in `AGENT_WORKFLOW.md`; do not replay them merely to check the copy.

The client CI job runs formatting, strict Clippy for all targets, and
`cargo test --workspace --locked --lib --test contracts`. Selecting only the
library and contract targets excludes `live_gui`, `live_virtual`, and
`nonrouting` from test execution independently of their existing ignore
attributes. Clippy compiles all targets without running them. The CI job uses a
target directory under `runner.temp`, the client lockfile, and no native server,
runtime, descriptor, credentials or GUI setup. It does not fetch backend
submodules or invoke the coordinator. Local validation and independent review
of this CI configuration are recorded in `evidence/ci-review.md`; a hosted run
requires the workflow and crate to be committed and pushed.

The integration source is `ProtoInputMux-rust-mcp-client-gui-validated-20261014.zip`,
SHA-256 `736de1777c06428a9d26b156376034bab5ac677fd1f77f25e2463baee2c025f1`.
Its original file manifest is retained as
`evidence/accepted-artifact-MANIFEST.sha256`. Rust source, test source, coordinator,
Cargo files and historical acceptance evidence are copied without changes.
Only the README and workflow's location/provenance text are adapted for the
repository; this integration note and the independent integration report are new.
`MANIFEST.sha256` records the final integrated tree, excluding itself.

`BASELINE.json` and the earlier review reports describe the historical isolated
acceptance environment. They do not claim that the current product checkout
matches that earlier snapshot. Integration-specific checks and file comparisons
are recorded in `evidence/integration-review.md`.
