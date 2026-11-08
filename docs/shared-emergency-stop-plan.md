# Shared emergency Stop — independent acceptance plan

Status: preparation only. No app, native input, test, Cargo or broker launch is authorized by this plan. Final frozen source approval, producer checks and explicit ROOT READY are required. The clean-restart group is closed; its saved-release/commit implementation is retained, not replayed as a separate family.

## Scope and budget

Two sequential owned phases, with one exact copied x64 executable shared by every controller and broker **within each phase**. The broker namespace includes executable path/metadata as well as logon/window-station/desktop identity; distinct copied image paths are not a valid coexistence fixture. Use only explicit disposable probe PIDs, virtual devices and their native traces. No hardware, RustDesk, SendInput, foreground changes, global pointer input or desktop capture.

Hard caps: **40 mutating MCP calls, 24 `routing_input` calls, four actual GUI clicks plus one disabled observation, two original GUI frames**. Expected workflow: native 26 mutations/16 input calls; GUI peers 12 mutations/8 input calls, for 38 mutations/24 input calls combined. Read-only state/owned-window inventory and process waits are recorded separately. One exact owned `WM_HOTKEY` message and one deliberate owned broker termination are external lifecycle actions, not input packets. No x86 native execution matrix; both Rust builds remain mandatory.

### 1. Actual standalone coexistence, isolation and shared hotkey

Start two standalone MCP sessions on disjoint owned ordinary targets A and B. Both use the same run executable/runtime and each owns one virtual user. Establish real EDIT recipients with MoveTo/primary Down/Up; hold A F6 and B F7, requiring exact native Down witnesses. Record two distinct live workers/overlays, shared broker PID/creation/image/command and hidden HWND before proceeding.

First an owned harness thread registers the fixed chord without sending input. A refused standalone Start must return an emergency/F11 acquisition error before worker spawn, with all target traces unchanged. Unregister the owned chord before positive setup. Then a third standalone controller starts an empty automatic route to A and attempts the actual target click: require the exact nonfatal `Routing unavailable: <A eligibility reason>` Busy status, no held/captured input and no new native output. A processed ACK is not delivery. Stop this empty competitor and verify its worker/overlay closure. This checks that shared registration does not weaken target ownership; it is not a new routing family.

Explicitly Stop A, observe its original-recipient F6 UP/worker exit/overlay cleanup, then disconnect its MCP process. B remains running with its F7 hold and unchanged trace. Deliver one F5 Down/Up pair to B as the bounded continuity witness. The original broker must retain the same live PID/creation/HWND even though its first subscriber/controller has exited.

Start a fresh participant C on its own third target and hold F9 at its EDIT, while B retains F7. Post **only** the exact `WM_HOTKEY` to that verified owned broker message-only HWND: message `0x0312`, ID `0x504d`, lParam `(0x7a << 16) | 7`. Require B and C's normal supervised Stops, plain F7/F9 UPs at their saved recipients, worker exits0 and overlay destruction. Do not synthesize a physical chord. After the final subscriber drops, require broker/window exit and an owned registration probe to register/unregister the fixed chord successfully. This registration probe sends no input.

### 2. Two standalone sessions plus GUI default Stop; broker loss

In a new sequential run, start two standalone sessions on fresh disjoint owned probes D/E, with F6/F7 held at known EDITs. Launch the owned GUI from the **same exact copied image**, using a third fresh assigned virtual target F and explicit default Stop preference. Other shortcuts, tray/minimize/close behavior and startup registration remain off; startup uses only the existing private GUID provider. No Agent Access grants or GUI-connected mutation client is required.

Use an actual Home **Start routing** click. Verify all three workers are running and sharing the same broker; GUI target F has no routed packet/hold. Terminate only the exact owned broker process after validating retained PID/creation/executable/`emergency-broker` command and HWND ownership. Never kill by image name or accept an unrelated broker.

All three sessions must observe emergency unavailability and request ordinary Stop. MCP `last_error` must preserve the emergency notice after normal Stop; require their native saved UPs, worker exit0, overlay cleanup and normal stopped recovery. The GUI must stop and retain its emergency health error: its clean routing recovery can allow restart while the independent emergency gate still disables **Start routing**. Broker disappearance alone must not poison cleanly retired targets. Cleanup errors must retain ordinary quarantine semantics; forced fence/UP failure is source/unit evidence, not another native failure family here.

