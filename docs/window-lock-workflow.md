# Exact-window Lock workflow

Status: bounded independent x64 GUI workflow accepted on October 20, 2026. This is a two-user virtual-device and owned-probe result, not full Window Lock or Ultra parity.

The Lock page uses the existing identity-bound window chooser with Window bounds. This follows the selected window's current client rectangle rather than copying coordinates into a fixed Region. MouseMux's [public Lock workflow](https://www.mousemux.com/pages/manual/#lock) is a functional reference; this implementation uses original Rust/Ouroboros controls.

## Configure and start

For automatic desktop routing with a virtual pointer and independent keyboards, open a user's Lock tab and choose Window bounds. Refresh windows, choose an application from the current inventory, then stage it for the next Start. Each user's stage is independent. Choosing a window also chooses that user's mouse/keyboard routing target. Unsupported routing/device combinations keep the chooser unavailable.

Stages are transient. They bind the stable user identity, virtual device paths, routing mode, independent-keyboard setting and normalized allowed-PID policy. Source, policy or mode changes invalidate them. Profile replacement, Stop and failure clear stages. Saving a profile retains its ordinary settings and PID scope, but does not add the staged HWND or creation token. A profile loaded later must choose its windows again.

Start retains one owned controller through terminal cleanup. After the worker configures, the GUI submits staged selections serially using the existing exact PID/HWND/process-token request. Ready remains gated until each matching acknowledgment succeeds. The worker revalidates current identity, eligibility, scope, ownership, held input and automation guards. A rejection or mismatched acknowledgment stops startup and retains an actionable reason after Stopped. Picking remains passive; it does not attach or route input by itself.

## Change a running lock

The same chooser is available while routing. Live selection and settings Apply are separate commits. The page reports the applied bounds mode from worker roster snapshots and effective movement bounds separately from the editable draft. A highlighted option or draft Window mode does not establish an active lock.

Window mode follows the current selected client rectangle as the window moves or resizes. When a target closes or becomes unavailable, existing input-release/recovery rules apply. Successful cleanup forgets the target; Window mode without a current target uses desktop bounds, as disclosed by the page. A failed saved release can retain original focus/ownership until retry or process exit; do not interpret closure as clean release.

## Compact Lock layout

The **Movement bounds** popup offers Legacy, Desktop, Window, Monitor and Region. The Lock summary shows draft and applied mode separately; its footer names the mode-specific **Apply** action. Region values have separate labeled Left, Top, Right and Bottom controls. They can be reached by scrolling; the bounded [layout assessment](lock-layout-assessment-2026-10-20.json) accepts individually visible whole responses at 320×360 and 360×360, not simultaneous visibility of the full form. It also covers invalid Region validation, monitor availability states and one live A Region Apply with B unchanged. Window mode remains dynamic client-rectangle following. This adds no MCP tool or permission.

## Agents

No new MCP tool or permission is added. Use `routing_windows` for current authorized PID/HWND/client bounds and the exact string `eligibility.creation_token`; do not select by title or reconstruct the token from its numeric compatibility field. Use existing `routing_input` action `select_window`, then inspect acknowledged state. Use `routing_settings` to replace the complete acknowledged per-user settings object with `bounds_mode: "window"`, retaining unrelated settings and application-focus policy. Both selection and settings require their existing grants; neither expands the PID allowlist. Inspect actual mode and current bounds before sending input. Acknowledgment is processing evidence, not proof that an application consumed input.

## Accepted bounded workflow

The [Window Lock assessment](window-lock-assessment-2026-10-20.json) records 43 actual GUI actions with two virtual user pairs and two disposable x64 probe processes. It accepts independent transient staging, profile save without HWND/token persistence, serial startup acknowledgments, live selection/settings, owned window move/resize with the other user unchanged, edge clamping to the resized client rectangle, closed-window identity denial, normal Stop, and a staged-target rejection on a fresh Start. The [independent actual review](../runtime/window-lock-ui-attempt002-review.json) approved the evidence with no findings. The separate [compact layout assessment](lock-layout-assessment-2026-10-20.json) covers the mode menu, contextual Apply and compact Region form. Failed compact-layout attempts 001–006 remain preserved as FAIL.

The original Window Lock screenshots are sampled at 540×520, 360×360 and 680×620; they do not establish complete page layout. The later compact Lock assessment adds individually reachable responses at 320×360 and 360×360, not full-page simultaneous visibility. General monitor reconnect/mixed-DPI, physical input, arbitrary applications, same-process isolation, broad P1 reliability and full Ultra/UX parity remain open. Neither acceptance identifies the cause of historical GUI stalls.
