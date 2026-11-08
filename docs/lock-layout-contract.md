# Compact Lock configuration

Status: bounded x64 Lock layout acceptance passed on October 20, 2026.

The Lock tab offers Legacy, Desktop, Window, Monitor and Region movement bounds through **Movement bounds**. Its summary distinguishes the editable draft from the worker-applied mode, while window controls separately identify the staged next-Start choice, highlighted candidate and current application. The footer names the current mode's **Apply** action. Window mode follows the selected window's current client rectangle; it does not copy coordinates into a fixed Region.

Region exposes labeled Left, Top, Right and Bottom values. Scrolling is supported: the acceptance confirms each named response is reachable and fully visible at 320×360 and 360×360, not that every control fits at once. Invalid bounds leave Apply disabled and preserve the draft. Monitor availability and an unavailable selected path remain visible and actionable. The **Refresh windows** control keeps its full accessible name while using a compact icon.

The [bounded assessment](lock-layout-assessment-2026-10-20.json) records one 54.656-second virtual-only run with 75 actual GUI actions, 26 fully contained response samples and 14 original product captures. It exercised all five choices, per-user staging and clearing, validation, one live A selection and Region Apply, and read-only GUI-connected MCP Inspect; it sent no routed input. The independent [actual review](../runtime/lock-layout-ui-attempt007-review.json), [source review](../runtime/lock-layout-ui-source-v7-review.json), [producer review](../runtime/lock-layout-producer-attempt005-review.json) and producer verification all passed.

This evidence accepts only the sampled compact Lock workflow. It does not establish simultaneous visibility of all controls, full-page UX, general P1 reliability, hardware behavior, monitor reconnect or mixed-DPI behavior, broad application compatibility, or full Ultra parity. Failed native attempts 001–006 remain recorded as failures.
