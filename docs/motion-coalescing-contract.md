# Motion coalescing

[Bounded independent acceptance](motion-coalescing-assessment-2026-10-15.json) passed the virtual native combination and stopped GUI workflow. Full Ultra parity remains open.

Each user's cursor settings can optionally delay native mouse-motion publication:

```json
"motion_coalescing": { "buffer_events": 8, "interval_ms": 4 }
```

Both fields default to `null`, preserving immediate publication. The event limit accepts 2 through 64 and the time window accepts 1 through 16 milliseconds. Either threshold sends the pending update. Count-only configuration also has a 16 ms oldest-event deadline; timer-only configuration has a 64-event limit. Windows scheduling can delay delivery beyond a deadline.

Rust transforms each packet before publication is buffered. Desktop routes also apply logical bounds per packet and retain the latest position; fixed routes accumulate transformed relative deltas and split output to the native packet range. Fixed routing has no Rust logical cursor; merging relative publications can change intermediate native clipping and hover behavior, so identical endpoints at native clamp boundaries are not established. Logical cursor, annotation and automation processing remain per packet. Pending work belongs to its original user and verified process/window/client geometry. Target changes cannot retarget old buffered motion.

Discrete input, settings replacement, explicit positioning, mode changes, Pause and orderly Stop flush earlier motion before their effects. Deadline polling also runs when no further input arrives. A native publication error is reported; failed pending motion is not replayed against a later target. Automatic routing reports the affected virtual user and continues unrelated users. A failed barrier withholds an affected user's new discrete action, while key/button releases are still attempted. Assigned routing retains its existing fatal target-loss behavior.

The Motion page uses Ouroboros controls and the keyboard-accessible slider adapter. Existing MCP `routing_settings` accepts the same nested cursor settings object; it remains a whole-object replacement, so callers must retain unrelated settings. No new tool or permission grant is introduced. The high-DPI/foreground option is visibly unavailable because routed virtual packets do not carry the required provenance.

The public [MouseMux manual](https://www.mousemux.com/pages/manual/) describes event buffering and time-based coalescing. This implementation uses original Rust code and explicit product limits; the reference does not specify those numerical thresholds or resolve its foreground/high-DPI policy. No MouseMux assets or code are used.

The original native aggregate remains NOT ACCEPTED because its historical-coordinate oracle conflicts with the unchanged ProtoInput MouseMoveFilter. Its approved count-reduction prefix is combined with a separate missing-only continuation: each barrier clip settles before any future motion. A reduced A stream emitted 2 rather than 8 motion messages with the same endpoint while B remained at 4. A single timer-only packet reached its new endpoint with only readonly polling. These are functional samples, not a latency benchmark.

Fourteen stopped-GUI observations used 10 pointer clicks and 48 app-local key pairs. Both toggles and arrow adjustments produced B settings of 9 events/5 ms, matching the native MCP payload; a real fresh Save committed the full profile with A and unrelated values preserved. Two exact originals show the normal section and a minimum-size time slider with the whole disabled Apply action. The minimum time checkbox is above the viewport, and periodic PNG timestamps do not establish request freshness. No new live Apply, Load or UIA claim is made.

Acceptance is restricted to the recorded virtual-device and owned-application cases. Exact still-pending lifecycle interleavings, forced native failures/resize races, relative ABI splitting, coalesced annotation/graph combinations and x86 execution lack new native acceptance. Physical-device behavior, desktop-wide isolation, broad application compatibility and full Ultra parity remain open.
