# Control center cleanup

Implemented and passed a bounded independent visual/workflow gate. This is a compact Home/navigation result, not a full Ultra UX-parity claim. The [assessment](control-center-cleanup-assessment-2026-10-14.json) and [evidence manifest](control-center-cleanup-evidence-manifest-2026-10-14.json) preserve exact hashes and run limits.

The layout reference is the [official MouseMux main window](https://www.mousemux.com/images/pages/manual/main-ui.png): user list, session/mode controls, then configuration actions. Reference pixels stay in ignored research storage; the application retains original Rust code, branding, visuals and Ouroboros components.

The default window opens on a compact Home page. A scrollable user list comes
first and shows each user's number, color, name, role, and pointer/keyboard
availability. Selecting a row updates a short status line; `Configure user`
opens the existing per-user editor. Start, Pause/Resume, and Stop sit
below the user details, with advanced routing options collapsed.

`Settings` opens a short category page. `Users & devices` and
`Profiles & backend` remain available there, and the existing tools are grouped
under `Tools`. Tool pages keep their current controls and return through
`Back to Home`. This avoids a permanent roster sidebar and the wide feature-tab
row while preserving the existing user, profile, automation, replication,
annotation, magnifier, agent, and output-rule workflows.

The default window is 420×600 logical pixels with a 360×480 minimum. Opening a
secondary page expands it to 900×720; returning Home restores the compact size.
UI font families named Iosevka use egui's bundled proportional face; explicit
monospace and icon fonts keep their registered families. The layout uses the
existing Ouroboros components and theme tokens. It adds no device attachment,
session permission, or routing behavior.

The UI script launcher follows the same Home-first flow as a normal launch. The assessment records 51 actual control occurrences: 15 offline navigation clicks, 20 accepted actions in an interrupted live prefix, one visual-column check, 11 Stop-tail controls and four routing-options controls. Three original aggregate artifacts remain NOT ACCEPTED; the live prefix's final Stop was verified by a separate tail. A separate column review rejection and a mistaken footer review record also remain archived with their original outcomes.
Action observer labels for the home and settings path are `Select user N`,
`Configure user`, `Settings`, `Back to Home`, `Users &
devices`, `Profiles & backend`, and the existing tool names such as `Input lab`
and `Agent Access`.

The [independent gate plan](control-center-cleanup-acceptance-plan.md) covers owned window geometry, many-row selection/navigation and one virtual session's Pause/Resume/Stop. The [home capture](control-center-home.png) shows the accepted compact roster and pinned footer; default and secondary-page dimensions were checked from owned windows. No worker started during offline navigation, and the 22-tool MCP inventory and permissions were unchanged. CJK glyph boxes, mixed-DPI geometry, pre-existing enabled Agent Access grants, tray/startup, hardware input, complete accessibility and broader Ultra UX remain outside this group. No application opens or attaches a saved target automatically.
