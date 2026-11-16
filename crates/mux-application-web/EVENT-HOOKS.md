# Consumer event hooks and virtual selector bindings

This original Rust/WASM surface is source only, pending independent review,
hosted Rust/WASM checks and actual consumer acceptance. It does not complete the
Windows/Web SDK family or expand the current cooperative virtual producer.

`set_event_handler(optionalFunction)` retains its single-handler behavior: it
receives the validated typed ServerMessage JSON text and local_disconnected
notifications. Passing undefined removes it. It runs before registered hooks.
No connection, input enable, user selection, host grant or retry is implicit.

`add_event_listener(filterJson, callback)` independently registers a consumer
callback and returns an opaque string. The filter is JSON text, for example
`{"kind":"input","user":{"epoch":18446744073709551615,"local":9007199254740993}}`.
Keep identities as exact JSON text throughout the application. Constructing them
as JavaScript Numbers or parsing/stringifying through Number loses precision.
The callback receives the original frame JSON string once when a frame matches;
a batch can include other already-authorized events. Filters select notifications,
not grants or redaction boundaries. `user` must match the same event as `kind`.
Kinds are `any` (default), `server`, `local`, `input`, `users`, `screens`, `windows`.
Input/user filters select actual stream events, not state snapshots or replies.
`remove_listener(opaqueString)` removes either registration type, returns whether
it existed, and is safe to repeat. Strings are never numeric handles.

`bind_virtual_click(optionsJson, callback)` observes virtual button transitions
for the first current selector match inside the mounted client root. Options are
`{"selector":"[data-canvas]","button":"primary"}` plus an optional exact `user`
object. The five shared button names are accepted; primary is the default.
This helper invokes only the explicitly supplied callback. It never calls a DOM
control's click(), dispatchEvent(), focus(), or global OS input. The result is
not a trusted hardware/DOM event and does not carry a browser user activation.
The callback receives JSON text with `kind: virtual_click`,
`source: authorized_virtual_stream`, the selector and the original authoritative
release envelope (including exact epoch, sequence, user, source and revision).
The current Host producer is virtual-only. SourceId alone is not physical device
metadata; a future physical producer needs an explicit source-kind contract
before this virtual helper can consume it under a different promise.

A binding starts empty, even when a snapshot already reports a held button.
An authorized incoming down must hit the target, followed by an incoming up for
the same user and configured button on the same Element, target rectangle,
canvas CSS rectangle, DPR and authoritative screen revision. All stream frames
first pass the actual ClientState atomic epoch/sequence/transition validation.
A bad batch never stages callbacks from its valid prefix. Refresh fences old
queued streams. Gap, snapshot replacement, disconnect, authority retirement and
dispose discard pairing; a user view replacement/disposal retires that user's
presses. Screen changes discard pending activations. No local DOM press is
counted until it returns through the authorized stream.

Screen pixels map through the existing full-desktop CanvasTransform to CSS
viewport coordinates. Both canvas and target bounds are half-open; a topmost
DOM hit must be the selected element or its descendant. Hidden, missing,
disabled/aria-disabled/inert or occluded targets do not match. Pairing and final
invocation reread geometry and selector identity. DPR is independent of backing
pixel rounding. The canvas must have zero border/padding and its ancestor chain
must have no CSS transform, perspective, individual transform, motion path or
CSS zoom. Browser zoom represented by DPR is supported. Unsupported computed
geometry fails registration or clears pairs; it is never treated as a hit.
Only the existing web-sys CssStyleDeclaration feature was added to obtain these
checks; dependency versions and lockfile are unchanged.

ResizeObserver and window resize/scroll invalidate pairs. Rectangles/DPR are
also checked at button transitions and immediately before callback invocation.
The browser does not provide a trusted historical geometry revision for arbitrary
application mutations. Call `invalidate_virtual_clicks()` whenever application
layout changes could move/remove/replace a target and revert between frames,
including nested scroller changes that do not reach window scroll. This clears
pending presses and fences pending click callbacks. Arbitrary transformed-canvas,
shadow-root, iframe, offline-user and historical-layout reconstruction support
remain unavailable; no hit is inferred for those cases.

Callbacks run after RefCell borrows are released. Dispatch snapshots registered
IDs before invoking the legacy handler. Removal/disposal takes effect before
each subsequent call; additions start on the next notification. A handler can
read state, remove itself/another registration, refresh, disconnect or dispose.
Refresh/disconnect/dispose fence remaining server and click callbacks from the
current dispatch. A callback exception is caught and reported locally; remaining
eligible callbacks continue. Reentrant notifications are drained without recursive
borrow/invocation, with a 64-notification budget. There are at most 64 combined
registrations (the demo uses two), 64 presses per binding and 256 staged click
callbacks per frame; overflow discards click staging/pairs. Counters never wrap.
Explicit dispose/free releases callback references, DOM listeners and observers;
disposed clients cannot register or reconnect. Reconnect requires a new explicit
credential and never restores presses.

The Rust demo invokes add_event_listener and bind_virtual_click itself for two
informational status lines. They register observers only; the demo still mounts
disconnected and its independent pointer/keyboard production gates remain off.
Applications should retain opaque tokens and remove listeners when a component
unmounts, then dispose the client at owner teardown.
