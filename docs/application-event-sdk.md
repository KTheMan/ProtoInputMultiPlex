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

## Owned-window metadata candidate (source only)

The isolated metadata candidate extends the existing `WindowInfo` returned by
owned-window queries, snapshots, tracking, change events, and control
acknowledgements. `metadata` contains an optional title, owning process and UI
thread IDs, client bounds, DPI, monitor bounds, and monitor work area. Older
serialized observations and portable hosts that do not supply these facts
deserialize with unavailable fields. These numeric IDs are informational; they
do not authorize registration or accept a native handle from the wire.

On Windows, the adapter observes only an explicitly submitted same-process
top-level window on its creating thread. It installs a private per-window
cookie before reading metadata, checks the same ownership and exact cookie
after observation, and removes that cookie if registration fails. The
title is read through `GetWindowTextW` with capacity for 256 UTF-16 units plus
a terminator, then limited to 512 UTF-8 bytes. An empty title is a present
empty string. Client bounds are physical desktop coordinates converted with
`ClientToScreen`, and a
zero-sized client is valid. DPI comes from `GetDpiForWindow`. Monitor bounds
and work area come from `MonitorFromWindow` (nearest monitor) and
`GetMonitorInfoW`. Failed native observation returns an error; it does not
substitute guessed metadata. A native observation is followed by the
ownership/cookie check before being committed; `GetWindowTextW` can reenter a
custom window procedure, and the OS calls have no fixed wall-clock deadline.
The engine and SDK client reject overlong or NUL-containing titles, zero owner
IDs, inverted or out-of-range client bounds, DPI outside 1–4096, invalid
monitor bounds, and a work area outside its monitor. No HWND or HMONITOR is
serialized.

This metadata has source review only under the workstation host freeze. The
new remote owned-window route below is a separate source candidate; it has
not been formatted, compiled, tested, or run natively. Its Windows, Web, and
agent workflows need hosted acceptance before any parity claim.

## Shared native windows (source candidate)

A GUI-approved native publisher may register up to 16 of its own top-level
windows through the live event connection. Registration binds an owner-local
token to a GUI-issued `WindowId` only after the current lease, publication
approval, epoch, and token uniqueness pass. A window-only publisher needs no
virtual user or pointer/keyboard grant. The DLL checks the window's process,
creating UI thread, per-monitor DPI awareness, and private lifetime cookie
before and after local observation and control. The GUI never receives an
HWND, HMONITOR, cookie, or pointer and never invokes window APIs for a
consumer's window.

Each separate Web or agent receiver starts with no access to those windows.
The GUI can approve observation and control for an exact window and receiver;
tracking and control each need their separate receiver permission, and tracking
remains an explicit receiver action. A current-epoch remote window
reference is required for each query, track, untrack, or control operation.
Revocation, Stop, destruction, lease loss, and a new epoch fence old IDs.
Snapshots and events include only that receiver's approved, tracked windows.
The browser and agent workflows use those same GUI-issued references and
approvals; neither can publish a native handle or grant itself access.
Epochs and window IDs are exact 64-bit values. Web callers should pass the
GUI-copied reference JSON unchanged to `query_window`, `track_window`,
`untrack_window`, and `control_window`. Parsing it through a JavaScript Number
and serializing it again can round a valid reference into a different ID.

Remote observations and optional PID/thread metadata are **native-client
reported**. PID/thread values are sent only when both the GUI publication
setting and the publisher's per-report consent are enabled. A report carries
host receipt freshness and can become stale. Neither the event credential
nor the reported PID/thread values attest the publisher's OS identity to the
GUI. The native DLL's local HWND checks protect its own actions.

The owner's WndProc should call `pim_app_native_window_notify` after ordinary
position, visibility, title, style, enabled-state, and DPI messages; the SDK
reads the actual state on that UI thread and emits only changed observations.
`pim_app_native_window_refresh` requests an explicit observation even if it
is unchanged. Call `pim_app_native_window_retire` from `WM_NCDESTROY` before
the HWND can be reused. Neither a requested control nor a timer manufactures
a new observed state.

