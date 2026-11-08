---
name: protoinputmultiplex
description: Operate ProtoInputMultiPlex MCP for scoped virtual Windows input, GUI setup, SDK status, shared native windows, and explicitly approved GUI host controls.
---

# ProtoInputMultiPlex

Use the connected server's advertised tools and the operator's existing grants.
Routing connections expose 25 tools in current builds. A stopped GUI Setup
connection exposes `profile_setup` and `application_event_status` in builds
containing the SDK-status addition. Check discovery before using a newer tool.

## Choose the existing connection

- Standalone `protoinputmultiplex.exe mcp --runtime <absolute-runtime-directory>`
  owns a separate virtual routing session.
- `protoinputmultiplex.exe mcp --gui-connection <descriptor.json>` uses the private
  descriptor supplied by the GUI operator. A live routing descriptor and a
  stopped Setup descriptor grant different authority; use the supplied mode.
- `protoinputmultiplex.exe mcp --event-connection <descriptor.json>` uses a
  separate GUI-issued event descriptor in supporting development builds.
  Discover `native_windows` and `hosted_session`; each requires its own current
  GUI grants. Setup and general routing commands remain unavailable.
- For stopped SDK inspection, the operator explicitly enables Setup access.
  Routing and target PIDs are unnecessary. Do not start routing, enable access,
  switch authority, or create an event session merely to obtain status.

Keep descriptors and credentials private. Connection failures and unavailable
tools are not permission to broaden scope or reconnect under another authority.

## Inspect application-event SDK status

Call `application_event_status` with no arguments or `{}`. Both supported modes
reject extra fields. On a stopped Setup connection,
`profile_setup {"operation":"application_event_status"}` provides the same
read-only operation. It accepts no profile document, mutation, consent, user,
input, capture, or credential fields.

The result is a safe summary, not an event stream. It contains no user/source/
receiver identities, names, input values, or credentials. Unavailable or stale
transport counters are omitted; absence does not mean zero or completed cleanup.
Standalone routing does not own the GUI's application-event session.

Event connection issuance and event/control consent remain local Ouroboros
workflows. **Select all visible users** changes only a receiver draft's exact
current IDs. Apply and observation/control consent remain separate; later users
are not implicitly shared. Application-event IDs are not `routing_input` user
indices. MCP status grants neither event production nor routing/capture access.

## Shared native windows (development candidate)

Use this mode only with the operator's supplied event descriptor and current
GUI approval for the exact shared window. Discover `native_windows` first;
source support does not establish runtime qualification. The native publisher
and receiver have separate GUI grants. Window operations cannot supply an HWND,
create users, change permissions, start routing, or obtain capture authority.

Call `native_windows` with `operation: inspect` for the current epoch and
connection state. Query the GUI-selected `window_id` with that exact `epoch`.
Keep both integers lossless; do not round-trip them through JavaScript Number.
Observation, tracking and control permissions are distinct. To act, explicitly
Track, then Refresh to install the subscription snapshot. Control also requires
its separate grant and a fresh owner report. Query/Track/Untrack/Control take
`epoch` and `window_id`; only Control takes the typed `control` action.
Inspect/Refresh take neither a window reference nor an action.

A control receipt describes the native owner's observation; Close admission is
not proof of destruction. Metadata is client-reported, not OS identity proof.
On an uncertain outcome, inspect and obtain a fresh owner observation before
any newly authorized action; never replay the uncertain action automatically.
`needs_refresh` requires explicit Refresh. A gap, old epoch, revoked grant or
`available: false` is not authority to switch to a routing/Setup connection or
issue new credentials. Ask the operator for fresh approval when needed.

## Hosted GUI controls (development candidate)

Discover `hosted_session` before use. Its separate GUI consents allow observing
host state, changing routing mode, and minimizing/restoring the GUI. Observation
alone does not authorize actions. Use mode control for the operator's requested
routing change; never start or stop routing merely to obtain status.

Obtain the current `epoch` from `native_windows` Inspect. If that live connection
needs a subscription refresh, use its explicit Refresh first. Call
`hosted_session` with `operation: inspect` and that `epoch`, omitting `revision`
and `mode`. Mutations take the same exact epoch and the observed
`hosted.revision`; keep both integers lossless. `operation: set_mode` also takes
`mode: native`, `switched`, or `concurrent`. `minimize` and `restore` omit `mode`.
Inspect again after an observed state change instead of guessing revisions.

Native means a clean routing Stop. Switched/Concurrent require the configured
automatic routing profile and actual worker readiness; assigned routing is not
a substitute. A hosted Start or Native Stop preserves only its requesting event
receiver and retires other receivers, including their issued descriptors. Other
clients need explicit GUI reapproval; do not reuse their old credentials.

A queued action is not a completion receipt. Lost or invalid receipts leave an
uncertain outcome: never replay the action automatically. Read connection status
through `native_windows` Inspect; Refresh applies only to a still-live
subscription. An ended connection needs fresh operator-supplied approval, not an
automatic reconnect. These controls neither load/save a profile nor change the
independent startup-loading or save-on-Quit preferences. Source support does
not establish runtime acceptance.

## Routing and other operations

Read the relevant section of [operations](references/operations.md) before using
standalone/live routing, profile or preference mutations, capture, Mapper,
replication, automation, drawing, magnification, or recovery. It preserves the
existing mode-specific workflows and historical acceptance boundaries.
Repository contract links in that reference resolve from a project checkout.

Inspect current ownership, user/PID scope and delivery state before input. Use
virtual devices and disposable applications for qualification. Do not move the
operator's hardware or RustDesk pointer. Acknowledgement is not proof of target
delivery; after uncertain input or cleanup, inspect rather than replay blindly.
GUI-owned routing starts/stops through its GUI lifecycle; follow the documented
client-disconnect semantics instead of treating them as global Stop.

Source support, build checks, rendered pixels and functional/runtime acceptance
are separate evidence. Full Windows/Web SDK, hardware/isolation and Ultra/UX
parity are not established by this skill or a successful status call.
