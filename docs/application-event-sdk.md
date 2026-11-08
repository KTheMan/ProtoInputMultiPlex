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
2. Apply the session owner's observation and control permissions. This master
   consent is separate from each receiver's permissions. Changing master consent
   retires every receiver before the change takes effect.
3. Add a receiver row for each native or browser consumer, up to eight. Select
   the exact users, approve pointer and keyboard observation separately, and
   choose its control permissions and exact HTTP(S) origin. Cross-owner input
   additionally requires explicit delegation. Apply each receiver independently.
4. Start the listener after approving the rows. Its allowed origins are fixed
   until the session ends. Issue a separate private event-only connection for
   each consumer, with a lifetime of 1–300 seconds. Credentials are single-use,
   separate from command SDK credentials, and never belong in URLs or logs.
5. Serve `crates/mux-application-web/example` with generated bindings from its
   approved origin. Select a visible user and enable pointer or keyboard input
   explicitly. A native push client uses its own approved connection and its own
   process's UI window. Both consume the same host-created user IDs; connecting
   either consumer does not create another user pool.
6. Revoke or replace a selected receiver independently. Unrelated receivers and
   local user IDs remain stable. Changing selection or revoking access cancels
   pending private-copy results. End the cooperative session to stop its
   listener. Global Stop and Quit wait for event and command SDK retirement
   before permitting restart or final shutdown.

The MCP `application_event_status` tool provides an authenticated read-only
summary, including the approved receiver count, through existing GUI admission.
It does not issue credentials, grant itself event access, or expose receiver
identities or event payloads. Worker sample age is preserved across GUI
publication; unavailable or stale transport counters are omitted.

## Native push client lifetime

The optional Windows `native-client` feature adds an external C/Rust event
consumer of the GUI listener. It creates no separate host or users. Connection
setup and local pointer/keyboard enables are explicit and default off. The
generated header describes every export and the retained-response contract.

After successful connect, obtain the caller-owned completion handle before
yielding to the UI loop. Include that handle in the owner thread's Windows
message wait. Registered window messages contain only a coalesced wake hint;
drain the SDK on the window's creating thread to read event data. A gap requires
explicit resubscription; advancing loss notifications keep the model cleared
and raise the minimum acceptable fresh snapshot. There is no automatic
reconnection or replay of uncertain commands.

Call Stop in `WM_NCDESTROY`, then keep the owner cleanup loop alive after the
window is gone. Destroy remains busy until the worker's actual Windows thread
has exited; success consumes its completed Rust join handle. Remove and close
the duplicated completion handle only after successful destruction. A failed
handle duplication leaves the SDK client live and requires explicit cleanup.

Before unloading the DLL, every owner must successfully destroy all clients,
every SDK call must have returned, and no later call may enter it. Never wait
or clean up under `DllMain`. Exiting an owner thread with outstanding clients
abandons state and forbids DLL unload for the remaining process lifetime.
Local completion or TCP EOF does not establish remote producer-hold cleanup;
that needs authoritative host evidence. Actual DLL load/stop/destroy/unload
qualification is still pending.

## Builds and qualification

The baseline at `0a82187085aced04eed723d7d03ccfc586a02073` passed independently
reviewed hosted formatting, strict workspace Clippy, workspace tests, x64/x86
builds, WebAssembly checks, and matching browser binding generation. Its 19
ignored tests were not executed. The receiver roster, native push client and
software renderer require their own current-source checks and runtime gates.

Generated source-formatting and dependency-lock artifacts are candidates for
review and commit. Checks on a runner's modified tree do not qualify the
committed source. SDK artifacts must pair each architecture's unsigned DLL
with the header from its matching feature build. Development artifacts are not
an accepted installer or a signed release.

The optional `software-renderer` feature provides an explicit
`gui --renderer software-dx12` request. Glow remains the default. The new path
requires a surface-compatible CPU/DX12 adapter and fails without one; it never
retries with a hardware adapter. Adapter enumeration and the OS compositor can
still involve graphics drivers. This is not a guarantee of workstation safety,
proof of rendered frames, or a fix for the reported host freeze. No local
build or native/graphics testing is being resumed on the affected workstation.

Required acceptance still includes actual native window/C ABI behavior, a
native consumer and browser canvas concurrently using the same two users,
real socket authentication and lifecycle failures, GUI/MCP behavior, and
independent review of the resulting runtime evidence. Unit tests and green
builds alone do not establish that outcome.

Physical input export, pen/touch, cursor themes/nonzero flags, additional
application modes, and remaining Windows/Web API families are still parity
gaps. Controls for unavailable capabilities must remain unavailable. No
hardware or RustDesk mouse is needed for cooperative validation. The separate
product release, reboot qualification, signing, historical GUI stall, and
whole-product UX review remain open until their own evidence and acceptance
gates pass.
