# Independent client CI review

**ACCEPTED for the standalone client CI change**, October 14, 2026
(America/Los_Angeles). No material findings remain.

The reviewer inspected `.github/workflows/rust-mcp-client.yml`, the client README
and integration instructions, real-server ignore attributes, CI check evidence,
and the external 103-file before audit. The new Windows push/pull-request job
runs from `examples/rust-mcp-client` using that standalone workspace and lockfile.
It installs stable Rust with rustfmt/Clippy, uses a client-specific directory
under `runner.temp`, grants only read access to repository contents, disables
checkout credential persistence, and has a 20-minute job deadline.

The job runs exactly:

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked --lib --test contracts`

The explicit library/contract selection excludes `live_gui`, `live_virtual` and
`nonrouting` from execution. All three retain their unchanged ignore attributes.
Clippy checks all targets without running their test bodies. The workflow has no
native server/backend/submodule setup, coordinator invocation, descriptor or
runtime configuration, live approval flags, `--ignored`, or GUI/capture actions.
Live acceptance therefore remains separately opt-in and coordinated.

The reviewer independently ran all three exact commands from the integrated
crate with the explicit separate local client target directory and process-local
empty `RUSTC_WRAPPER`. Formatting and strict Clippy passed; all 5 SDK and 26
contract tests passed. No real-server test target was executed. Independent
PyYAML BaseLoader parsing and semantic assertions verified the event keys,
permissions, runner, timeout, working directory, separate target, checkout/toolchain
steps and exact command list.

Independent before-file hashing checked 103 preexisting files. The only changed
entries were the authorized client `README.md` and `INTEGRATION.md` CI text.
The existing `.github/workflows/rust.yml` is byte-identical to its retained
before copy. Rust sources, tests, coordinator, Cargo manifests/lockfiles, product
code, schemas, historical evidence and other preexisting files remain unchanged
at this review boundary. The nested workspace remains independent of root Cargo
commands.

This is local configuration and command acceptance. No hosted GitHub run was
dispatched, no commit/push/staging occurred, and no native/GUI applications or
ignored gates were launched by the reviewer. Hosted execution remains pending
normal committing and pushing of the workflow and crate. The reviewer wrote only
this report. Final CI metadata may replace its pending review field with this
acceptance and the client manifest may be regenerated; no functional change is
covered by that metadata update.
