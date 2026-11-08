# Horizontal wheel preservation

**Status: implemented; bounded virtual acceptance passed.** The independent assessment covers automatic and concurrent assigned routing to cooperative owned targets. This is not a claim of hardware or general application compatibility.

## Intended behavior

The Rust `HorizontalWheelPolicy::Preserve` option sits alongside the existing ignore and convert-to-vertical policies. It preserves signed `WM_MOUSEHWHEEL` deltas and delivers them to an explicitly resolved target `HWND` after checking its PID against the current target policy. The screen point comes from the virtual cursor; `MK_*` flags come from that virtual user's held state. Horizontal scale and inversion are independent of vertical settings. Existing policies and defaults remain unchanged.

Support automatic desktop routing and concurrent assigned routing when a virtual cursor is available. Fixed assignments without a cursor must reject Preserve before any profile or worker mutation. Horizontal wheel dispatch must not create a click, change switched-mode root ownership, capture a drag, or acquire/retain a button hold. It should target only a currently authorized window under the virtual cursor; it must not move the OS pointer or broaden target scope.

The Ouroboros **Preserve horizontal wheel** control exposes independent scale, inversion and Apply. Complete `routing_settings` replacement includes Preserve and retains ignore/convert behavior. `routing_input`, macro playback and replication preserve the horizontal axis through the supported paths. Unsupported session modes remain visibly unavailable when they cannot resolve a safe target.

## Acceptance evidence and limits

The [independent assessment](horizontal-wheel-assessment-2026-10-14.json) passed six native groups, seven distinct native snapshot IDs, 21 automatic-UI and 14 assigned-UI control occurrences, plus a switched-mode arbitration supplement. It verified actual signed `WM_MOUSEHWHEEL` delivery, scale/inversion, virtual modifier flags, unchanged ignore/convert behavior, and horizontal-axis replay through a macro and graph edge. In switched mode, one user's click root and captured primary hold remained intact while another user's horizontal messages were delivered. The assigned smoke used an owned x64 target; automatic native cases exercised x64 and x86 targets. Two original setup-prefix artifacts remain `NOT ACCEPTED` and are preserved unchanged. Invalid PID/HWND, fixed-mode rejection and helper bounds have source/producer checks, not independent live coverage. No hardware, general application scrolling compatibility, or full Ultra parity is established. See the [Ouroboros capture](horizontal-wheel-control-center.png) and [validation record](validation.md).
