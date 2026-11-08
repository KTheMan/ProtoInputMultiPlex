# Keyboard configuration

The application adds keyboard and accessibility behavior around the pinned Ouroboros slider atom. All eleven responsive call sites for seven user settings use this adapter; the framework source and painted controls remain unchanged.

| Setting | Range | Arrow step | Page step |
| --- | --- | --- | --- |
| Horizontal and vertical motion speed | 0.1–8 | 0.1 | 1 |
| Vertical and horizontal wheel multiplier | 0.1–8 | 0.1 | 1 |
| Cursor size | 12–256 px | 1 px | 10 px |
| Overlay text size | 8–48 px | 1 px | 10 px |
| Idle timeout | 250–600,000 ms | 250 ms | 2,500 ms |

Tab and Shift+Tab traverse the normal focus order. On a focused, enabled slider, Right/Up increase and Left/Down decrease by one step; PageUp/PageDown use ten steps, and Home/End select the exact endpoints. Arrow keys stay with the focused slider. Values clamp to the existing limits. Rendering preserves valid values between steps; keyboard increments are additive, and opening a page does not quantize saved settings. Existing disabled gates, Apply behavior and profile persistence remain in force.

Slider identity derives from its user/control key independently of responsive parent layout. Windows accessibility nodes expose the slider name, value, range and step. Enabled nodes offer value changes and increment/decrement actions where applicable; these use the same bounds as keyboard changes. Nonfinite requested values are ignored. Native accessibility is enabled through the existing eframe `accesskit` feature, with no framework revision change. Home user buttons also expose the user's bounded name and role/device/selection status while retaining the visible ordinal.

MCP `routing_settings` continues to replace the user's settings under its existing permissions. GUI keyboard focus changes configuration controls; MCP `routing_input` sends virtual input to selected target applications. This feature adds no MCP tool, target grant, global shortcut or OS input injection. The opt-in `--ui-script` fixture generates keys only inside egui, records actual focus/activation, and bounds each Tab search to 24 key pairs.

The [independent assessment](keyboard-configuration-assessment-2026-10-15.json) accepts a bounded virtual group. The final producer passed formatting, strict Clippy, 219 workspace tests and both Windows builds; i686 was built only. The first run accepts 22 observations covering natural navigation, focused Right/Home/End/PageDown, named Home buttons and a native Motion slider value change, generic Apply acknowledgement and Stop. It sent 42 local key pairs, one UIA value write and no routed packets. Its 23rd observation interrupted Save before completion and remains unproven. A separate five-click stopped GUI continuation seeds the accepted profile explicitly, creates a fresh destination through real Save, waits for commit, then verifies actual Load and unchanged file bytes. This is distinct from an uninterrupted roundtrip or an existing-file backup test.

Root and the independent tester inspected three originals: Home selection, the focused Motion value and readable loaded-profile controls. Periodic screenshot write times are not request-associated freshness evidence; JSON and UIA are authoritative for numeric state and saved-file checks. Disabled behavior and responsive identity also have egui-frame tests. The Windows UIA sample establishes only the observed nodes/action, not every slider, screen-reader compatibility or full application accessibility. Generic Apply acknowledgement is distinct from independently reading a worker's applied numeric value. Full UX and Ultra parity remain open.
