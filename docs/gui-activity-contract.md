# GUI callback activity

The October 29 assessment covers the compact diagnostics UI. The November 6 shared checkout acceptance covers stopped and live MCP observations across Start, Stop and disconnect with the default Rust worker. The historical GUI stall remains open. [Original assessment](gui-activity-assessment-2026-10-29.json) · [Integration assessment](rust-port-feature-integration-2026-11-06.json)

The existing script progress, phase and screenshot files can lag behind GUI execution. A Rust probe at the outer application's raw-input, logic and UI callback boundaries records scalar counters independently of filesystem and capture publication. Markers do not perform filesystem work, format messages, allocate payloads or acquire a shared GUI lock. A bounded atomic snapshot returns unknown if it cannot obtain a coherent observation.

## Agent access

Enable the appropriate private connection before observing. This adds no endpoint, input permission or tool:

- In stopped Setup access, call `profile_setup` with `{"operation":"activity"}`. The result contains `gui_activity` and no profile. The authenticated broker handles it directly without submitting work to the GUI queue. It accepts no profile document, revision or file operation.
- In a live GUI connection, inspect `routing_state.agent_session.gui_activity` or `agent_session.connection.gui_activity`. The compatibility projection also lives under the connection's `discovery.gui_activity`. Standalone sessions and legacy peers may omit the field or return null.

Stopped reads retain the existing token, admitted client, sequence and enabled-epoch checks. Start retires stopped authority. Live reads still depend on the routing worker and broker's existing authorization path. Revocation, terminal state or unavailable metadata must not reuse an old sample as current evidence. No observation grants routing, capture, restart or recovery authority.

## Interpretation

`version` identifies the supported shape. `process_id` and process-local `gui_instance` identify a probe within the existing authenticated connection lifetime; they are not proof against PID reuse or a new authorization credential. `coherent:false` with no `observation`, malformed/future-version telemetry, null and missing fields all mean unknown or unavailable.

An observation contains independent `raw_input`, `logic` and `ui` counters for entered, normally returned and unwound callbacks. `phase` identifies the latest callback boundary. With `in_callback:true`, its normal return has not been observed. `phase_age_ms` is age since that latest entry or exit marker; `last_return_age_ms` excludes unwind. `closed` means the probe's writer retired, not that native cleanup succeeded.

The receiver increases `sample_age_ms` and event ages while a cached sample remains resident. Pipe transport time is not measured. Compare current samples and their counters; a stored response is not a continuously refreshed monitor.

Logic can continue while UI callbacks are skipped for a hidden or minimized window. A returned UI callback means widget construction returned, not that rendering or presentation completed. Advancing callback counters alongside stale publication files can establish callback progress behind those artifacts. Stale counters, a callback awaiting return or an unavailable reply alone cannot establish a hang, renderer/OS failure or causal diagnosis.

The default-closed Ouroboros activity disclosure is confined to Agent Access diagnostics. It uses the existing pane's single scroll area and fixed action footer. It provides the same observations without a green health verdict or an action inferred from them.

## Screenshot readback comparison

For an owned GUI test, use the same virtual profile and app-local script with and without `--ui-script-no-screenshots`. The flag requires `--ui-script`. It disables screenshot requests and PNG publication while retaining script events, frame tracking and viewport snapshots. Screenshots remain enabled by default. Use a fresh directory for each launch so an older PNG cannot be mistaken for a current publication.

The script's progress JSON reports the latest requested and received generations under `capture_render`. A received generation means the GUI accepted a matching renderer event. The existing `capture.published` field records completed PNG publication. These fields are snapshots from the asynchronous script writer. When progress goes stale, compare it with fresh callback observations or a bounded responsiveness probe on the owned window.

One stopped x64 run in each mode completed the same virtual-user navigation and closed normally. The default run returned and published a screenshot, while the disabled run advanced without screenshots. The historical stall did not recur. [Bounded assessment](gui-capture-differential-assessment-2026-11-06.json)

A follow-up visited User B's Keyboard page and switched from Native controls to
Compatibility and back with capture enabled and disabled. Both runs advanced
and closed normally. The default PNG was requested after the last control
click. An earlier attempt failed on a progress-file read after the two clicks,
and retained close telemetry showed that the application kept advancing.
[Keyboard readback assessment](gui-keyboard-readback-assessment-2026-11-06.json)

## Script screenshot metadata

Optional `gui_activity.render_capture` correlates explicit Script screenshot
requests, received frames and PNG-worker publication generations. It adds no
tool or authority and contains no image bytes, paths or hashes. Request/receipt
markers and worker publication are sampled independently, not as a globally
atomic transaction. One nonblocking worker `try_lock` reports unavailable
worker details as unknown. Missing or malformed optional capture data does not
invalidate otherwise valid callback observations.

The default-closed Ouroboros disclosure exposes the same metadata. Ages are not
GPU-call timings, ordinary repaint cadence or a health verdict. Retired does
not mean completed. See the [capture-observation contract](capture-observation.md)
and its [bounded assessment](capture-observation-assessment-2026-11-07.json).
The historical GUI-stall P1 remains open.

## Acceptance boundary

Formatting, strict workspace/all-target Clippy, 687 workspace tests (zero failures, one ignored) and a fresh x64 native GUI build passed on the frozen 121-file source. Focused checks cover real callback wrappers and early returns, unwind accounting, bounded unknown samples, cache aging and backward-compatible optional fields. A real ScriptIo phase barrier held publication while an egui action and a separate metadata reader completed, then released normally. A real stopped broker/client answered Activity while its GUI receiver remained undrained and denied stale or revoked authority.

Acceptance composes the retained callback-metadata core with a separate stopped-GUI visual check. Six paired observations cover stopped/live activity in one owned GUI lifetime, the historical B keyboard-control pair, minimized logic progress and restored UI progress. Start retired stopped authority; terminal Inspect reported routing stopped and cleared activity metadata. The core's raw aggregate remains NOT_ACCEPTED because its harness expected descriptor deletion before client EOF; the retained terminal broker is intentional. All owned processes closed normally.

The final eight-step visual check verified the disclosure, full explanation and all counter rows through ordinary scrolling at 320×360, Disable cleanup, descriptor retirement and normal Quit. Original product frames are preserved at [normal size](gui-activity-normal-680x620-2026-10-29.png), [minimum explanation](gui-activity-explanation-320x360-2026-10-29.png) and [minimum counters](gui-activity-counters-320x360-2026-10-29.png). Earlier failed attempts remain unchanged, including the compact nested-scroll failure resolved by using the existing outer pane.

The core used two virtual users and a disposable target for explicit PID scope, with no target selection, attachment or routed input. The visual check used only a stopped GUI and virtual profiles. This does not replay the SDK/Mapper/Pause matrices or qualify broader application, device, display or Ultra parity. P1 remains open until separate causal and recovery evidence resolves it.
