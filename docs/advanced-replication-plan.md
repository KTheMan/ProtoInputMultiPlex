# Advanced replication behavior

This Rust feature group passed independent acceptance for bounded virtual input to disposable, explicitly owned cooperative x64/x86 applications. The [assessment](advanced-replication-assessment-2026-10-14.json) records accepted evidence and exclusions. Producer checks also passed: 79 workspace tests, formatting, strict Clippy and x64/x86 builds ([source record](advanced-replication-rust-checks-2026-10-14.json)). These results do not establish full MouseMux Ultra parity, real-device behavior or broad application compatibility.

## Per-edge options

Each stable-ID graph edge may carry an `options` object. Missing fields keep legacy behavior: anchored positioning, all mouse components enabled and immediate delivery. The Ouroboros graph editor and `routing_graph` use the same saved shape.

- `motion`, `wheel` and `horizontal_wheel` independently filter movement and the two wheel axes. `buttons` filters primary, secondary, middle, back and forward. Keyboard and text continue to use their existing graph category filters.
- `position: "anchored"` (default) keeps the destination's anchor. `"source_relative"` uses the processed source position plus signed virtual-pixel `offset_x`/`offset_y`; offsets are limited to ±32767 and must be zero when anchored. Mixed chains compose hop by hop. Relative placement was verified for motion; automatic button repositioning is not claimed.
- `click_delay_ms` is bounded to 0–60000 ms and preserves ordered down/up delivery. `step_interval_ms` is bounded to 0–1000 ms and controls time between events, not motion gain. The first discrete event is immediate; paced motion coalesces by half-distance toward its goal and ends with a final snap barrier.
- Candidate graph edits validate before replacement. Old edges load with defaults, and options survive graph/profile save and load.

For chains and diamonds, the planner picks a canonical path by stable seat-ID order, independent of saved edge order, and emits each component once. Combined/compound mouse packets remain unsupported by inherited packet validation; per-component filtering inside one compound packet is not covered.

## Queue and cancellation

Event and motion jobs share a queue capped at 4096 combined slots, with estimated pending payload capped at 8 MiB. Active cadence reservations have a separate 4096-slot bound. `routing_state.automation.replication` reports pending counts, estimated bytes, limits, suspension/error and emitted, coalesced and cancelled counters.

The graph page offers **Cancel pending replication**. MCP `routing_graph` supports Inspect, Apply and Cancel; callers may include `expected_revision` so stale operations are rejected. Cancel clears queued copies and replication-owned holds while preserving the graph definition/revision and ordinary or source-program owners. Later source events can replicate again.

When admission fails, the triggering source action is rejected before delivery to the source target. If it is a source release, release cleanup is still delivered. Overflow clears queued copies and suspends replication; ordinary source input continues without copies until a valid Apply resumes it. A source-program failure releases that program's own claims. Pause, Stop and stdio EOF retain their session-level release behavior. Replication Cancel does not stop recording or macro playback.

## Accepted evidence and limits

The [independent assessment](advanced-replication-assessment-2026-10-14.json) records 23 accepted native group occurrences (18 behavior occurrences across 17 distinct behavior IDs, with repeated setup counted separately) and 49 actual UI control events across two phases (34 plus 15, observing 26 distinct button labels). Counts are occurrences, not unique feature totals. The accepted GUI demonstrated acknowledged Apply, persisted option save/reload, queue/error visibility, recovery, and the visible suspension warning. See the [graph controls](advanced-replication-control-center.png) and [overflow warning](advanced-replication-overflow.png).

The native gate exercised the real 4096 event-count cap, rejection of the excess source-down before dispatch, ordinary source input while suspended, release cleanup, queue cancellation and cleanup ownership. The 8 MiB payload accounting and separate cadence-reservation bound have portable source/unit coverage; byte-only overflow and cadence-cap exhaustion were not exercised natively. The gate also did not separately verify new native delivery after Apply recovery, or button repositioning from source-relative motion. Timing observations cover selected values on this host, not general scheduling precision or throughput.

Earlier native and queue prefixes and failed GUI attempts retain their original statuses. The assessment accepts only passed subgroups and later completed tails; it does not relabel failed aggregate artifacts. Validation used only virtual seats and owned cooperative targets. Hardware, RustDesk, arbitrary applications, desktop-wide isolation, same-process independent isolation, node dragging/persisted layout, and compound packets remain unverified. Generated-clone live lifecycle is still missing; see [the next-group proposal](generated-clone-plan.md). Broader Ultra parity remains incomplete.

The [public Replicator manual](https://www.mousemux.com/pages/manual/) informed high-level behavior. Implementation and UI wording are original; no proprietary code or assets are reused.
