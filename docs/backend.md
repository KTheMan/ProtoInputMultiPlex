# Backend integration

The [Rust migration proposal](protoinput-rust-port-proposal.md) outlines a staged host-control port, conditional later scopes, retained native components, and acceptance gates.

## Build

`scripts/build-backend.ps1` builds pinned ProtoInput for x64 and x86 with Visual Studio Build Tools 2022/v143 and the installed Windows SDK. It stages DLLs and injectors under ignored `runtime/`. Both architectures built successfully on October 13, 2026. `doctor` loaded the real 64-bit loader and resolved every required export.

Install C++ ATL normally, or use `scripts/prepare-atl.ps1` for the local MSVC 14.44 fallback. It reads the installed Visual Studio catalog, downloads its ATL Headers/X64/X86 payloads from Microsoft's download host, verifies SHA-256, and extracts them into ignored `.native-tools/`. These are machine-local build tools, not product runtime assets.

Compatibility is isolated in `scripts/native-compat.targets` and `patches/`:

- Skip legacy C# packaging references when building EasyHook's native static library.
- Build EasyHook and BlackBone before the loader, correcting missing upstream dependency order.
- Supply installed Windows SDK/toolset and ATL include/library paths.
- Add required dependent `typename` qualifiers for current MSVC.
- Correct EasyHook's undefined `_NTDDI_WIN10` and respect existing SDK version definitions.
- Accept the `ERROR_PIPE_CONNECTED` race, fix the keyboard-unplug vector erase, initialize cursor bounds when selecting the target window, and remove installed hooks when the controller pipe disconnects.
- Convert wheel-message coordinates from the private client cursor to screen pixels, and preserve an explicitly selected child/window on button release. Routing policy and ownership remain Rust code.
- Isolated side-button compatibility forwards Raw Input button 4/5 down and up as `WM_XBUTTONDOWN`/`WM_XBUTTONUP`, places `XBUTTON1`/`XBUTTON2` in the high word of `wParam` (`<< 16`), and admits the tagged up messages through the mouse-button filter. The x64/x86 backend builds compiled and staged with this patch; virtual button mapping and delivery passed the focused settings acceptance. Physical-device side-button delivery remains unverified.
- Queue-fence compatibility adds a fixed-width 24-byte `MuxQueueFence` pipe packet and serializes complete loader pipe frames. Its isolated delta is nested in the consolidated top-level compatibility patch; original vendor patch bytes are retained. See the [compatibility record](queue-fence-compatibility-2026-10-14.json).
- The development [native keyboard-state adapter](keyboard-state-backend-patch.md) forwards consumed message facts to Rust and acknowledges provider startup/shutdown. Its isolated overlay follows the lifecycle overlay. The [bounded keyboard assessment](native-keyboard-assessment-2026-10-20.json) accepts an owned Ctrl+S accelerator and two-process classic WordPad typing/save/Stop workflow; broader API/application support remains open.

Patches apply idempotently. A conflict fails the build rather than resetting vendor edits. The vendored checkout shows these documented modifications; its base commit stays pinned.

## ABI

Bindings follow `vendor/ProtoInput/src/ProtoInput/ProtoInputLoader/include/protoloader.h`. Exports use the C ABI (`cdecl`); Windows `unsigned long` and instance handles are `u32`, and C++ `bool` is Rust `bool`. Resolved device handles must fit the upstream 32-bit parameter. Runtime paths are absolute UTF-16 with a trailing backslash because the loader concatenates filenames.

The loader uses `LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS`. Required symbols resolve before mutation. All device assignments, target existence/bitness, and target-specific runtime files are checked before injection. A process can have one seat. Empty keyboard selection accepts no keyboard devices.

Setup freezes input, selects devices, installs hooks 0–10 and filters 0–7, enables generated messages and the fake cursor, starts focus messages, then unfreezes input. No global input lock or Explorer suspension is used.

## GUI lifecycle

The Rust control center serializes a profile to a disposable `session-worker` process. The worker resolves devices again and owns the non-thread-safe native loader. A supervisor thread reads explicit status records from stdout, sends pause/resume/stop commands, and enforces startup (45 seconds per seat), control (10 seconds), and stop (3 seconds) deadlines. Native calls never run on the UI thread.

Before sending a profile, the supervisor puts the worker in a kill-on-close Windows Job. Closing the job terminates its worker and injector descendants, including if the control center crashes. Injected target applications are not members of that job. On a normal stop, session drop requests freeze, focus-loop stop, message/filter/cursor disablement and reverse-order hook removal. The native disconnect patch also attempts cleanup when the pipe closes unexpectedly.

This is best effort. Upstream configuration calls return `void`; **configuration sent** does not certify that hooks installed or that input reached an application. Worker termination does not unload remote DLLs or restore every Raw Input registration. Restart target applications before another session. The unsupervised experimental `attach` CLI should only be used for debugging disposable targets.