Observe disabled **Start routing** and capture two error-state originals, normal Home 360×360 and minimum 320×360: readable new emergency error, **Retry emergency stop** and disabled Start are the pixel criteria. Use actual **Retry emergency stop**, not an implicit reconnect; require a new valid broker identity/healthy subscription. Actual **Start routing** then **Stop routing** verifies the explicit recovery path through data and receipts, with no third healthy-state frame. A cropped generic bottom status does not establish full reason readability; inspect exact pixels after cleanup and do not run microtails for unrequested polish.

Close both MCP controllers and the GUI normally. The stopped GUI's default-Stop app-lifetime lease may intentionally keep the broker alive until GUI exit; the final lease must release its broker/window/chord. Reuse of F after clean Stop is necessary setup for this health recovery, not new clean-restart acceptance.

## Source and deterministic unit gate

Review the final complete changed source after freeze, including:

- Protected logon-SID pipe DACL, local/remote-client rejection, desktop namespace and exact peer PID/creation/executable checks. Inventory never grants access or accepts a stale PID. No arbitrary target injection or new permission/schema surface.
- NOWAIT fixed 40-byte timestamped message handling, malformed/oversized frame rejection, bounded client/traffic/admission behavior and connect/handshake/boot timeouts. Scope election must not drop a valid subscriber or allow a stale refcount to keep ownership forever.
- Heartbeat250ms/health1.5s, GetTickCount64 sender timestamps checked after bounded drain (so a healthy service can tolerate delayed polling while stale buffered heartbeats fail), monotonic pulse sequence, sticky failure and no automatic reconnect. Focused production-helper units should cover stale/mismatched frames, backwards sequence, health expiry and no replay of pre-join pulses; do not infer these from only a killed-broker native case.
- Default Stop special-cased into a shared subscription; custom shortcuts retain their existing transactional registrations. Reserved fixed chord cannot target a non-Stop action. Candidate failure/rollback must preserve accepted shortcuts, subscription and health state; explicit removal may clear sticky error only after successful commit. Retry must reject a pending preference transaction.
- Standalone subscription is acquired before worker spawn. An unrelated application owning the fixed chord must prevent a WELCOME/worker start, not steal registration. Source/unit evidence may establish this refusal; no additional native conflict matrix is required.
- Broker has no first-subscriber lifetime dependency; process/pipe leases and pending handshakes have bounded cleanup. Last-valid-lease close must release the actual native hotkey/window. Joining/exiting races and failed publication must not silently lose emergency coverage.
- Emergency-unavailable notice is separate telemetry and cannot consume an unrelated pending command. It requests ordinary Stop, preserves all-target commit only on successful cleanup/worker exit and retains quarantine on failure. GUI health gate/explicit Retry and MCP notice projection must match actual source.

No forced peer impersonation, malformed native IPC matrix, broker pause/freeze race, fake heartbeat, injected telemetry or new test hook. Such timing/protocol edge cases remain deterministic unit/source coverage unless a concrete failure justifies a separately approved tail.

## Evidence and cleanup

Use current Client and existing owned GUI/geometry/hotkey helpers; declare all HWND/HANDLE ctypes signatures. Discover the broker only through the expected message-only class/title prefix plus exact owned run executable, `emergency-broker` command, retained creation identity and PID-owned HWND. Scope suffix discovery is read-only; it is not authentication by title alone.

Record literal MCP calls and tagged replies, complete native event traces including duplicates/motion, actual worker/broker handles and exits, the original holder's UPs and unaffected peer holds/continuity, health/error/recovery state, actual GUI receipts, exact original image hashes/dimensions and byte-identical copied binaries/runtime/source manifests. Periodic GUI PNG mtime is not request-associated freshness; direct original inspection after closure is required.

Every owned worker, subscriber, probe, broker, overlay, GUI/lifecycle HWND, pipe, private GUID key and descriptor must be closed. Verify recorded terminal codes plus scoped CIM for exact unique run images, and actual chord release after last lease. A deliberate broker kill is distinguished from normal exit; finally cleanup is not substituted for Stop acceptance. Root is the sole producer/native launcher. Preserve every failed raw aggregate byte-for-byte, accept only witnessed bounded subgroups, and stop on a material finding before any correction/relaunch.

Full P1/Ultra/UX, physical shortcut actuation, all desktops/logons, arbitrary apps, same-process multi-user isolation and hardware behavior remain open. No completion claim is authorized until independent source/data/pixel/cleanup approval and root review.
