# Desktop routing with virtual users

Automatic desktop routing gives each user a Rust-owned desktop cursor and click-selected application ownership. Enable **Automatic window targeting**, choose **Switched** or **Concurrent**, and optionally enable **Multi keyboard**. Under **Profiles**, a nonempty eligible-PID list restricts selection; an empty list permits eligible ordinary application processes. The controller, shell, system applications, terminals and RustDesk are excluded.

Starting creates click-through, no-activate overlays without injecting. Use **Input lab** desktop X/Y controls and **Move to point** to position a virtual cursor, then **Hold click** and **Release click** to select the visible window. The backend attaches lazily to that process. A bounded cross-process nonclient hit test distinguishes the client, caption and resize borders; child controls are discovered in the same process. Layered click-through overlays, including those owned by other applications, do not mask targets. An out-of-scope foreground app obstructs selection instead of allowing clicks through to an underlying app.

Switched mode gives a user holding a client click the root role, blocking other client clicks until release. Independent keyboards keep their last clicked focus. Another user can still scroll, move a window by its caption or resize its border in a separate process. Concurrent mode permits overlapping client clicks, drag motion and typing in distinct processes. Mode changes release held input before applying the new policy.

The Rust engine leases each process to one user. Held input prevents another user taking it; an idle process can transfer ownership. A drag stays captured by its original window while crossing other windows. A new click releases that user's keys held in the old application before selecting the new keyboard focus. Process exit clears its ownership, focus and capture; a later click can select another application within the scope. The same-process multi-user case is not independently isolated.

Ouroboros controls show selected PIDs, route status, root role, cursor position and held input counts. Settings can be applied live; assignments and process scope are edited while stopped. Pause releases input; the supervisor owns **Ctrl+Alt+Shift+F11** and enforces a stop deadline even while a native call is blocked. Stop destroys overlays. Restart injected applications before starting another session.

Implementation is split between portable `mux-core::routing` arbitration, `mux-windows::targets` geometry, a lazy `DesktopBackend` delivery adapter, and `protoinputmultiplex::desktop_router` held-input/capture policy. The legacy assigned-process router remains available. The [MCP tools and skill](agents.md) use the same worker through its supervisor.

## Assigned-process profiles

Concurrent mode gives each virtual user a Rust-owned pixel cursor, mouse-button state and keyboard state, paired with one explicitly assigned application process. ProtoInput remains the delivery backend. The desktop arrows are separate click-through, no-activate Windows layered windows; they never move the shared Windows cursor.

## Control center

1. Add an emulated user for each participant.
2. Under **Configure user > Output**, open a disposable routing test window or select an application. Each user requires a distinct process.
3. Enable **Concurrent desktop cursors**, then **Start routing**.
4. Use **Input lab** to move, hold/release mouse buttons, scroll, tap W/A/S/D/Space, or hold a mouse button and W for all users concurrently.
5. Configure **Motion**, **Buttons**, **Wheel**, **Theme**, **Overlay**, and **Lock** per user. **Apply cursor settings** applies the settings to the running session and releases all held inputs first. Device/process assignments remain fixed until stopped.
6. **Pause** releases held inputs and freezes routing. **Resume** restores it. **Stop routing**, closing the controller, or **Ctrl+Alt+Shift+F11** ends the session. Restart injected target applications before another session.

The shortcut must be available before injection starts. F12 is reserved by Windows, and Ctrl+Alt+F11 is already assigned on the development workstation. The three-modifier shortcut was checked independently before use.

Settings are persisted in the profile's optional `cursor` object on each seat. Old version-1 profiles still load with `concurrent: false`; new GUI profiles default to concurrent mode. There is no seat-count gate.

## Implementation

- `mux-core::desktop`: independent pixel coordinates, fractional motion accumulation, speed/inversion and bounds; settings validation.
- `mux-windows::overlay`: thread-owned layered cursor windows, colors/size/labels, transparent hit testing, DPI awareness, emergency shortcut and message pump. GDI rendering currently refreshes at up to 20 Hz.
- `protoinputmultiplex::router`: policy and held-input ownership. Button releases use the mapping that was active at button-down. Pause, stop and settings application release state. Tapping an already-held key leaves the existing hold intact.
- The worker reads commands on a separate thread and processes input, timed releases and overlay updates without blocking for each tap. A batch can hold inputs for multiple users at once. The supervisor's command timeout is not extended by cursor telemetry.
- Concurrent sessions block the shared pointer's `WM_MOUSELEAVE` through the existing ProtoInput message API, so it cannot clear a target's independent virtual hover state. Normal cleanup removes this block.
- Each virtual packet is still broadcast through the native bridge. ProtoInput's device selection is responsible for admitting it only to the assigned process.
- Client rectangles are read in a per-monitor-DPI-aware context. Window-locked cursors use client bounds; unlocked cursors use the virtual desktop rectangle, including negative monitor origins. Clicks and wheel input outside the assigned client area are discarded; releases always reach the original application. Keyboard routing stays fixed.
- Overlay windows are owned by the worker, so Windows also destroys them when the supervised worker is terminated.

## Verification

`scripts/test-desktop.ps1` creates three disposable target processes, including x86, and uses the actual Ouroboros controls to test automatic discovery, switched ownership, caption drag/resize while another user holds a client click, concurrent drags/wheels/keys, target handoff, release of old keyboard state, exit recovery and cleanup. Its events enter egui locally, and it waits for probe reports to agree with worker state before sampling native delivery. `scripts/test-mcp.py` independently exercises a real stdio MCP client and native targets, including normal EOF cleanup.

`scripts/test-concurrent.ps1` launches two disposable probes and the real dashboard. It feeds egui-local events to actual Ouroboros controls, collects probe reports and overlay telemetry, and captures the dashboard from egui's renderer. It does not call Windows pointer or keyboard injection APIs. The second probe can be the x86 build.

The sequence checks concurrent holds, independently delivered drag motion and wheel events, per-user motion/button/wheel transforms, alignment between desktop overlay and target cursor APIs, pause/release/resume, the timed-tap hold regression, settings navigation/application, and destruction of overlay windows on stop. The Windows unit test separately posts an emergency message to its own test thread and checks overlay lifecycle and styles. This exercises the dispatch path without synthesizing a physical keyboard shortcut.

## Scope and remaining parity

These virtual identities are application-local, not installed HID devices. This does not validate physical transport, hot-unplug, RustDesk input, or arbitrary hardware.

The application routes to assigned processes, not whichever arbitrary desktop window lies underneath a cursor. Switched ownership, automatic application changes, independent users within one process, physical input suppression, and Windows shell routing remain unimplemented. Target closure, hiding or minimization stops the session. The current picker persists the process ID; the worker selects its first visible application window, so multi-window applications need further selection policy.

Applications that warp/clip their own private cursor can diverge from the Rust position and need compatibility work. Mixed-DPI monitor transitions, sustained high-rate input, occlusion policies and broad third-party application compatibility remain unverified. The global shortcut is pumped by the worker; if a native call hangs, use the controller's Stop button or its supervisor timeout to terminate the worker. The current overlay uses GDI, not a claimed Ultra-equivalent GPU renderer. Advanced remapping, acceleration curves, extra mouse buttons, full keyboard generation, macros, replication, magnification, drawing, remote collaboration, tray/startup and packaging remain on the Ultra roadmap.

The UX follows the public [MouseMux manual](https://www.mousemux.com/pages/manual/) workflow: colored user roster, device pairing, interaction mode, per-user configuration and profiles. All implementation and visuals use original Rust/Ouroboros code; MouseMux screenshots and assets are not distributed.
