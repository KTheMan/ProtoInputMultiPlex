# Keyboard delivery contract

The keyboard page configures where an eligible virtual user's keys are sent inside that user's selected application. The primary workflow reference is the [Keyboard section of the MouseMux manual](https://www.mousemux.com/pages/manual/); it informs the familiar pairing and delivery workflow, not a claim of feature parity.

## Configuration

The profile field is `seat.cursor.keyboard`:

```json
{
  "enabled": true,
  "focus": "focused",
  "application_focus": "compatibility"
}
```

`enabled` controls the raw virtual keyboard source. `focus` accepts `clicked`, `focused`, `native`, `active`, or `caret`. Older profiles that omit this field keep the legacy default: enabled with `clicked` delivery. A virtual mouse/keyboard pair newly initialized by Add emulated user or MCP Start uses `focused` delivery only when concurrent assigned or automatic desktop routing supports it; physical and fixed-relative users retain the legacy default. Generated session-only clones inherit their source user's keyboard settings.

The UI keeps device pairing and unpairing in the stopped-only keyboard selector. Delivery policy can be edited while routing, then committed through the existing Apply action. The controls are visibly disabled when the user has no keyboard, has a physical keyboard, or uses fixed-relative routing. Nondefault keyboard policy is rejected by the runtime for unsupported users.

## Application focus

`application_focus` is separate from the recipient choices below. Compatibility is the default; opt-in Native controls is available while stopped for automatic desktop routing with a virtual mouse and paired virtual keyboard. A live policy change is rejected before held-input release or settings mutation, including while paused or unattached; other valid settings retain their release rules. Standalone agents set ordered `routing_start.initial_keyboard` at startup. See the [Native controls contract](native-controls-contract.md) for its bounded ordinary-control focus/Tab/capture evidence and unresolved P1 GUI reliability issue.

## Recipient choices

- **Clicked control** uses the last clicked eligible application window and performs no focus query.
- **Focused control** is the recommended choice. It makes up to three immediate focused-control queries, then falls back to the clicked window.
- **Direct focus lookup** makes one focused-control query, then falls back to the clicked window.
- **Active application window** makes one active-window query, then falls back to the clicked window.
- **Caret window** queries the caret window once; if it cannot be used, it tries the focused control and then the clicked window.

Queries address the clicked target's own nonzero GUI thread. They do not query the global foreground thread, activate a window, or change Windows focus. Candidates must remain in the same PID and clicked root or its owned popup chain before use. The live recipient display is diagnostic state: it reports the observed clicked/resolved windows, fallback state, and status. A resolved window is a read-only candidate, not an acknowledgement that input reached the application.

## Source enablement and cleanup

When disabled, raw keyboard presses and text do not enter recording, keyboard-bound Escape handling, or keyboard replication. Existing key-up cleanup remains deliverable so held keys can be released. Mouse-button-to-key output conversion stays active and retains its matching key-up behavior; disabling the raw keyboard source does not disable mouse-to-key mappings.

Changing keyboard settings cancels queued keyboard copies and releases held keyboard paths before the new settings are committed. If release fails, the settings remain uncommitted and the session reports the failure for recovery.

## GUI and MCP state

The MCP `routing_settings` tool continues to accept the complete per-user `CursorSettings` object; its nested keyboard field has the shape shown above. No new MCP tool is required. `routing_state` exposes the last acknowledged settings and a per-user keyboard snapshot with `settings`, `clicked`, `resolved`, `fallback`, and `status`. The snapshot's recipient is diagnostic candidate state, never delivery proof.

The GUI tracks acknowledged keyboard settings by stable seat identity. It follows incoming settings while the local draft still matches the previous snapshot and preserves a draft the user has edited. The Keyboard page shows candidate-versus-live differences and keeps recipient details collapsed by default.

The revised disposable fixture keeps the clicked and focused recipients distinct through target-owned focus-preserving message handling. It reads genuine GUI-thread state and never resets focus after injection or overrides the resolver. This October 15 evidence proves a bounded recipient mechanism under Compatibility. The later opt-in [Native controls assessment](native-controls-assessment-2026-10-19.json) separately accepts ordinary-control focus persistence and Tab in two owned x64 processes; it does not expand the historical assessment or establish arbitrary-application behavior.

## Validation status

The bounded owned x64 native/data and two normal-size originals passed independent review. The minimum correction separately passed eight GUI actions, a whole off-draft checkbox and Apply footer, subsequent acknowledged settings, and cleanup. Recipient modes require scrolling at 320x360; simultaneous whole-checkbox-plus-mode visibility remains unproven and is excluded from acceptance. Physical-device, native fixed-relative and arbitrary-application focus/Tab behavior remain unverified. See the [assessment](keyboard-delivery-assessment-2026-10-15.json) for source/build boundaries and preserved earlier verdicts. This does not establish full MouseMux Ultra or UX parity, desktop-wide isolation, or same-process multi-user isolation.
