# Typed Input Mapper — implementation contract

Status: **bounded foundation accepted for a qualified private cohort**; see the
[October 29 assessment](input-mapper-assessment-2026-10-29.json) for evidence and
remaining acceptance limits. This P2/P5 group
adds a general typed processing graph, separately from replication and input
forwarding. It does not complete the full Ultra target. Historical GUI-stall P1
remains open; stop and fix any concrete recipient/cleanup regression encountered.

The public [Input Mapper description](https://www.mousemux.com/ai-agents/),
checked October 29, 2026, supplies capability targets only. Implement original
Rust behavior and Ouroboros UI; copy no vendor code, assets or protocol.

## Signals, definition and evaluation

- Persist a separate version-1 mapper definition, with stable node/user IDs,
  named ports, directed edges and optional finite canvas positions. Legacy
  profiles default to an empty definition. Never persist handles, armed state,
  actor values, credentials or native release receipts.
- Three signal families have explicit semantic types: vector **position** and
  **delta**, scalar **state** and **impulse**, binary **hold** and **pulse**.
  Wires require exact semantic types. Conversions are explicit nodes; positions
  cannot silently become relative movement, nor wheel impulses latched holds.
- Inputs: a virtual user's pointer (position and delta ports), vertical or
  horizontal wheel impulse, named virtual key hold, or mouse-button hold.
  Outputs: selected virtual user's position, delta, wheel impulse, key or button
  hold/pulse. Text continues through its existing path; no text source is implied.
- Pointer input position is an actor-private, unscaled input trajectory. Seed it
  from the virtual user's current pointer on that actor's first relative input;
  integrate only that actor's raw deltas, and let absolute MoveTo reset it.
  Retiring the actor/run clears this baseline. It is not the shared rendered
  cursor position after other actors, speed settings or output clamps. A live
  rendered-cursor sampling node remains a separate capability.
- Include useful original processing: constants; position translation/clamping
  and position/delta conversion; delta scale/add/rotate/dead-zone; scalar
  arithmetic/clamping; vector components/composition; comparisons; binary logic;
  typed gates; explicit impulse-to-pulse, rising-edge pulse, toggle and scalar
  accumulation conversions. State/impulse variants retain their declared units.
- Reject unknown/duplicate IDs or ports, multiple producers for one input,
  incomplete required inputs, incompatible types, cycles, nonfinite/out-of-range
  parameters and duplicate source subscriptions before effects. Output nodes
  require an input-source ancestor; constants alone do not actuate on Arm.
- Resource bounds: at most 256 nodes, 1,024 edges, 1 MiB serialized definition,
  128 sink effects per admitted input wave and 64 actor contexts. These bound
  graph work, not product seat counts. Use deterministic stable-ID topological
  order. Recursion, unbounded queues, free-running clocks and feedback loops
  are unavailable.
  A pulse costs both DOWN and UP; mandatory binary releases also count. Validate
  the entire wave before consuming primary input or emitting a sink. Never
  truncate a wave. Retirement cleanup drains every retained receipt separately
  and is not discarded to satisfy a normal-input effect limit.
- Evaluate once per admitted original input or due Timer pulse. Impulses and
  pulses expire within that wave. State values may latch within their actor. Dirty propagation prevents
  unrelated inputs from replaying outputs. Opening a gate alone never replays
  old motion/wheel impulses. Closing a binary gate emits a release even if a
  different input caused closure. Wheel-to-binary uses explicit pulse semantics.
- All numeric inputs/results must be finite and bounded. Retain fractional
  motion/wheel remainder per actor/sink; quantize deterministically at delivery,
  check integer range, and clear remainder when the run retires. Never wrap.

## Counter and Switch

Counter takes a pulse on `count` and emits actor-local scalar state. The
initial count is an integer from 0 through 1 billion, and further pulses
saturate at that limit. Enable `with_reset` to add the required `reset`
pulse input. A reset returns to the initial count and wins when both pulses
arrive in the same wave. Actor retirement also restores the initial count.

Typed Switch uses matching `off` and `on` inputs and a hold on `select`.
A transient branch forwards only a current-wave value from the selected
input. Position and scalar-state branches require effect-free initial
values, such as a constant or an initialized converter. An unavailable
selected hold reads false, so an earlier held output can release.

Both nodes use the existing version-1 graph, the Ouroboros catalogue and
inspector, and the MCP Mapper catalogue and commands. The public MouseMux
catalogue supplies the feature reference. [MouseMux Input Mapper](https://www.mousemux.com/pages/apps/webapp-input-mapper-beta/) · [Counter and Switch assessment](mapper-flow-assessment-2026-11-06.json)

## Timer

Timer takes a hold on `enabled` and emits binary pulses on `value`. Configure
`interval_ms` from 50 through 60,000 and `count` from 1 through 1,000. A live
virtual input starts the run, and the first pulse waits one interval. A late
tick stretches the schedule. Lowering the hold cancels remaining pulses, and
a held enable must go low before it can start a fresh run after exhaustion.
Allow up to 16 Timer nodes per graph and 64 pending runs per session. Timer
nodes require a virtual source ancestor and cannot feed another Timer.

Due work uses the original actor and rechecks permissions, the saved target
lifetime and the actual recipient before delivery. A timed fault disarms the
Mapper and retires its claims. Existing release receipts remain retryable,
and unrelated native input keeps its ownership. The Ouroboros Activity view
and MCP Mapper inspection show pending runs and processed Timer pulses.
[Timer assessment](mapper-timer-assessment-2026-11-06.json)

## Monitor

Monitor has one typed `value` input and no outputs. It holds the latest
committed value and sample count per actor, with up to 16 Monitors in a graph.
It requires a virtual source ancestor, and it samples only during an armed
run. A failed wave cannot publish a sample. Existing passthrough or consume
behavior still belongs to the input source.

The Activity view displays the local virtual-input actor's latest readings.
Use `routing_graph` with `operation: "inspect_mapper_monitors"` for a
correlated, private read of the requesting actor. It returns `revision`,
`run` and `samples`. Monitor values are kept out of shared routing state,
profile data and script JSON. Disarm and actor retirement clear values.
[Monitor assessment](mapper-monitor-assessment-2026-11-06.json)

## Scope

A Scope takes one exact typed `value` input and produces a time plot in
Activity. It requires a virtual source ancestor and shares the 16-node
observer limit with Monitor. Each actor retains up to 64 committed points
per Scope in time order. `elapsed_us` is worker elapsed time since Arm.
The plot uses the latest committed values. Position and delta show X and Y,
holds use steps, and impulses and pulses use event marks.

`routing_graph` with `operation: "inspect_mapper_monitors"` returns
`traces` for the requesting actor. Each item has `node` and `samples`,
with `elapsed_us` and the typed `value` in each sample. The top-level
`samples` array is the latest-value Monitor response. Activity displays
only the local virtual actor's history. Disarm and actor retirement clear
the histories. [Scope assessment](mapper-scope-assessment-2026-11-06.json)

## LED, Gauge, XY Pad and Path Tracer

LED takes a Hold and displays its on or off state. Gauge takes scalar State
or Impulse and displays the latest value against a configurable range.
XY Pad takes Position or Delta and displays the latest X/Y value. Path
Tracer takes Position and draws up to 64 committed points in arrival order.
Both vector displays use configurable X/Y bounds and the desktop Y axis. These bounds affect the
display. MCP returns the original typed values.

Apply the graph and explicitly Arm. Activity shows the manual virtual actor.
The private `inspect_mapper_monitors` result includes LED, Gauge and XY Pad
under `samples` and Path Tracer under `traces`. The Mapper catalogue reports
the shared observer and trace limits. Disarm and actor retirement clear the
readings. [Visualizer assessment](mapper-visualizers-assessment-2026-11-06.json)

## Remap, Smoother and Velocity

Remap maps scalar State or Impulse from ordered input endpoints to output
endpoints. Enable Clamp to hold values at the endpoints. Descending output
endpoints reverse the range. The output retains the input's signal type.

Smoother applies an exponential moving average to Position or scalar State.
The first available input initializes the filter. Set alpha above zero and at
most one. An alpha of one follows the input immediately. Filter memory belongs
to the actor and current armed run.

Velocity takes Position and outputs scalar State in desktop pixels per second.
The first live position establishes the timing baseline and emits zero. Later
positions with a positive elapsed interval produce speed, capped at the engine
numeric limit. A position without a later timestamp leaves the baseline in
place. Scope can observe the input positions with the same worker timestamps.
[Signal conditioning assessment](mapper-signal-conditioning-assessment-2026-11-06.json)

## Scalar and vector math

Scalar function applies `abs`, `negate`, `sqrt`, `square`, `round`, `floor`,
`ceil`, `sin`, `cos` or `sign` to State or Impulse and preserves that type.
`sin` and `cos` use radians; `sign` returns zero for either signed zero.
Vector magnitude maps Position to State and Delta to Impulse. Normalize vector
preserves Position or Delta; an exact zero vector remains zero. Numeric inputs
and results must be finite and within ±1,000,000,000. A negative square-root
input or any out-of-range result, including a too-large magnitude, rejects the
whole actor wave atomically before delivery or observer updates.
[Math assessment](mapper-math-assessment-2026-11-06.json)

## Node palette

The Graph editor discovers implemented nodes in nine categories: Sources,
Constants, Vector, Scalar, Binary, Routing, Conversion, Observers and Outputs.
At wide sizes, the left palette and canvas appear side by side; compact layouts
use Browse nodes. Choose selects a prototype without changing the draft. Add inserts
the selected node as one undoable draft edit and opens Inspector. Add is
unavailable until a filtered result is selected. Apply and Arm remain separate.
Category descriptions cover implemented graph nodes; unsupported AI generation
and hardware control remain visibly unavailable. The palette uses original
branding and visuals and imports no MouseMux assets.
[Node palette assessment](mapper-palette-assessment-2026-11-06.json)

## Delay and Throttle

Delay queues typed Position, Delta, State, Impulse, Hold or Pulse values for
1–60,000 ms. It supports up to 16 Delay nodes, 128 pending values per actor and
256 per session. Values stay in FIFO deadline order, including Hold(false);
Pulse(false) is not an event and is not queued. Throttle uses the same interval
bounds and all six signal types. It passes the leading value and drops later
values within the window without a trailing replay. Hold(false) bypasses the
window immediately without changing its existing deadline, and Pulse(false)
does not consume quota. Zero scalar and vector values remain valid typed values.

Scheduling runs in the mapper worker and does not block the UI. A delay deadline
starts only after the original wave's native effects are accepted. Before a
due wave, the worker rechecks the original authority, recipient lifetime and
windows, and processes at most the oldest due wave per turn. Disarm, EOF,
revocation and Stop cancel queued work. A due delivery fault retires the run.
If an input wave is rejected while that actor has pending Delay work, or while
it owns claims in a Delay graph, the runtime retires the run, drops queued
values and performs required saved-claim cleanup. A domain rejection in a graph
without Delay work retains its prior behavior. Apply and Arm never trigger a
Delay or other autonomous actuation.

`pending_delays` and `delayed_events` are ordinary Mapper state counters.
Committed node samples remain private to the existing actor-scoped inspection.
Delay may compose acyclically with Timer in either direction and with other
Delay nodes; the existing Timer-to-Timer restriction remains in force.
[Timing assessment](mapper-timing-assessment-2026-11-06.json)

## Mapper screen observations

`PixelColor` and `ImageMatch` are asynchronous nodes in the existing Observers
catalogue category.
Both take `trigger: Pulse`, require real virtual-source ancestry and name an
explicit stable `capture_user`. Pixel Color reads one client-relative pixel
and outputs red, green, blue and alpha as State values plus `done: Pulse`.
Image Match outputs matched Hold, accepted-match score State, desktop-center
Position and done Pulse. No qualifying match outputs false, score zero and done
without inventing or clearing a position; Position remains the most recent
successful match. Gate actions with matched. Capture or analysis failures are
not matches and retire the affected run with its saved-claim cleanup.

Image Match accepts a PNG template up to 256 KiB encoded and 128 × 128 decoded;
the complete version-1 graph remains within 1 MiB. GUI graph-shape validation
does not read a window. MCP `validate_mapper` additionally validates PNG bytes
and still performs no capture or permission check. Apply validates and caches
templates, applies the existing mutation grants, and commits disarmed. Arm
never captures autonomously. Copying a snapshot crop into an Image Match node is
deliberate: encoded PNG becomes shared/persisted graph configuration. Snapshots
and live results remain private to the actor and never enter graph JSON,
profile state or shared routing state.

Capture reads use the existing selected client window, at most 16 Mi pixels per
frame and the exhaustive 64,000,000 candidate-pixel matching budget. Mapper's
raw graph path has no standalone Screen Tools 2 MiB PNG-return wire cap. GUI
admission uses its existing active virtual user plus explicitly selected
allowed PID and has no Agent Access checkbox. Connected MCP needs a separate
capture grant plus the requested capture-user and PID scope. Admission,
dispatch and completion bind/recheck source ancestry, capture user, target
window/watch, recipient scope, authority and target generation. Stale results
are dropped. Only the original actor's saved claims are released on its fault.

Limits are eight observation nodes per graph, one outstanding request per
actor/node, eight pending per actor and 32 per session. Repeated triggers and
session-queue overflow drop the new request, increment `dropped_observations`,
and do not reject otherwise-valid input or its effects. The oldest retained
request dispatches first; queued work has no age expiry and its five-second
deadline starts at actual dispatch. `pending_observations`, completed/dropped
counts and `capture_worker_busy` report counts only. Apply and Arm stay
unavailable until a canceled capture worker physically drains. The native
two-target/two-root cohort sent WM_CLOSE to target B's B0 root before a later
trigger/admission while B1 remained in the same process; that admission did
not fall back to B1. It did not exercise delayed completion. Pending/drop
bursts, delayed EOF, stale completion and tombstone cases have fake-provider
and source tests only. This does not establish hardware support or
arbitrary-application reliability.
[Mapper screen assessment](mapper-screen-assessment-2026-11-07.json)

## Starter flows

The Ouroboros Templates tab groups six editable flows by purpose.

| Route | Behavior |
| --- | --- |
| Mirror pointer | Consumes relative motion and reverses the horizontal axis. |
| Remap a key | Consumes F7 and holds F10 for the same interval. |
| Repeat a key on a timer | Consumes F7 and emits up to three F10 pulses at 500 ms intervals while held. |
| Smooth pointer | Consumes pointer motion and routes Position through a Smoother with alpha 0.5. |

| Monitor | Behavior |
| --- | --- |
| Speed monitor | Sends Position through Velocity into a 0–2,000 pixels per second Gauge. |
| Pointer trail | Shows Position in an XY Pad and Path Tracer. |

Monitoring preserves the source's original input and needs only a source user.
Routing binds a source and destination by stable virtual user ID. Keyboard
flows need virtual mouse and keyboard pairs at both ends.

Load replaces the draft and supports Undo. Apply and Arm remain separate.
MCP lists the same flows with `mapper_templates` and generates a portable
definition with `instantiate_mapper_template`. Provide `mapper_template`
and `mapper_source_user`. Provide `mapper_target_user` only when the
listed template reports `requires_target: true`. Live user, recipient and
permission checks still run before routing. Unexpected operation fields are rejected.
[Original starter-flow assessment](mapper-templates-assessment-2026-11-06.json)
· [Motion starter assessment](mapper-motion-templates-assessment-2026-11-06.json)

## Saved flows and JSON transfer

Use **Saved flows** to name and save a draft for reuse across profiles. Select
a flow and choose **Load into draft**. To share a draft, enter a JSON transfer
path and choose **Export current draft**. Use **Import to draft** to open a
portable file. Load and Import are undoable draft changes. Apply and Arm are
separate. Save and Export use the draft as it exists when clicked. Finish or
discard pending Inspector edits before using these controls.

The library stores version-1 documents with `version`, `name`
and `mapper` under `mapper-flows` in the chosen application data
root. Names match exactly and can be at most 64 UTF-8 bytes. Saving under the
same name replaces that entry atomically. Import validates both the document
and the typed graph before loading. Recheck stable user IDs and live recipient
permissions before Apply or Arm.

For an explicitly chosen data directory, start standalone MCP with
`--data-dir <directory> --allow-mapper-library`. Call `mapper_library`
with `operation` set to `list`, `load`, or `save`.
Load takes `flow_name` and returns the portable `flow` document.
Save takes `flow_name` and `mapper`. The agent can exchange
portable JSON in responses and requests. Local file paths remain a GUI choice.
[Flow library assessment](mapper-flow-library-assessment-2026-11-06.json)

The detached flow storage worker retains installation and data maintenance
leases until its work ends. Closing the editor releases the UI immediately.
A blocked-save regression checks lease retention after the GUI owner drops,
successful commit on resumption, and maintenance access after worker exit.
[Storage lifecycle assessment](mapper-flow-lifecycle-assessment-2026-11-06.json)

## Admission, arming and retained ownership

- Define input-copy versus consume-original explicitly on each source. Default
  is passthrough. Reject a passthrough path that also emits the same user's same
  input channel, preventing accidental duplicate motion/wheel/key effects.
  Other channels/users may be intentional copies. Derived outputs never re-enter
  mapper admission, replication or forwarding sources recursively.
- Copy/consume disposition is latched for every binary DOWN until its UP,
  including repeat DOWNs across Arm/Disarm/Apply. Reuse or extend the existing
  raw-source claim ledger (including output-free consumed claims). Never drop
  a pre-existing primary release or synthesize a primary DOWN on Disarm. Arming
  may additionally reject affected already-held sources with a clear error.
- A GUI/manual session is one evaluation actor; each connected client is a
  separate actor. Multiple authorized users can combine inside an actor; values
  and gates never leak between clients. Preserve original source Origin/user
  separately from the evaluation actor.
- Every sink has a graph-generation/sink namespace in logical ownership. Keep
  native output tokens refcounted across ordinary, mapper and other program
  owners. Retain the exact DOWN owner, translated output, recipient and source
  provenance for UP; never recompute release ownership from the closing event.
- Arm is off by default and never restored by profile load, Start or navigation.
  Apply validates the candidate and expected revision before effects, then
  disarms and releases only mapper claims before committing the new definition.
  Invalid/stale Apply has no effects. Failed cleanup remains visible/pending;
  Arm and replacement stay blocked until saved releases finish or process-exit
  recovery is proven. Disarm is always reachable and never releases unrelated
  ordinary/client holds. Existing EOF/revoke/Stop includes mapper claims under
  their original client and retains failed obligations.
- Retirement means Disarm, Apply, EOF/revoke, Stop or affected target/user loss.
  Clear mapped latches, toggles, accumulator/conversion baselines, dirty flags
  and fractional remainder. Preserve only raw per-DOWN copy/consume/repeat/UP
  obligations until UP or retirement of that input actor. Arm rejects affected
  already-held binary sources; it never treats a held repeat as a new pulse.
  A partial native failure stops that run before admitting further outputs and
  retains every successful DOWN receipt for cleanup, including a failed pulse UP.
- Before latching or delivery, validate every reachable source/output user and
  required current recipient. Connected mutation uses existing session-change,
  all-active-user and complete explicit PID graph scope, plus expected revision.
  Input retains its originating client's grants; graph authorship lends no
  permissions. Missing/retired users, keyboard disablement, Pause and target loss
  must neutralize affected producers and retain required releases.
- Reuse supervised Engine and lazy-attachment/selection paths. Do not call
  Windows SendInput, manipulate the OS/RustDesk pointer, or add native C++ patches.
  Reject unsupported routing/providers visibly. Existing replication/forwarding
  modes remain separate; simultaneous graph composition is unavailable until
  its ownership/feedback semantics are deliberately implemented and accepted.
  Resolve native key capabilities and packet limits during definition/whole-wave
  admission, before original delivery. The portable catalogue's key range is
  broader than the current native transport; unsupported keys/scans and relative
  motion/wheel ranges must fail before Apply/Arm or before the wave's effects.
  This group's original ingress is manual app-local virtual input and authorized
  agent input. Existing macro/playback outputs bypass admission; their recording
  and playback controls must be unavailable while the mapper is armed. Macro
  nodes and program composition remain an explicit later graph capability.

## Ouroboros, MCP and persistence

Add Settings → Automation → Input Mapper. Provide a node catalogue, structural
parameter/port editor, compatible-wire chooser, disconnect/delete/undo,
node/port validation and an original selectable graph view. JSON is optional
inspection/export, not the only authoring interface. At 320×360 use Graph,
Inspector and Activity views with one scrolling body and reserved Apply and
Arm/Disarm actions; a canvas must not force a wider minimum.

In the Graph canvas, drag a node to stage its position. Releasing the primary
button commits one undoable draft edit. Open a node in Inspector and choose
Duplicate node to copy its parameters under a fresh stable ID. The copy uses
automatic layout and starts disconnected. Duplicate is available for processing
and output nodes. In Graph or Inspector, use Ctrl+D to duplicate, Ctrl+Z to
undo, Ctrl+Y to redo, or Delete to remove a node and its wires. Text fields
and open menus take priority over graph shortcuts. Use Apply to submit the
draft. [Authoring assessment](mapper-authoring-assessment-2026-11-06.json)

Use the Graph canvas to connect ports by dragging from an output to an input,
or click the output and then the input. Hover a handle for the exact port name
and signal. Connections require the same semantic signal type, a free input
and an acyclic graph. Click a connected input and choose Disconnect wire to
remove its edge. Each connect or disconnect is one undoable draft edit.
[Canvas wiring assessment](mapper-canvas-wiring-assessment-2026-11-06.json)

Use Shift+click or drag a box around nodes to select several at once. Select
nodes mode also lets you toggle nodes with ordinary clicks. Drag a selected
node to move the selection together. Duplicate copies the selected processing
or output nodes and wires between them. Delete removes the selection and its
incident wires. Each group edit has one Undo step. Use Apply to submit the
draft. [Canvas group assessment](mapper-canvas-groups-assessment-2026-11-06.json)

Named groups organize a flow as described by the public
[MouseMux Flow Management reference](https://www.mousemux.com/pages/apps/webapp-input-mapper-beta/).
In canvas selection mode, select at least two ungrouped nodes and choose
**Group selected**. Select its heading or group chooser to edit its label,
collapse or expand it, or ungroup its members. Drag a group heading to move
all its members. Each action has an Undo step.

Collapsed groups show a card with external connections. Expand the group to
edit member ports. Deleting members prunes membership and removes empty
groups. Duplicate selected nodes creates ungrouped copies and preserves the
original group. These operations stage the draft for a separate Apply.

Version-1 definitions optionally include `groups`, each with `id`,
`label`, `nodes` and `collapsed`. Old definitions default to an
empty group list. Up to 64 groups are supported, each containing existing
unique node IDs. A node belongs to at most one group. Labels must contain visible text,
have no control characters and use at most 128 UTF-8 bytes.

MCP `routing_graph` operation `edit_mapper_group` takes `mapper`
and `mapper_group_edit`. Nested edit operations are `create` with
`group`, `rename` with `group_id` and `label`,
`set_collapsed` with `group_id` and `collapsed`, or
`remove` with `group_id`. The returned definition has validated
group metadata. Its overall `valid` and `validation_error` report
routing readiness separately, allowing unfinished drafts to be organized.
Groups preserve all processing nodes and edges, and are included in saved
profiles and portable flow documents.
[Named group assessment](mapper-node-groups-assessment-2026-11-06.json)

Use **Browse flows** from Templates or Saved flows to compare graph previews.
Choose Templates or Saved, enter a search and choose **Search**. Template
search includes descriptions. Saved search matches names without case
sensitivity. Saved results have up to twelve cards per page, with Previous
and Next navigation. At compact sizes, scroll the browser body to inspect a
card and its Choose action.

Cards show node, connection and group counts with an original structural
schematic. Collapsed groups appear as a single preview node. Damaged or
invalid saved flows appear as unavailable entries alongside healthy flows.
Choose returns to the corresponding flow page with that item selected.
Load remains a separate undoable draft action. Apply and Arm remain separate.

Standalone MCP `mapper_library` operation `browse` accepts
`query`, `offset` and `limit`. Defaults are an empty query,
offset zero and limit twelve. Queries are at most 128 UTF-8 bytes and have no
control characters. Offset is at most 4,096 and limit is from one to twelve.
The returned `page` includes query, offset, limit, total and entries.
Each entry includes name, a compact `preview`, and an optional error.
Use `load` to retrieve a full flow document. Preview data contains
structural node IDs, types, normalized positions and edge indices. Node
parameters, virtual-user bindings and live samples are excluded.
[Flow Browser assessment](mapper-flow-browser-assessment-2026-11-06.json)

Keep draft, acknowledged applied revision, pending command, armed state and
cleanup separate. Save through the existing asynchronous profile service.
Navigation never arms. Bounded activity exposes revisions, processed/rejected
counts and errors, without an unbounded input/text history. Vision/OCR,
image-match, LLM, hardware adapters and feedback loops remain unavailable.

Extend `routing_graph` with distinct mapper catalogue/validate/inspect/apply/
arm/disarm operations; preserve existing replication/forwarding commands.
GUI and MCP use the same worker state, revision and permissions. Update the
agent contract/skill with inspect → build/validate → apply disarmed → explicit
arm → act/verify → disarm/recover, without automatic retries of uncertain input.

## Independent acceptance

Required Rust formatting, strict workspace Clippy, workspace tests and native
build precede one focused owned-virtual gate. Independently verify transformed
motion and wheel, binary/pulse delivery, gate closure while held, copy/consume
transitions, cross-user same-actor signals and cross-client isolation. Prove
Disarm, valid/stale Apply, EOF/revocation and Stop preserve unrelated holds and
normal cleanup. Inspect normal/minimum authoring controls and saved definitions
loading disarmed. Unit evidence covers invalid graphs, deterministic bounded
evaluation, no impulse replay and failure transitions. Do not replay the
accepted SDK/keyboard/cursor matrices or call this full mapper/Ultra parity.
