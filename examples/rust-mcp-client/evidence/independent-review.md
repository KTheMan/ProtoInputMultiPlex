# Independent acceptance review

**ACCEPTED for the demonstrated scope**, October 14, 2026 (America/Los_Angeles).

The separate acceptance reviewer inspected the standalone Rust client, transport,
CLI, focused contracts, opt-in real-server tests, instructions, baseline metadata,
validation record, and redacted acceptance evidence. No material findings remain.

The accepted workflow discovers the running server's schemas, inspects session,
user and target state, performs bounded virtual input within one explicit target
PID, verifies acknowledged routing state, releases input, stops the standalone
session, and closes the transport. The retained third native-attempt trace shows
the requested PID 82540 for both mouse and keyboard, a released click and key,
zero remaining holds, release acknowledgements, and a stopped session. The
acceptance record reports the cooperative owned probe observed one click and
one key press and was reaped; the final owned controller/helper count is zero.
The reviewer inspected this evidence and did not independently launch native
applications or routing.

The first two native attempts remain explicitly FAILED. Their incomplete private
runtime omitted injection helpers; no target click or key was observed. The
retained second-attempt trace ends in Verification, followed by acknowledged
release/stop and transport closure. Fresh probes were used after fixture repair;
these failed prefixes have not been relabeled as successful native delivery.

The reviewer independently ran:

- `cargo fmt --all --check` in the standalone crate: PASS.
- `E:\Kenny\Documents\GitHub\ProtoInputMux-client-build-20261014\debug\deps\contracts-fb78a777ed0652df.exe`: 26/26 PASS.
- `E:\Kenny\Documents\GitHub\ProtoInputMux-client-build-20261014\debug\deps\protoinputmux_mcp_example-c3f0b3b0cde7130b.exe`: 5/5 SDK transport tests PASS.
- SHA-256 comparison of every entry in the snapshot source baseline manifest:
  66 files checked, zero mismatches, including the application sources/manifests.

The implementation owner's final checks additionally report Clippy with
`--workspace --all-targets --locked -- -D warnings`, the workspace test suite,
the DLL-free real non-routing CLI gate, and the coordinated native gate as PASS.
The baseline has an unborn Git HEAD; the hashes establish only the documented
copied scope, not an atomic whole-tree snapshot or a historical commit.

Review findings were resolved before acceptance: GUI demo policy now requires a
singleton allowed PID matching the explicit request; acknowledged movement and
clean state are checked before clicking; missing or known failed routing state
fails closed; and the actual CLI's shared approved-reconnect helper has focused
no-approval, revoked/denied/closed connection, fresh discovery/inspection, and
no-input-replay tests. Input, capture, session-control and app-preference grants
remain distinct. The client does not acquire grants, enable startup registration,
print raw remote state or descriptors, or implement capture.

Live GUI policy revocation/reconnection was **not performed**. That recovery and
GUI permission behavior have deterministic contract coverage only. Live
acceptance covers one cooperative owned x64 probe in standalone mode, not
arbitrary applications, hardware/RustDesk input, desktop capture, crash recovery,
desktop-wide isolation, or same-process multi-user isolation. Those limits are
accurately retained in the instructions and validation record.
