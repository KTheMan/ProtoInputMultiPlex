# Validation record

Validated on Windows x64 on October 14, 2026, using the isolated source snapshot
and separate build directory recorded in `BASELINE.json`. No application source,
MCP schemas, GUI or routing engine was changed. The source repository has an
unborn HEAD; the preserved status and 66 source-file hashes identify the copied
Rust/application baseline rather than inventing a commit ID.

| Check | Result |
| --- | --- |
| `cargo fmt --all --check` | PASS |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS |
| `cargo test --workspace --locked` | PASS: 26 client contracts + 5 SDK transport tests |
| Opt-in real-server `nonrouting` test | PASS with nonexistent runtime; no preferences file created |
| Coordinated one-probe `live_virtual` test | PASS: correct scoped PID, native click/key counters increased, no held input |
| Coordinated live GUI `live_gui` test | PASS: four explicit denials, policy Apply revocation/hold release, approved fresh reconnect without replay, GUI-preserving disconnect; 17 actual controls |
| Owned process cleanup | PASS: probe reaped, no owned controller/helper processes remaining; native slot released |
| Independent acceptance | Original standalone scope: `evidence/independent-review.md`; GUI extension: `evidence/independent-gui-review.md` |

The standard suite deliberately ignores the three real-server tests. They were
run separately under their documented prerequisites; passing the standard suite
alone does not establish those gates. The native gate exercised the same Rust
workflow called by the CLI. The real non-routing gate invoked the CLI binary's
`inspect` and `check` commands.

Two earlier native attempts failed because the private test runtime contained
the DLLs but omitted the injection helper executables. The client observed a
failed routing state and refused the keyboard step; release/stop succeeded and
each probe was reaped. Those attempts remain FAILED in
`evidence/acceptance.json`; the second includes a redacted action/state trace.
The fixture was corrected with the actual built helper executables, and the
test now checks its required x64 runtime files before opening a target. A fresh
third probe then observed one click and one key press and released test input.
No server behavior was changed to make the test pass.

The accepted live scope includes one cooperative owned x64 target under a
standalone session and a separate fresh owned GUI/target under a singleton
explicit PID allowlist. The GUI extension exercised capture, session-control,
app-preference and unpermitted-user denials with unchanged target counters and
preferences. It held a virtual F9 through the old connection, changed the user
selection draft without revocation, then used actual Apply to add the second
owned virtual user while all three extra grants stayed false. The acknowledged
policy cleared old clients and released native input before the approval signal.
The CLI's shared approved-reconnect helper refused the old transport, launched
a fresh MCP process, rediscovered/reinspected policy and obtained a distinct
client ID without replay. Fresh input succeeded and scoped disconnect preserved
GUI routing. Actual Disable removed the descriptor, followed by Stop and owned
process cleanup. Timeouts and cleanup-failure branches retain deterministic
coverage. Capture is not implemented; its denial is tested. Hardware, RustDesk
input, unrelated desktop pixels, startup
registration, arbitrary applications and isolation claims are outside this
validation.

Two GUI harness prefixes remain FAILED in `evidence/gui-attempts.json`: one read
the Agent page snapshot before its first render; the next expected the wrong
public acknowledgement shape and stopped before reconnect. The latter left one
private descriptor after forced cleanup. A separate audit verified its owned
controller was dead and pipe absent, removed only that stale descriptor, and
confirmed zero remaining owned descriptors. This runner remediation is not
product crash-cleanup acceptance. The successful fresh run required zero job
members before final close, so its cleanup did not depend on forced job cleanup.
The coordinator now assigns every suspended process to an owned Windows job
before execution, covering Cargo/test/MCP descendants on failure. Its separate
non-GUI cleanup check confirmed all remaining owned descendants terminated.

Evidence contains only whitelisted action names, numeric state/counters, hashes
and summaries. Raw tool snapshots, descriptors, credentials, application titles,
status messages and pixels are excluded. The deliverable contains this crate
only; the snapshot's copied generated runtime is excluded.
