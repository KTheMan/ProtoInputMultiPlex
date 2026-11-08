# Explicit window selection contract

## Select a listed application root

`routing_windows` returns a read-only snapshot of visible application roots and eligibility. Select one with `routing_input` using `action: "select_window"`, `user`, `pid`, `hwnd`, and the exact canonical, lossless `eligibility.creation_token` string from the chosen inventory row. Do not reconstruct the token from its numeric compatibility value or identify a window by title. The server revalidates process creation identity and HWND.

Selection requires an active virtual user in automatic desktop routing with independent keyboards. The target must be eligible, inside the explicit PID scope, satisfy client/cursor-bound intersection and exclusive ownership checks, and pass idle preflight. Selection is passive and transient: it does not click, focus, move either pointer, emit input, or attach. The worker commits the selection and emits `WindowSelected`; the MCP server internally correlates that event, then `routing_input` returns worker state. MCP clients receive neither the event nor a selection request ID. Inspect returned selected PID and status. In the GUI, **Current application** is committed worker state and **Highlighted** is an uncommitted chooser draft. The response confirms selection, not attachment or application delivery. The next real input attaches lazily and positions the cursor in the selected client/bounds intersection.

## Permissions and routing limits

The GUI path is **Configure user > Output > Refresh windows > Window chooser > Select window**. A connected agent needs grants for the user and PID, the PID in the routing allowlist, and **Allow session changes**. Capture permission is separate. Applying an Agent Access policy revokes all clients; reconnect and Inspect under the new policy before further use. No public grant bypass exists. Same-title/same-PID roots may be indistinguishable in the GUI title/PID line; `hwnd` and canonical creation token in `routing_windows` inventory distinguish them for the request.

Before lazy attachment, the server automatically captures and validates the recipient for the first key DOWN and each logical text action. The matching key UP uses its saved recipient; each replica captures independently. A text error may follow partial delivery: inspect state/recovery and do not retry blindly. Persistent `EDIT` focus across separate actions remains open. The `PrintWindow` GPU/compositor discrepancy remains open.

## Bounded validation

V10 source qualification passed formatting, strict Clippy, 368 workspace tests (0 failed, 3 ignored), and default x64/i686 builds. A bounded independent native acceptance passed on x64 only; see the [assessment](explicit-window-selection-assessment-2026-10-16.json) for exact evidence and limits. This does not establish full Ultra parity, hardware behavior, desktop-wide or same-process isolation, arbitrary-application compatibility, persistent `EDIT` focus, or GPU `PrintWindow` behavior.

This qualification covers the V10 development build. The frozen 329-test distribution predates the worker-transport and selection/Text corrections; shipping qualification remains open.