Device disappearance requests stop; reconnect does not silently reassign handles. The supervisor owns Ctrl+Alt+Shift+F11 for concurrent/automatic sessions, so a blocked native worker cannot prevent the stop deadline. Independent users within one process and broad desktop compatibility remain separate work.

## Reproducing hardware checks

1. Run the control center from the repository root and add a connected mouse as a user.
2. Pair a keyboard; under Application choose **Open routing test window**.
3. Repeat for a second physical mouse/keyboard and a second test window.
4. Start routing. Move each mouse and hold W/A/S/D/Space separately; only its assigned target should change. Test simultaneous use too.
5. Check pause/resume, unplugging, normal stop, target exit, and controller exit. Restart target processes between sessions.

Test windows write counters, cursor API position, and the five test-key state bits to `runtime/probe-<pid>.json`. They do not log typed text. Compare before/after reports; a DLL-load or mocked-ABI test alone is not routing evidence. See `validation.md` for completed checks and remaining gaps.

## Emulated input

The Rust UI can create application-local `virtual://mouse/N` and `virtual://keyboard/N` identities. These are not installed Windows HID devices. Rust generates and validates fixed-width packets, and the worker broadcasts them to every attached instance through the optional `MuxSubmitInput` export. A small native transport adapter converts them to `RAWINPUT` and enters the existing ProtoInput processing path. Native device-selection filters enforce per-target isolation. Physical packets still use the original Raw Input capture path.

Automatic desktop sessions use a separate Rust adapter: start without injection, select an eligible window on virtual click/wheel, then lazily attach once per process. All session virtual identities are admitted to an instance because upstream has no device-removal API; Rust submits each packet only to the process owning that route. It selects the actual target HWND, maintains private client cursor coordinates, and remembers the original press window for releases. Process handles detect exit and prevent PID reuse from hiding it. This supports ownership transfer between users, with held-input arbitration; it does not add multiple native input states inside one process.

Virtual controls support relative motion, mouse buttons and wheel events, named keys, and Unicode text. Named keys are mapped through the active Windows keyboard layout into a virtual-key/scan-code packet. Text is validated in full, encoded as UTF-16 units, and sent through the native emulated-input v2 packet format; an older adapter rejects text before dispatch and asks for a backend rebuild. The text path does not use the clipboard. No `SendInput` or shared desktop-pointer movement is used. Emulation is useful for repeatable routing tests, but does not validate USB discovery, unplugging, or hardware timing. The reserved identity range is checked against enumerated physical handles before attachment.

Virtual routing requires backend ABI version 3. Version 3 includes both `MuxSubmitInput` and `MuxQueueFence`; the latter lets Rust request a bounded queue-level acknowledgement during shutdown. The v3 backend bridge compiled and staged for x64 and x86 in the [native build record](queue-fence-native-build-2026-10-14.json); this verifies compilation and staging only. A version 2 backend cannot satisfy deterministic cleanup for virtual routes and is rejected before virtual routing starts. A fence receipt means an eligible target message pump removed the private marker after earlier queued messages; it does not prove that application code processed those messages. See the [queue shutdown plan](queue-shutdown-plan.md) for the pump conditions, timeout behavior, and acceptance boundary.

The worker's session-local automation runtime records accepted virtual mouse, key, move-to and text actions in ordered clips. Clip validation bounds event count, duration and size and checks each action against a current seat. Playback uses a bounded number of due actions per tick; stored packet device IDs are replaced with the current source or destination seat's assigned virtual device at delivery. Replication builds and validates a candidate directed graph before replacing the active graph, rejecting duplicate edges, empty category filters, invalid seats and cycles. Failed automation commands report an error event without stopping the routing worker; cancellation releases held input. Native and Ouroboros UI automation and focused settings/remapping acceptance passed for bounded virtual x64/x86 targets; the [Output Rules assessment](output-rules-assessment-2026-10-14.json) covers automatic concurrent routing only. Queue-fence Stop acceptance is limited to ordinary unfiltered message pumps; see [the shutdown plan](queue-shutdown-plan.md). Physical devices, fixed-process assigned output conversion, i686-controller behavior, broad application/message-pump compatibility, Windows pointer ballistics and gamepad output remain outside accepted scope. Horizontal-wheel preservation passed its separate bounded virtual assessment.

For a repeatable UI check, build the Rust app and native runtime, then run `scripts/test-emulated.ps1 -Executable <absolute-x64-exe> -SecondExecutable <absolute-x86-exe>`. The hidden `gui --ui-script` option accepts a local JSON list of button labels and delays. Its pointer events enter egui only. It exercises actual button responses, the supervisor, injected hooks, and target API counters without moving the Windows pointer. Reports are saved under `runtime/emulated-*`. The runner owns and closes only its disposable windows.
