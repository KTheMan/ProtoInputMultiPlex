# Native application controls

Opt-in Native controls separates application focus behavior from Rust's keyboard recipient policy. Compatibility remains the default. The [bounded assessment](native-controls-assessment-2026-10-19.json) accepts the sampled virtual workflow; the intermittent GUI stall remains OPEN/P1 and overall feature, UX and Ultra parity remain incomplete.

## Configure a new session

Use automatic desktop routing with a virtual mouse and paired virtual keyboard. While stopped, open **Configure user → Keyboard → Application focus → Native controls**. Unsupported or running-session choices are disabled. **Keyboard recipient → Focused control** follows the application's focused control; application focus mode alone does not choose a recipient. Save the profile and start the session. Assigned and physical-device routes are outside Native controls support.

The profile's `cursor.keyboard` has three separate fields:

```json
{"enabled":true,"focus":"focused","application_focus":"native_controls"}
```

`enabled` controls the raw virtual keyboard source. `focus` selects `clicked`, `focused`, `native`, `active`, or `caret` recipient lookup. `application_focus` selects `compatibility` or `native_controls`. Native controls omits ProtoInput's focus hook and synthetic focus/activation behavior so the target can use native control focus and mouse capture. Current development Native controls also requires the synchronized Rust keyboard provider, updated loader and isolated [keyboard-state adapter](keyboard-state-backend-patch.md). Keep the complete app/backend/architecture-specific observer runtime from one accepted build together; missing capability denies Native controls input. Compatibility retains its existing path. A target process cannot mix application-focus policies.

## Agent setup and live changes

Standalone `routing_start.initial_keyboard` optionally supplies one complete keyboard object per user, in `users` order. Wrong arity is rejected before creating a worker. Omission preserves existing defaults. Use a nonempty explicit authorized PID scope, for example:

```json
{"users":["A","B"],"allowed_pids":[1234,5678],"mode":"concurrent","multi_keyboard":true,"initial_keyboard":[{"enabled":true,"focus":"focused","application_focus":"native_controls"},{"enabled":false,"focus":"focused","application_focus":"native_controls"}]}
```

Replace example PIDs with explicitly authorized targets. GUI-connected clients use the GUI's configured session and existing grants; they cannot call standalone Start to change it. No tool or permission was added; current MCP count remains 23.

For live `routing_settings`, copy and replace the complete acknowledged settings object, preserving `application_focus`. A policy change is rejected before releasing held input or changing settings, including while paused or unattached. Stop and configure a new session to change it. Other valid fields retain their existing validation and release rules. Inspect state after errors; recipient snapshots are candidates, not delivery proof. Target mouse capture is separate from the MCP screenshot capture grant.

The accepted development standalone wrapper reports this prevalidation error as `Command rejected: ...`; actual saved-release failures keep `input release incomplete:`. The [feedback assessment](input-rejection-feedback-assessment-2026-10-21.json) accepts one x64 virtual Settings rejection with Control preserved until its original UP, normal Stop and a separate later read-only process-absence audit. The original native aggregate remains NOT_ACCEPTED. Mode has source/unit coverage and GUI status text has source/compile coverage; neither was natively exercised by this gate. Retained `last_error` is historical, including after successful UP.

## Observed scope and open gates

Current x64 ordinary single-line Windows EDIT fixtures in two owned processes passed independent persistent focus, separate second text actions, application Tab navigation, retained key release across a rejected policy change, simultaneous actual GUI-thread capture and independent release, GUI Stop, target reuse, standalone ordered keyboard setup/disable/re-enable, and normal standalone Stop. Final saved profile bytes were unchanged. Formatting, strict Clippy, 372 workspace tests (0 failed, 3 ignored), all 43 required regressions and default x64/i686 app/observer builds passed. A later bounded run exercised two owned i686 ordinary EDIT targets through one x64 standalone controller; it did not execute an i686 worker.


## Bounded i686 target compatibility - October 21

The [assessment](native-i686-targets-assessment-2026-10-21.json) and [independent review](../runtime/native-i686-targets-attempt002-review.json) accept one virtual-only x64 standalone MCP controller and worker routing to two owned i686 ordinary Windows EDIT fixtures. In 16.531 seconds, all 22 virtual input calls completed and both targets received 30 exact native text characters. The fixtures retained right-EDIT focus across a two-second idle interval and the second text action; real Tab delivered the right EDIT key-up and moved native focus to the matching left EDIT. Both left EDITs held independent native mouse captures simultaneously; releasing A left B's hold, focus and trace unchanged, then releasing B cleared its capture. Ordinary Stop exited the worker and MCP with zero holds, zero errors, normal probe exits, and no private image remaining.

This is target-architecture compatibility evidence for two owned ordinary controls, not execution of an i686 ProtoInputMux worker. The [raw evidence](../runtime/native-i686-targets-attempt002/evidence.json) SHA-256 is `fac8285573275095fc57cdc0ede2f871250733278b4293ac1eae6b0bb24f914f`; the independent review SHA-256 is `7558edc6b68135faaf831a2a322f454fdab584c10df08000fe5fb3afe6d4c803`; the public assessment SHA-256 is `3399decb161bac5a33c27aa0b955e05c91c6fb60a6c23c721ec7461d65a36f61`. The passing [producer verification](../runtime/producer-magnifier-controls-20261021-attempt003/verification.json) supplied formatting, strict Clippy, 437 tests (0 failed, 3 ignored), and x64/i686 app/observer builds; no new Rust checks were run for this native gate. Attempt001 remains NOT ACCEPTED with its original evidence and review. Arbitrary applications, hardware, RustDesk input, GUI configuration replay, passive selection API behavior, desktop-wide or same-process isolation, GPU capture, i686 worker execution, P1 reliability and full Ultra parity remain unverified.

Evidence is composed: separately accepted current-binary configuration originals ([A](native-controls-A-320.png), [B](native-controls-B-320.png)) plus one fresh GUI/standalone functional cohort. It is not an uninterrupted configuration-to-Stop run. The 320×640 originals prove the sampled choices, not a whole footer/Apply workflow, minimum-size polish, or complete UX parity.

Historical attempt004 stalled and required forced owned-process cleanup; the later configuration diagnostic completed but contained three transient WM_NULL timeouts. Their cause remains unresolved. Earlier failed aggregates retain their original verdicts. These results do not establish arbitrary-app compatibility, hardware/RustDesk behavior, desktop-wide or same-process isolation, real monitor/mixed-DPI coverage, GPU PrintWindow behavior, or shipping qualification. The frozen 329-test distribution predates this change.

The October 20 [keyboard compatibility assessment](native-keyboard-assessment-2026-10-20.json) additionally accepts the consumed-state Ctrl+S fixture and two owned classic WordPads: independent text/focus, idle retention, typing without re-click, exact Ctrl+S saves, normal Stop and ordinary closure. Required checks passed 394 tests (3 ignored), both app/observer architectures and both native backend builds. This development runtime is separate from the frozen distribution. The later [keyboard-table gate](keyboard-theme-assessment-2026-10-20.json) accepts one English-US Shift+A case with full GetKeyboardState tables, two non-removing peeks and state changes only on removal. Broader keyboard patterns, apps, hardware, P1 reliability and full UX/Ultra parity remain open.