A control is sent asynchronously to the native owner. The network worker
posts only the existing wake hint. The owner's `pim_app_native_drain` validates
its local binding, executes the supported control, observes the result, and
queues a typed receipt. A successful Close receipt means the close message
was posted, while actual destruction requires retirement. If a receipt is
lost, times out, or is revoked after dispatch, the outward result is
**uncertain** and the last observation is stale. The action may already have
run; reconnect, resubscribe, and retry never replay it automatically. An
explicit fresh observation or registration is required before another action.

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
that needs authoritative host evidence. The bounded native-client DLL lifecycle
case passed on hosted x64/x86 Windows runners as described below; it does not
qualify every native window operation or the standalone Host C ABI lifetime.

## Builds and qualification

The source at `da98ada7ad454a254fc578e7d2f0016c14679106` passed hosted
formatting, strict workspace Clippy, workspace tests, x64/x86 builds, native-client
feature checks, software-renderer feature checks, WebAssembly checks and matching
browser binding generation in [run 42467047475](https://github.com/KTheMan/ProtoInputMultiPlex/actions/runs/42467047475).
The pull-request merge commit `1652cbf980b5b23dfe649024fa043f76c10e8e11`
has the identical Git tree. Ignored tests were not executed. The
[independent bounded assessment](hosted-sdk-prerequisites-assessment-2026-11-08.json)
binds the source, artifacts and limits. These results apply only to this tree;
later source changes require their own build and lifecycle checks.

That same run separately exercised the actual native-client DLL on hosted x64
and x86 Windows: explicit connection to a controlled peer, a data-free window
wake, snapshot folding, retained short-buffer reads, default-off local input,
Stop on owned-window destruction, actual worker-thread completion, successful
Destroy and DLL unload before the owner thread exited. Both external processes
exited normally with code 0. The peer was a fixture Host, not the GUI Service;
this does not establish native/Web use of the same GUI-created users or the
separate standalone Host C ABI lifetime.

The renderer-only fixture separately selected a CPU/DX12 adapter, advanced six
UI frames, read back its own four-color pattern, verified 25 pixels in each
region, closed once and exited normally with code 0. This validates that bounded
hosted renderer readback, not the full GUI, physical presentation, workstation
safety or a fix for the reported freeze. Independent acceptance and the exact
source/artifact evidence must remain attached to these bounded results.

For the later source at `6d7567446f0d754700972f1036df17f7d93b76a3`,
[run 42471108593](https://github.com/KTheMan/ProtoInputMultiPlex/actions/runs/42471108593)
passed the full compilation/test job and the bounded native-client, standalone
Host and renderer jobs. Its actual merge checkout `86128559200e014bad5cd724b8d56062bf5bf143`
has the same Git tree. The standalone Host fixture exercised all eight base ABI
exports, retained responses and explicit destruction through the actual DLL,
then unloaded it before each owner thread exited. Two sequential cycles passed
on each of x64 and x86; actual OS-thread completion/join and external exit 0
were required. It created no users or native windows. This qualifies that
positive lifetime case, not every Host/window operation or transitive TLS case.

**The overall run and actual GUI/native/WASM pair failed.** The pair was rejected
at GUI preparation checkpoint 37, before private connection issue checkpoint 38
and before either consumer launched. Source review found that the controller
read UTF-8 receipt text with Python's default encoding; checkpoint 37 is the
first expected label containing a middle dot. This is a portability bug, but
the historical encoding and rejected predicate were not recorded, so the cause
of that run remains unproven. A reviewed UTF-8 correction and sanitized failure
observations are required before another specimen. At this assessment, later
Web acceptance extensions were unpublished, uncompiled and unrun. The
[new bounded assessment](hosted-sdk-prerequisites-assessment-2026-11-08-v2.json)
records the accepted prerequisites, failed pair, exact artifacts and exclusions;
it preserves the earlier assessment above.

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
