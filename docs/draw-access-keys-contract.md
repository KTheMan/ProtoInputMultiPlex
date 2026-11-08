# Draw access keys

Status: bounded native GUI acceptance passed October 21, 2026. The [assessment](draw-access-keys-assessment-2026-10-21.json) and [independent review](../runtime/draw-access-keys-ui-attempt002-review.json) cover one app-local workflow with two owned virtual probes and one owned GUI. This establishes neither desktop-wide behavior nor real-device isolation.

These are local Draw-page controls using original Rust and Ouroboros UI. They add no MCP tool, permission, global shortcut or hardware-control path; the existing 23-tool MCP surface is unchanged. They act only while Draw is focused and use exact Alt combinations. Repeated key-downs, modified Alt combinations and keys while an unrelated egui popup is open are ignored. Action keys are suppressed while a text edit has focus; Alt+B, Alt+O and Alt+S remain available to focus/open their controls.

| Key | Behavior |
| --- | --- |
| Alt+D | Stage Brush when the selected user's staged tool is Off; otherwise stage Off. Apply commits the change. |
| Alt+E | Stage Eraser for the selected user. Apply commits the change. |
| Alt+C | Dispatch the existing Clear User or Clear All command using the live applied `clear_all` scope when available, otherwise the committed profile scope. An unapplied draft cannot widen the clear scope. Configuring `clear_all: true` requires authorization for every active user, even without a clear key; Clear All and a configured all-user clear shortcut also require every active user's grant. |
| Alt+O | Open the brush-color popup and focus its keyboard hex field. Valid `#RRGGBB` text stages RGB only; alpha is separate. Invalid or partial text does not change staged RGB. Escape or outside click closes the popup. Native acceptance observed the popup opening and focus; keyboard entry was covered by source tests, not this native run. |
| Alt+B | Focus the selected brush activator choice. |
| Alt+S | Focus the clear-shortcut field. |
| Alt+V | Dispatch the existing PNG Export command with the current path, source and ink scope, subject to the existing ready, busy, pending, path and selected-user guards. It does not invent a path. |

Settings remain staged until explicit Apply. The collapsed **Keyboard shortcuts** row documents the keys. Alt+V reuses the existing annotation export operation and its scope; no new export mode is introduced.

The accepted run completed all 26 GUI steps, nine Alt receipts and 18 scoped virtual-routing calls in 41.156 seconds. Seven annotation acknowledgements matched the observed effects. Alt+E and Alt+D/Apply changed only selected user A; Alt+B/S focused their controls; Alt+O opened/focused the editor; Alt+V exported a fresh transparent ink-only PNG with both known strokes and no target PID/HWND; Alt+C cleared selected-user ink, then all-user ink after `clear_all` was applied. Ordinary Stop and process cleanup passed. Formatting, strict Clippy, 431 tests (0 failed, 3 ignored), and x64/i686 app/observer builds passed for the recorded source. See the [evidence](../runtime/draw-access-keys-ui-attempt002/evidence.json) and [producer verification](../runtime/producer-draw-access-keys-20261021-attempt003/verification.json).

Native attempt 001 remains FAIL. Its strict antialias color oracle was corrected in the acceptance runner before the fresh attempt 002; the failed result is preserved and not relabeled. The separate [GUI stall diagnostic](../runtime/gui-stall-rerun-attempt001-review.json) observed 12/12 steps, 17.922-second configuration at the historical window position `[2224, 0, 2560, 679]`, and ordinary close in 155 ms. It does not establish a reliability fix or explain the intermittent stall; P1 remains open. Real devices, arbitrary target applications, desktop-wide routing, same-process multi-user isolation, complete compact-page UX and full Ultra parity remain unverified.
