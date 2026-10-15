# Independent GUI acceptance extension

**ACCEPTED for the demonstrated GUI extension**, October 14, 2026
(America/Los_Angeles). No material findings remain.

This extends the earlier standalone acceptance in `independent-review.md`.
That report remains historical; its then-pending live GUI gate is satisfied only
within the scope and evidence described here.

The reviewer inspected `tests/live_gui.rs`, `scripts/live-gui-gate.py`, the shared
reconnect helper, the application's unchanged Script acknowledgement format,
instructions, validation record, all three retained GUI attempt records, the
descriptor-remediation audit, owned-job cleanup diagnostic, and final process
audit. Native/GUI evidence was reviewed, not independently replayed. The reviewer
launched no native target, GUI coordinator, hardware input, or capture workflow.

The accepted successful fresh run used one owned x64 target PID 38248 and 17
actual app-local GUI controls. Its evidence supports:

- Default disabled access and separate capture, session-control, and
  app-preference grants all false. The Rust client received explicit denials for
  those three capabilities and the unpermitted second virtual user, with no
  image response and unchanged preferences/native counters.
- A native F9 hold through old client 1. Selecting the second user preserved the
  old connection and hold as a GUI draft. Actual Apply acknowledged the user
  scope expansion, cleared old clients, and released the native hold before the
  coordinator issued the approved handoff signal. Key events advanced from 0 to
  1 for the hold and 2 for release; the three extra grants remained false.
- The same `reconnect_after_approval` helper used by the CLI refused the old
  connection, closed it, started a fresh MCP process, rediscovered/reinspected
  policy, and obtained client 2. Native counters stayed unchanged across the
  handoff until fresh input was deliberately sent. Final key events were 4 and
  mouse events advanced from 2 to 4; input holds were empty.
- Scoped agent disconnect preserved GUI routing. Actual Disable removed the
  descriptor; Stop and owned shutdown followed. The passing run had zero job
  members before final close, all direct children reaped, no remaining
  descriptor, and no saved pixels. The separate final owned process audit
  reports zero controller/helper/test processes and native-slot release.

Material review findings were resolved. Every child now starts suspended and is
assigned to an owned kill-on-close Windows job before execution, preventing
Cargo/test/MCP descendants escaping cleanup. A run cannot pass with remaining
job members, an unreaped direct child, or a leftover descriptor. Inaccessible
process handles are treated as unknown, not proof of termination. The public
acknowledgement adapter now uses the actual guarded AgentPage snapshot format.
Instructions distinguish ordinary owner-supplied descriptors from the private
lookup restricted to the coordinator's exact newly created GUI PID.

Both earlier GUI prefixes remain FAILED. The second record's original leftover
descriptor field remains false. A separate audit verified its exact old owned
controller was dead and pipe absent, removed one stale descriptor privately,
and confirmed zero remaining owned descriptors across all three attempts.
This is runner remediation, not product crash-cleanup acceptance. The job
diagnostic also retains its earlier incorrect fixed-count assertion and reports
termination of all actual owned members on the fresh diagnostic run.

The reviewer independently completed these safe checks on the final artifact:

- `cargo fmt --all --check`: PASS.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: PASS.
- `cargo test --workspace --locked`: PASS, 26 contracts and 5 SDK transport tests;
  all three real-server gates remained ignored, so this rerun launched no apps.
- Python `ast.parse` of the coordinator without importing/executing it: PASS.
- SHA-256 comparison of the 66-file snapshot application baseline: zero
  mismatches.

The completed Cargo checks used the explicit separate client build directory.
Earlier reviewer invocations inherited the host's shared target directory and
were interrupted; they are not counted as completed validation. No local target
directory or main-checkout change was created.

Acceptance remains limited to the disposable cooperative x64 target, virtual
users, singleton explicit PID scope, and the demonstrated input-only user-policy
expansion. It does not establish successful capture, granted session/preference
mutation behavior, arbitrary applications, hardware/RustDesk behavior, startup
registration, provider-hang/crash recovery, desktop-wide isolation, or same-process
multi-user isolation. Timeouts and cleanup-failure branches retain deterministic
coverage. No raw descriptors, credentials, target titles, status text, or pixels
are required by this acceptance evidence.
