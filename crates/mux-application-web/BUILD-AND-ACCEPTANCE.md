# Rust/WASM cooperative application client

This is an authored, unexecuted overlay. It must be assembled beside
`mux-application-sdk` and `mux-core` in a private workspace by the root launcher.
Add `crates/mux-application-web` to that workspace's members. The library disables
native SDK features. Existing compatible exact pins are wasm-bindgen 0.2.129 and
web-sys/js-sys 0.3.106. No lockfile or candidate tree was changed by this work.

The root launcher alone runs format, Clippy, Rust tests, WASM builds, binding
generation, local HTTP fixtures, browser and native acceptance. This author ran
none of those tools. Required checks after independent static review:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check -p mux-application-web --target wasm32-unknown-unknown
cargo clippy -p mux-application-web --target wasm32-unknown-unknown -- -D warnings
cargo build -p mux-application-web --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir crates/mux-application-web/example/pkg target/wasm32-unknown-unknown/debug/mux_application_web.wasm
```

Use an independently reviewed wasm-bindgen CLI matching 0.2.129; do not silently
install another version. Generated JS/WASM output is a build artifact. The only
handwritten JavaScript imports/initializes the generated binding and mounts the
Rust component. No network, input, canvas or application behavior is in JS.
Serve the example from a reviewed loopback HTTP origin explicitly approved by the
host. Do not use file://, wildcard origins or browser storage. The example CSP
permits loopback WebSockets only; change that authored fixture policy explicitly
if testing a separately approved WSS host.

## Protocol and authority

`EventHello`, `EventReady`, `ClientMessage`, `ServerMessage`, `ClientCommand` and
`Reply<ClientResponse>` are shared Rust types, not parallel browser wire models.
The server authenticates the exact HTTP Origin and a separate expiring event-only
credential, binds recipient identity out of band, and applies event and control
grants. Client validation supplements these server gates; it cannot grant itself
authority. The credential entry is emptied on Connect, authentication consumes
the transient Rust value once, and disconnect empties it again. No token appears
in URLs, state getters, callbacks, errors or storage. The client has no host
registration/approval/reset method and creates no users on connection or mount.

After Ready the client requests Subscribe exactly once. An atomic Subscribed
snapshot establishes the expected epoch and stream watermark; asynchronous
EventStream frames fold through shared ClientState. There is no browser Poll
loop, retry loop or queued local input. Explicit Refresh fences old stream frames
until a fresh Subscribed snapshot arrives. Gap invalidates rendering and input;
End/revoke/transport errors clear all state. Explicit reconnect creates a fresh
client state and sends a fresh Hello/Subscribe.

Input registration is canvas scoped. Pointer and keyboard remain off until a
user selects a visible user and separately enables approved controls. Keyboard
requires both the distinct key observation and key input grant, and a focused
canvas. Tab leaves the canvas. Five buttons, both signed wheel axes and mapped
USB HID keyboard usages are handled in Rust. Pen/touch are refused. Canvas
pointer capture balances releases outside its rectangle. Blur and normal
disconnect release only this client's held transitions while authorized; failed
release/backpressure retires the connection and requires server cleanup. No
observed hold originating in another consumer is released.

`WebClient.set_event_handler` registers an optional application handler for
validated ServerMessage JSON and local_disconnected notifications. Invocation
occurs outside internal state borrows, permitting SDK getter/control calls.
`connect(endpoint, event_credential)` provides explicit programmatic connection
with the credential passed directly into transient Rust memory; `select_user`,
`enable_pointer`, `enable_keyboard` and `refresh_subscription` expose the same
explicit gates to cooperative applications through generated WASM bindings.
`state_json` provides current authorized users, screens, windows, mode and
capabilities. `request` accepts only the typed ClientCommand control surface;
wire subscription/poll/disconnect operations use the explicit lifecycle UI.
Rendering changes only from validated snapshots/events, never acknowledgements.

## GUI-shared native windows

The GUI can approve a separate browser receiver for one current window published
by a native receiver. Publication, observation, tracking and control are separate
checkboxes; the browser cannot publish a window or choose a native handle. The
browser must use the `RemoteWindowRef` object returned by the current GUI epoch,
`{"epoch":<exact u64>,"window":<exact WindowId>}`, through `query_window`,
`track_window`, `untrack_window` and `control_window`. Keep large IDs as JSON text
through JavaScript; do not round-trip them through Number. The final method
accepts a typed `WindowControl` JSON value as its second argument. All four
methods send actual event SDK commands, and their validated responses reach
`set_event_handler` with request IDs. `state_json.windows` contains only tracked
windows currently delegated to this receiver.

An old epoch is rejected locally and by the host. Control also requires a live
tracked observation whose `observation.stale` is false; the owner revalidates the
native window and reports observed state after applying. A returned receipt is
an owner report, not GUI or OS peer attestation. Process and thread details, if
present, are explicitly consented client-reported metadata. A lost control
receipt is reported as uncertain, clears the browser's local state, and requires
explicit refresh after a new owner observation. It is never replayed.

Authoritative screen desktop bounds and revision are mapped to the CSS canvas
rectangle; DOM client coordinates use the inverse transform. The renderer scales
CSS drawing coordinates to actual rounded backing dimensions independently of
devicePixelRatio. ResizeObserver refreshes backing extent after layout/zoom;
every pointer event rereads geometry. This is an explicit full-desktop viewport,
not inferred browser-window-to-monitor DPI discovery. Negative desktop origins
and revision mismatch are covered by authored Rust tests.
DOM input sends shared `InputAtRevision`, allowing the server to reject a screen
revision change occurring after the browser computed a point. Cleanup releases
use ordinary Input so releasing a hold does not depend on an old transform.

## Acceptance gate

`acceptance-plan.json` is an authored action/expectation fixture for the root
launcher's browser/native test harness. It contains no credential. It is not an
execution result. Browser success alone cannot satisfy the named SDK group:
retain independent concurrent native+Web, Ouroboros GUI/MCP authority integration,
server authentication, lifecycle and zero-injection evidence. Physical input,
pen, touch and desktop-wide isolation remain unavailable.


## Event-hook extension acceptance (source only)

See EVENT-HOOKS.md for the exact original API and geometry limitations. The
following remain unexecuted, and require the root launcher's approved hosted
checks plus actual generated-WASM and simultaneous native/Web consumers:

1. Run the required workspace fmt, Clippy and tests, native/default/non-Windows
   checks and WASM check/Clippy/build/binding generation. Exercise the authored
   pure tests for exact u64 filters, same-event matching, bounded/nonreused IDs,
   registration removal/addition during a snapshot dispatch, per-user pairing,
   user/epoch/geometry reset and delayed nonauthoritative Snapshot replies.
2. Against the actual GUI Host authority, create exactly two approved virtual
   users. Connect actual native and generated-WASM consumers with distinct
   event credentials to that same Host. Prove their UserIDs/sources agree and
   the Web's default input gates remain off after registration, refresh and
   reconnect. Registration never creates users or changes grants.
3. Observe down/up from the actual authorized virtual producer on the browser
   canvas. A callback receives exactly one original release envelope with
   virtual source labeling; compare epoch/sequence/user/source/revision to the
   native consumer. Held-in-snapshot up, cross-user pairing, release outside the
   selected target, occlusion, target replacement and wrong-button pairs must
   produce no click callback. Verify no native DOM click, synthetic trusted
   event, focus transfer or OS input is caused by a binding.
4. Add/remove hooks in callbacks, including removing a later hook, self-removal,
   registering a new hook, setting/unsetting the legacy handler, throwing an
   exception and disconnect/refresh/dispose/free. Verify no RefCell or closure
   panic, no post-removal callback, addition only on a later notification,
   bounded reentrant disconnect handling, and release of retained references.
5. Exercise actual batch prefix+invalid suffix, old epoch, duplicate/reordered
   sequence, subscription fencing, repeated gaps, revocation/EOF and new fresh
   snapshots. No invalid/old stream reaches any callback and no press bridges
   these boundaries. A legitimate Snapshot query delayed behind pushed events
   validates without rolling back or closing the live stream.
6. Exercise negative desktop origin, fractional CSS extents and browser DPR
   zoom; change screens/revision, move/resize canvas/target, nested scrolling,
   disabled/inert states, occlusion and invalid geometry. Call explicit
   invalidate_virtual_clicks for application-owned transient layout changes.
   Unsupported CSS transforms/zoom, border/padding and missing computed data
   must fail closed. Clear pairing between each case; no inferred monitor/DPI,
   physical-device, transformed-canvas or historical-layout support is accepted.
7. Fill registrations and per-frame click staging to bounds; remove and
   re-register to prove tokens do not repeat; dispose twice and reject later
   registration/connection. Verify ordinary legacy callback traffic remains
   unchanged and credentials never enter callbacks or state.

These checks extend only the browser toolkit group. They cannot substitute for
full Windows SDK, GUI/native integration, actual device/target, security or full
functional parity acceptance, all of which remain open under their own gates.

# Hosted shared-window call gate

After a distinct GUI-approved Web receiver connects beside the disposable native
`--window-route` fixture, call `runWindowRouteAcceptance(client, referenceJson)`
from `example/window_route_acceptance.js`. The function makes real generated
binding calls for query, track, owner action and untrack and returns `PASS` only
after typed responses and validated native-client provenance. The native fixture
must independently observe the action and receipt. This source gate has not been
run in the isolated overlay.
