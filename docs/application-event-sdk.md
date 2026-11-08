# Application event SDK

This development branch adds a Rust application event SDK, a Rust/WASM browser
client, and Ouroboros controls under Agent Access → SDK → Application events.
It is separate from the existing command SDK. Full native/browser runtime
acceptance is still pending; this is not a release or an Ultra parity claim.

The cooperative SDK supplies virtual users, cursor state, pointer/buttons, both
wheel axes, separately approved key events, snapshots, bounded event streams,
explicit gap recovery, and producer-owned held-input cleanup. The native C ABI
uses the same Rust protocol and emits its header from Rust declarations. Native
window operations are restricted to registered windows in the owning process
and UI thread. The browser client uses the shared protocol, pushed events, a
canvas, and revision-checked coordinates.

## Local workflow

1. Create a cooperative event session, then explicitly create virtual users.
   Creating a session or listener does not create users or grant access.
2. Select the exact users to export. Approve pointer observation and keyboard
   observation separately, then choose each control permission. Browser control
   additionally requires explicit delegation. Apply the approvals.
3. Enter one exact HTTP(S) browser origin and start the event listener. Issue a
   private event-only connection with a lifetime of 1–300 seconds. Its credential
   is separate from command SDK credentials and never belongs in a URL or log.
4. Serve `crates/mux-application-web/example` with generated bindings from that
   approved origin. Select a visible user and enable pointer or keyboard input
   explicitly. Native and browser consumers can share the same host-created user
   IDs; connecting a browser does not create another user pool.
5. Revoke a receiver to retire that receiver's holds. Reapplying approvals creates
   a fresh receiver while retaining local user IDs. End the cooperative session
   to stop its listener. Global Stop and Quit wait for event and command SDK
   retirement before permitting restart or final shutdown.

The MCP `application_event_status` tool provides an authenticated read-only
summary through existing GUI admission. It does not issue credentials, grant
itself event access, or expose user identities or event payloads. Worker sample
age is preserved across GUI publication; unavailable or stale transport counters
are omitted.

## Builds and qualification

The Rust workflow runs format checks, strict workspace Clippy, workspace tests,
x64/x86 builds, and WebAssembly build/Clippy on standard GitHub-hosted Windows
runners. It generates browser bindings with the matching official
`wasm-bindgen 0.2.129` binary after checking its SHA-256 digest. Short-lived CI
artifacts contain unsigned SDK DLLs, generated headers, and the browser example.
They are development artifacts, not an accepted installer or a signed release.

Required acceptance still includes actual native window/C ABI behavior, a native
consumer and browser canvas concurrently using the same two users, real socket
authentication and lifecycle failures, GUI/MCP behavior, and independent review
of the resulting runtime evidence. Unit tests and green builds alone do not
establish that outcome.

Physical input export, pen/touch, cursor themes/nonzero flags, and unsupported
desktop-wide isolation remain outside the currently implemented cooperative
surface. Controls for unavailable capabilities must remain unavailable. No
hardware or RustDesk mouse is needed for cooperative validation. The separate
product release, reboot qualification, signing, and historical GUI stall remain
open until their own evidence and acceptance gates pass.
