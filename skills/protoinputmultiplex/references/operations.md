# ProtoInputMultiPlex



Use the MCP tools to control Rust-owned virtual input in explicitly selected Windows applications. Routing servers expose 25 tools. Current stopped GUI Setup builds expose `profile_setup` and read-only `application_event_status`; discovery and scope details are in repository `docs/agents.md`. Standalone mode owns a separate routing session. An optional GUI connection shares the running Ouroboros session through a private local descriptor. This is virtual input, not the user's physical pointer, keyboard, RustDesk session, or OS-wide input injection.



For numeric configuration, `routing_settings` replaces the complete settings object under the existing user/session permissions. The native GUI also supports Tab/Shift+Tab navigation, focused slider arrows, Home/End and PageUp/PageDown; named Home controls and slider ranges are exposed through Windows accessibility. GUI configuration keys act on local controls, while MCP `routing_input` delivers virtual keys to selected target applications. See repository `docs/keyboard-configuration-contract.md` for the sampled native scope and remaining accessibility limits.



For standalone MCP, configure the client to launch `protoinputmultiplex.exe mcp --runtime <absolute-runtime-path>` over stdio. For a live GUI session, launch `protoinputmultiplex.exe mcp --gui-connection <descriptor.json>` using the private descriptor from **Settings → Agent access → Agent Access**. Keep the descriptor private. The compact Home is 360×360 logical pixels (320×360 minimum). Its bounded presentation, virtual app-lifecycle/preferences and per-user layout/tool-integration gates passed. Open **Configure user → Draw, Magnifier or Macros** to work on the selected roster user; global tool pages remain available. Full UX parity remains pending. See repository `docs/validation.md` for current evidence and limits.



## Choose a workflow



Use one workflow per server connection. Standalone calls below start and manage an agent-owned routing session. In the GUI-connected workflow, the GUI owns the session: never call `routing_start` or `routing_stop` to start or stop GUI routing.



### Stopped GUI profile setup

For builds supporting GUI callback observations, `profile_setup` with `{"operation":"activity"}` reads `gui_activity` through the authenticated stopped broker without queuing GUI work. It accepts no profile document or file operation. Live GUI connections expose the optional observation at `routing_state.agent_session.gui_activity`. Compare fresh samples within the same connection and GUI instance: counters distinguish callback entry, normal return and unwind. Missing, malformed, incoherent or terminal observations mean unavailable; a returned UI callback does not prove paint or input delivery. Cached ages exclude unmeasured pipe transit. These observations add no routing, capture or recovery authority. See repository `docs/gui-activity-contract.md` for interpretation and acceptance scope.

Optional `gui_activity.render_capture` reports Script screenshot request/receipt
generations, counts, monotonic ages, dimensions and PNG-worker acceptance and
publication metadata. It contains no image data, paths or hashes. The two stages
are independently sampled; do not impose a global ordering from one response.
Missing or contended metadata is unknown. Pending age is the continuous
outstanding-response interval; retirement does not prove completion. These
ages do not time `glReadPixels` or ordinary repaint and cannot establish a
historical stall cause or fix. See repository `docs/capture-observation.md`.

In Agent Access, **Enable setup access** opts into stopped-profile inspection, whole-profile replacement, profile persistence policy, and Save/Load/Import/Export at the GUI-selected paths. Use the displayed MCP server entry with its setup descriptor. Current builds expose `profile_setup` and read-only `application_event_status`; routing, input, capture and global preference tools remain absent. Setup access is separate from live routing permissions.

Call `inspect`, then pass its `revision` as `expected_revision` for a mutation. `configure` takes `document`, a JSON string containing the complete profile. It may stage saved target PIDs and PID scope, but never attaches or starts routing. Keep stable seat IDs and canonical virtual paths for virtual seats. Newly assigned physical seats use fresh discovered interface paths of the right kind; saved disconnected paths can remain for offline editing, subject to Start's current checks. `devices` is fresh native inventory only when `devices_current` is true; `configured_virtual_devices` is a separate profile projection, with no Raw Input handles. `refresh_discovery` queues a scan; inspect its applied generation and freshness afterward.

Profile persistence uses the same stopped Setup authority. Inspect first, then call
`profile_setup` with `{"operation":"configure_persistence","expected_revision":7,"persistence":{"load_profile_at_startup":false,"save_profile_on_exit":true}}`,
replacing 7 with the current revision. Both booleans are required. This updates
only these two saved preferences; it chooses no path, loads nothing immediately,
and grants no routing or input. Inspect `profile_persistence` and
`preferences_operation` after the acknowledgement. Local application-preference
drafts are retained; pending profile file work must finish first. Unclaimed work is
cancelled when authority is revoked or the request is abandoned; a claimed commit
can finish after a timeout or EOF. Re-inspect before retrying. The ordinary
`app_preferences` Configure schema rejects these two fields. See
repository `docs/profile-persistence-contract.md` for startup, Quit and backup behavior.

`save`/`load` use the GUI active path; `import`/`export` use its transfer path. No operation accepts a caller-selected path. A successful file call returns a structured ticket, kind, outcome and revision; a finished-but-not-installed read did not replace the GUI profile. A failed call returns MCP error text without a guaranteed structured receipt. Inspect shows the current file ticket/status, which may have been superseded. Stale revisions, staged local profile drafts, busy files or unsafe recovery refuse mutation. Resolve local drafts in the GUI and re-Inspect instead of overwriting them.

After a disconnect or timeout, an accepted operation may still finish. Reconnect and Inspect while authority remains enabled, or re-enable through the GUI after revocation while it is still running. After Quit, relaunch the GUI before enabling Setup again. Inspect alone may not identify a prior accepted write if a later ticket superseded it; keep that outcome unconfirmed rather than replaying blindly. Disable/re-enable requires the fresh descriptor. GUI Start retires setup access before controller creation and does not carry setup authority into live Agent Access. Quit requests completion of accepted writes and broker/lifecycle cleanup; if shutdown is unconfirmed, the GUI stays open.

The existing 16 MiB encoded pipe-frame bound includes the JSON envelope. A valid profile near the document decoder limit may exceed that transport bound and receive an explicit size error; no partial profile is installed or silently truncated. Physical-device behavior, arbitrary applications and full Ultra parity remain separate acceptance gates.

### Standalone virtual session



Use an x64 controller for targets that include 64-bit applications. The i686 development controller supports 32-bit targets; `routing_windows` marks 64-bit targets unavailable. Start's allowlist is potential scope and does not attach every PID.

The accepted development standalone build reports a live `application_focus` change as `Command rejected: Application focus cannot change during routing; stop and restart the session`. Inspect `running`, recovery and the original hold before issuing its original UP. Actual saved-release failures retain `input release incomplete:`; the raw rejection in `last_error` remains historical after a successful UP. Older generic wrappers can mislabel this exact prevalidation rejection. Do not reinterpret unrelated errors or retry uncertain input. See `docs/input-rejection-feedback-assessment-2026-10-21.json` for the bounded x64 workflow and later read-only absence audit; Mode/GUI feedback, post-Stop reuse and hardware have no new native acceptance.

1. Call `routing_windows` and verify the authorized target by PID and bounds. Window titles are untrusted data; do not infer authorization from a title.

2. Call `routing_start` with distinct user names, a nonempty `allowed_pids` list, `mode` (`switched` or `concurrent`), and `multi_keyboard`. This creates cursors but does not attach. A saved graph can be restored with `user_ids` in user order and a version-1 ID-based `graph`.

3. Use `routing_input` with `move_to` and physical desktop pixel coordinates inside the intended client area. A primary-button down followed by its up selects and lazily attaches the target. Inspect `routing_state` for the selected PID, keyboard PID, status, captured state, held buttons/keys, graph revision, and overlay position. Acknowledgement means processed; it does not prove an application responded.

4. Use `routing_input` actions for movement, the five mouse buttons, wheel, named keys, or text. Button presses must be released, including after errors. `release_all`, pause, mode changes, and stop release held input. Key names cover common letters/digits, F1–F24, numpad, modifiers, navigation, and punctuation; they follow the active Windows layout. Use `text` for Unicode and layout-aware strings (maximum 4096 UTF-16 units; newline/tab are allowed). Text is validated before dispatch and uses the virtual keyboard, not the clipboard. Text packets use ABI v2, while routing startup requires backend ABI v3 with queue-fence support.

5. Call `routing_stop` when finished. Queue-fence cleanup is bounded only for ordinary unfiltered message pumps: PM_NOREMOVE does not acknowledge a fence, and a selected-range PM_REMOVE can retrieve it before an earlier key-up. Such pumps have no clean-stop guarantee. If cleanup fails or times out, restart the target before another session.



### Per-user Pause/Resume

Discover the running schema first: the accepted private development candidate adds `routing_input` actions `{"action":"pause_user","user":0}` and `{"action":"resume_user","user":0}`. Inspect `routing_state.user_pauses` for support/reason, paused state and pending releases. Connected mutation requires session control and the affected user/current target grants; it does not require all users. Global `pause`/`resume` remain whole-session actions.

Pause releases only that user's holds and discards their unpublished motion. Other users continue. Resume requires completed cleanup and an unpaused session; it replays nothing. Repeated DOWNs remain quarantined across Resume until that actor's real UP. Send the matching UP before a fresh DOWN. Source grants remain required: a denied UP keeps its marker until authority returns or the client retires. Inspect after uncertain results instead of replaying input; EOF is not proof that saved native releases completed.

Use Concurrent automatic virtual routing with independent keyboards for the accepted native workflow. Stop affected mapper/replication/forwarding/composite/program work first; unsupported combinations report a reason. Assigned/failure/EOF/program coverage is source/unit only. Pause flags are not persisted. See `docs/per-user-pause-contract.md` and its assessment; live Rust-port integration, P1 and full Ultra parity remain open.

### Explicit per-user window selection

Use automatic desktop routing, an active virtual user, and independent keyboards. A connected GUI client also needs current user/PID grants, the PID in the session routing allowlist, and **Allow session changes**. From `routing_windows`, call `routing_input` with `action: "select_window"`, `user`, `pid`, `hwnd`, and the exact string `eligibility.creation_token`. Do not reconstruct the token from its numeric compatibility field or select by title. The worker commits selection before replying; the MCP server internally matches `WindowSelected` and returns worker state in the tool result. The client receives no raw event or public request ID. Inspect selected PID/status. **Current application** is committed worker state; **Highlighted** is a draft. Selection is passive and transient. Before lazy attachment, the server captures the recipient for the first key DOWN and each text action; key UP uses its saved recipient. Text may be partial on error; inspect state and do not retry blindly. Applying Agent Access revokes all existing clients; reconnect and Inspect under the new policy before further use. No public grant bypass exists. Repository contract: `docs/explicit-window-selection-contract.md`. The historical selection gate does not establish persistent `EDIT` focus. The separate opt-in Native controls workflow below has bounded ordinary-control evidence; GPU `PrintWindow` remains open.

### Window Lock workflow

In the GUI, automatic desktop routing with a virtual pointer and independent keyboards exposes **Configure user → Lock → Movement bounds → Window bounds**. Choose a current window and **Stage window for Start** for each user while stopped. Stages are transient and tied to stable user/device/policy identity; saving a profile retains Window mode but does not save the staged HWND or creation token. After Start, wait for Ready: the GUI sends each staged selection serially and requires matching worker acknowledgments. A closed or invalid staged target stops startup and retains a reason; choose a fresh window before starting again.

While live, selection and settings Apply are separate commits. Through existing MCP, use identity-bound `select_window`, then replace the complete acknowledged settings object with `bounds_mode: "window"` under existing grants. Inspect actual selected PID, applied mode and current movement bounds before input. Window mode follows the live selected client rectangle; after successful target cleanup, no selected target means desktop bounds. Selection alone remains passive. The bounded two-user staging/live-resize/clamp/rejection/Stop gate passed `docs/window-lock-assessment-2026-10-20.json`; physical devices, arbitrary apps, mixed-DPI and full Ultra/UX parity remain outside it. No new tool or permission is added. The compact Lock form distinguishes Draft from Applied, and its footer names the action (for example, **Apply Window bounds**). The bounded 320×360/360×360 workflow with individual controls reached by scrolling passed `docs/lock-layout-assessment-2026-10-20.json`; it does not complete feature-page UX parity.

MCP sessions require a nonempty explicit `allowed_pids` list. For session-only `virtual_users`, choose an already permitted PID and an active source ID. Each Start appends a stable slot and source-to-clone edge; it does not expand scope. Stop only generated users. Original users cannot be removed, removed slots remain tombstones until session shutdown, and clones are not saved to stopped profiles. The GUI's empty PID list preserves its existing unrestricted policy but still requires explicit target selection.



After a standalone cleanup error, inspect `routing_state.recovery`: `stopping` means worker cleanup is still pending; `quarantined` retains affected original target processes until they exit. `targets` excludes unused allowed PIDs. Wait for `can_start: true`, then explicitly select fresh authorized applications and Start; changing the profile or allowlist does not clear quarantine. A late key-up does not establish clean recovery. There is no force-clear, kill-target or automatic restart action. Recovery is in memory for this owning GUI/MCP process; do not describe it as durable across application restarts. For an authenticated GUI-connected client, `routing_state` includes the safe GUI-owned recovery projection; after worker termination, the same connection is inspection-only. See **GUI recovery inspection continuation** for its limits and disconnect flow.

Development-source worker stdin writes now have bounded asynchronous supervision; use the exact artifact/source assessment in repository `docs/worker-transport-assessment-2026-10-15.json` when checking support. A worker that stops reading its pipe can time out while other telemetry is fresh. A successful write or accepted Stop intent does not establish Stopped: inspect terminal/recovery state, retain original-process quarantine, and do not replay uncertain input. The separately frozen 329-test distribution source predates this fix. Connected `routing_stop` still disconnects only this client.

For a later virtual-only supervised session, inspect `routing_windows.windows[].eligibility` for the explicitly selected target. `reusable` is a read-only candidate state after an acknowledged all-target clean Stop; the snapshot does not reserve, attach, or confirm input delivery. Start only within the existing selected PID scope. Direct CLI and physical/legacy sessions remain fresh-target-only. Failed or ambiguous cleanup and worker loss keep touched targets unavailable until their original processes exit. See repository `docs/clean-restart-contract.md` and `docs/validation.md` for bounded acceptance and limits.



The default `Ctrl+Alt+Shift+F11` emergency Stop helper is shared across multiple standalone MCP desktop sessions and the GUI only when they use the same executable path, metadata and version under the same Windows logon and desktop. Supervised standalone automatic-desktop/concurrent routing acquires an independent lease before worker spawn; direct CLI attach and nonconcurrent assigned routing retain existing behavior. GUI subscription requires accepted preferences enabling the default binding. Each standalone MCP desktop session has its own lease: an emergency event stops all subscribers, while GUI Stop, standalone `routing_stop` and EOF remain local. Custom chords remain exclusive; the default chord is reserved for Stop. For GUI-connected work, use the GUI exact binary and preserve each client's grants and input ownership. The helper adds no MCP tool, permission or global agent-trigger capability.

For target-selection Busy/dirty denial only, `routing_input` can return `isError: false` and `running: true` while `routing_state.users[i].status` starts with `Routing unavailable: ` plus the eligibility reason. No held input, capture, or native output is produced. This acknowledgement does not prove input delivery: inspect route status, selected target, capture and held state and reconcile before continuing; do not blindly retry or replay. Other `InputRejected` results are distinct. Recovery views are process-local, but dirty markers on touched/versioned virtual targets survive controller restarts until original target exit; no recovery data is stored on disk.

If broker coverage is lost, inspect the existing `routing_state.last_error` and recovery state; the helper requests ordinary Stop. Do not replay interrupted input. GUI Start stays disabled while its enabled default binding lacks coverage. **Retry emergency stop** obtains a healthy lease; an accepted preference change may instead remove or replace the shared default binding. Start rechecks health when that binding is enabled. A pending preference transaction blocks Retry. Recovery behavior remains session-local as described in the contract.

### GUI discovery freshness

For a GUI-connected client, inspect optional `agent_session.connection.discovery` or `routing_state.agent_session.discovery`. `requested_generation` can exceed `completed_generation` while `in_flight` or `queued`; counts may describe an older completed scan. `age_ms` is time since last completion (null initially), while `sample_age_ms` is time since the GUI published its status and advances even if publishing stalls. Inspect errors and `worker_failed`; absent legacy metadata is null. Fresh metadata is not target authorization, successful Start, input delivery or general GUI-health proof. Do not replay uncertain input or use counts to expand scope. No new tool or grant is added. The bounded delayed-scan/navigation/GUI Stop/close gate passed. The separate background GUI Start gate below covers pending startup; broader P1 reliability remains open. See `docs/gui-discovery-assessment-2026-10-20.json`.

### Background GUI Start

The local GUI owns one pending controller while authoritative device, process, runtime and emergency-stop health checks run on its supervisor. Wait for Ready before enabling Agent Access or connecting; there is no new pre-Ready connection or agent Start permission. GUI **Stop routing** cancels pending preflight. With close-to-tray disabled, closing the GUI also requests cancellation; enabled close-to-tray retains its existing hide behavior. Inspect terminal state rather than treating Stop intent as completed cleanup. Before profile delivery, a canceled launch discards untouched identity watches; after delivery may have begun, existing cleanup/quarantine rules remain. The delayed preflight, same-GUI fresh restart, scoped read-only MCP inspection and close workflow passed `docs/start-preflight-assessment-2026-10-20.json`. Cancellation is cooperative between native phases, not interruption of arbitrary blocked Windows calls. Standalone MCP Start semantics and 23 tools/grants are unchanged.

### Live GUI session

Agent Access startup and retirement run on a retained background Rust owner. Wait for its matching enabled-policy acknowledgement before connecting; disabling or Home Stop may remain pending until cleanup finishes. Re-enable uses the current descriptor; reconnect and inspect the current policy. Stop intent does not establish terminal cleanup. The bounded normal x64 virtual lifecycle core has independent composed acceptance at repository `docs/gui-broker-lifecycle-assessment-2026-10-21.json`, with a later read-only absence audit and preserved failed raw runs. This does not prove the historical GUI stall fixed or a hard OS-cleanup deadline. No tool or grant was added.




The GUI owner chooses active virtual users and eligible target PIDs, then enables **Agent Access**. When present, **Copy MCP server entry** prepares a structured JSON server entry with the exact running GUI executable and `args: ["mcp", "--gui-connection", descriptor]`; **Copy descriptor path** copies only its locator. Wait for the matching enabled-policy acknowledgement and available controls. The default-closed **Additional permissions** disclosure separates acknowledged Applied grants from differing Draft permissions; a draft is not an applied grant. Capture and session changes remain separate permissions and default off. Inspect `agent_session` after connecting to learn the current policy. Applying **Apply access policy** revokes existing client IDs, so reconnect using the current descriptor and inspect the current policy before further writes rather than retrying on the revoked transport. The bounded layout/read-only handoff assessment is repository `docs/agent-connection-handoff-assessment-2026-10-21.json`; native OS clipboard contents, clicked feedback and a rendered one-client count remain unverified. No tool or grant was added.



- `routing_input` acts as the selected active virtual user. `release_all` releases this connected client's owned input across its users; it preserves GUI and other-client ownership.

- The PID policy governs target focus, capture, and click/gesture input. It does not confine cursor movement to a window rectangle. Unfocused movement can position a virtual cursor; a click is checked against the PID policy before targeting. GUI recorders do not include connected-agent input.

- `routing_capture` requires the separate capture grant and returns a PNG of the selected permitted target window, not a desktop screenshot. Limits are 16 megapixels, 2 MiB PNG, and a five-second response deadline. A timed-out provider job remains retained until it completes; forced provider-hang behavior is not established.

- `virtual_users` Inspect is read-only. Start and Stop require session control, an explicit `expected_revision`, automatic routing with a nonempty PID allowlist fully covered by Agent Access, all active users in scope, and permitted current mouse/keyboard recipients. Start appends a session-only clone and source edge without granting that new user. It appears unchecked; select it in Agent Access, apply the policy, reconnect because Apply revokes clients, then Inspect before writing. Until granted, direct input and graph paths through the clone are rejected. Stop normally needs all active users; the only narrower user exception is the same still-connected client stopping its exact clone, while all PID checks still apply. EOF/revocation clears that receipt but leaves the GUI-owned clone. A cleanup error leaves the clone active at the same roster revision with its creator receipt. Inspect before retrying because some releases or cancellation may already have taken effect. Connected clone lifecycle passed bounded virtual acceptance (five groups, 18 GUI observations and three original frames); see repository `docs/connected-virtual-users-contract.md` for limits.

- Settings, output conversion, mode, pause, and resume use session-level permissions. `routing_output` Inspect is read-only; Configure additionally needs the current `session_control` grant and an authorized active user/PID. Pause and mode changes have global effects and may release GUI-owned programs/held input. Mode changes require automatic routing and permission for all active users. Other unsupported live mutations return an explicit error. `routing_stop` disconnects this agent and releases its claims; it does not stop GUI routing.

- `annotation` Configure and ClearUser require **Allow session changes** and an authorized active virtual user. Configure with `clear_all: true` requires permission for every active user, even when `clear_key` is unset; ClearAll and a configured all-user clear shortcut also require every active user's grant. Configure rejects held routing input, open strokes and consumed activator/shortcut keys until release; release your own input or wait for its owner before retrying. ClearUser clears that user's ink without releasing routing claims. A consumed activator still needs its matching up after Clear. Inspect returned annotation state/settings after success. Export kinds are `ink_only`, `selected_target`, `full_desktop`, and `desktop_region`; check the running schema because older builds predate desktop export. Every export requires an authorized active user; `all_users` also requires every active user. `ink_only` requires the existing capture grant; `selected_target` also requires the currently allowed selected target. `full_desktop` and `desktop_region` require the separate **Allow desktop screenshots** grant (default off) instead of target capture. No session-control grant is needed to export. A PID allowlist alone authorizes neither desktop pixels nor a target capture. Only `desktop_region` accepts/requires a physical-pixel rectangle with positive width and height fully inside desktop bounds; negative desktop origins are valid, with a 16-million-pixel limit. The GUI requires an explicit export choice. Region/source choices are transient and are not saved as seat settings. Ink is an immutable request-revision snapshot; one asynchronous export job is retained per routing worker while the MCP call waits for completion. Inspect GUI `export_pending`; another connection can route input during preparation. Client authority and lifetime are rechecked before publication. Preapproval revocation cancels the job; approval is the commit point. After approval, timeout or uncertain result requires inspecting the destination before any retry; do not assume rollback. Stop and cancellation before approval preserve existing destination bytes. October 21 bounded acceptance covers only an owned virtual fixture: repository `docs/annotation-desktop-assessment-2026-10-21.json`. Arbitrary desktops, hardware, mixed-DPI/multimonitor, display drivers, same-process isolation, P1 responsiveness and full Ultra parity remain unverified. Attempts 001-007 remain failed. See repository `docs/annotation-desktop-contract.md`. GUI-connected desktop annotation is allowed only when explicitly requested through an annotation desktop export kind and the current desktop screenshot grant; do not infer that permission from PID scope. Do not control hardware. The Draw page also has local Alt+D/E/C/O/B/S/V access keys for staged tools, applied-scope clear, color editing, focus and existing PNG export; see repository `docs/draw-access-keys-contract.md` for exact guards and bounded acceptance scope.

- `magnifier` Configure requires **Allow session changes** and an authorized active virtual user. Enabling additionally requires capture permission and that user’s currently selected allowed target. Refresh and Export require the separate capture grant, an enabled selected-target magnifier and a currently allowed target; they do not require session-control permission. Export uses a completed cached frame, including while a newer refresh is running, and waits for its bounded background encoder/writer. Authority is checked again before publication. After an approved commit, a timeout or revocation cannot undo an already authorized file; inspect the destination before retrying. Desktop capture remains unavailable through a GUI connection. See repository `docs/connected-magnifier-plan.md` for scope and current acceptance.



- `app_preferences` is separate: Inspect requires enabled Agent Access; Configure needs the independent **Allow app-preference changes** grant. It does not grant routing/session-control rights.



For a still-connected client after an automatic-desktop target exits, inspect the selected PID, keyboard PID, capture and status before further input. Target cleanup does not necessarily clear the originating client's logical key/button claims. Send `routing_input` with `action: "release_all"` on that same connection and await its successful acknowledgement before explicitly selecting a replacement. The replacement must already be in both the running session's desktop PID allowlist and the current client policy. A new PID requires an explicit scope change and, where necessary, a new session; do not silently add it or assume a repeated down starts a fresh claim. Concurrent assigned routing has the separate saved-release recovery described below; native fixed-relative recovery remains unverified.



After EOF, connected `routing_stop`, or policy revocation, the worker retains any failed saved releases and retries them automatically. Disconnection is not confirmation that every Up completed. Observe the GUI Agent Access cleanup status or another already authorized connection's `routing_state.agent_session.cleanup`; rows describe the retired client, remaining source-owner obligations, attempt count at that snapshot, and bounded last error. Other clients' shared holds are preserved. Never reconnect as the retired client or replay its Down/text/program; there is no force-clear action. Active clients plus pending cleanup share eight admission slots, so a new connection can be rejected until saved cleanup succeeds. Valid policy changes still commit and revoke every old client when cleanup is pending. This state is session-local, not durable across application restarts.



A GUI-connected agent may request desktop pixels only through an explicitly requested annotation `full_desktop` or `desktop_region` export with the current desktop screenshot grant. PID allowlists do not authorize desktop capture. This does not authorize hardware control. See repository `docs/gui-agent-plan.md` and `docs/validation.md` for acceptance scope and limitations.



## Application preferences



`app_preferences` accepts `operation: "inspect"` or `"configure"`. Configure requires `settings` containing `launch_minimized`, `minimize_to_tray`, `close_to_tray`, and `shortcuts`, and replaces those values together. Shortcut bindings cover Stop, Pause/Resume, Toggle Mode, Toggle Routing, Switched, Concurrent, and Toggle Keyboard; bindings are validated together before they replace accepted bindings.

If saved shortcut registration fails at GUI startup, the GUI opens visibly with the complete set inactive and Start disabled. Repair locally using Home **Retry shortcuts** or edit the full policy in Application preferences and Apply; Retry uses the latest accepted set. Connected replies may include live `shortcut_status`; standalone document replies use null and do not prove another process registered its shortcuts. This adds no tool or grant. See `docs/startup-shortcut-recovery-contract.md` for accepted virtual scope and limits.



Current source adds optional `appearance_mode` (`dark`/`light`) and `zoom_percent` (50–200 inclusive in multiples of 5). Omit either to preserve its current value; legacy documents default to dark/100%. Bounded offline-GUI verification passed Light/Dark selection and persisted Light/75% rendering across relaunch; native lifecycle has a separate bounded virtual assessment, and this does not establish general-device acceptance. Discover the running server's schema before sending new fields to an older build.



`show_app_launcher` is optional in `app_preferences` Configure. Omission
preserves the existing value. It defaults true on older preference
documents. When disabled, Home shows Exit in place of Apps.
[Apps launcher assessment](../../../docs/apps-launcher-assessment-2026-11-06.json)

Standalone Inspect reads the chosen preferences document without a write grant. Standalone Configure requires `--allow-app-preferences` and writes only that document; it does not launch the GUI, start routing, or touch startup registration. Use `--preferences <absolute-path>` for a controlled document. GUI-connected Configure requires the current `app_preferences` grant, separate from `session_control`.



`launch_at_login` may appear as a persisted setting, but MCP does not read or change the current-user Run key. Only the local GUI changes startup registration after explicit user opt-in. Its quoted startup command is limited to 260 UTF-16 characters. Tests use a disposable injected registry provider under `Software\ProtoInputMultiPlex\Tests\`; never test against the user's real Run key. Bounded virtual lifecycle/preferences acceptance passed with owned targets and isolated startup keys; actual login/logoff/reboot and general-device behavior remain unverified.



GUI-connected Configure is asynchronous: wait for its correlated reply, then Inspect committed preferences. The GUI keeps newer local edits, including invalid shortcut text, and offers Revert to load the latest committed values. Stop or revocation can cancel a request before activation. A revoked client may receive a generic bridge error without a preference acknowledgement; reconnect and Inspect under the current policy instead of treating that error as proof of cancellation. Activation can still commit after the requesting client disconnects. If a reply or activation is uncertain, Inspect the reported preferences and uncertainty, then reconcile with explicit local Apply; do not replay Configure blindly. Inspect may report last acknowledged values with uncertainty; it does not independently verify native registration or disk state. A degraded preference state blocks connected changes and Start until an explicit local Apply reconciles it. Quit waits for accepted preference work and native lifecycle shutdown; failed shutdown keeps the GUI open with its error. The historical GUI-stall cause and full Ultra parity remain unverified.

## Per-user settings and tools



`routing_settings` replaces the complete cursor settings object; omitted fields reset to defaults. Validate the user and the full object before applying. The five source-button order is primary, secondary, middle, back, forward. Optional custom mappings must have unique outputs; legacy swap applies after the custom map. Motion curves (`linear`, `gentle`, `strong`) transform relative packets before axis speed/inversion.



**Motion coalescing** (`motion_coalescing`) defaults to `{"buffer_events":null,"interval_ms":null}`. Non-null event limits accept 2-64 and time windows 1-16 ms; the first limit reached publishes motion. Count-only has a 16 ms oldest-event deadline, and timer-only has a 64-event cap; scheduling can exceed a deadline. This buffers native output after per-packet Rust transforms. Discrete input and routing changes flush earlier motion; failed pending segments are not replayed to later targets. Retain the whole cursor settings object and inspect the running schema before using this field with an older build. Foreground/high-DPI filtering is unavailable for virtual input. See repository `docs/motion-coalescing-contract.md` for bounded virtual acceptance and unverified relative/race/hardware cases. No new tool or grant is added.

Wheel settings are independent by source axis. `horizontal_wheel` handles already-horizontal input (`ignore`, `convert_to_vertical`, or `preserve`) with separate scale/inversion. `horizontal_wheel_emulation` converts vertical input to horizontal (`never`, `always`, or while a raw source button is down). Raw button state triggers conversion even if the button output is disabled or mapped to a key. Conditional conversion is limited to virtual mice on automatic desktop or concurrent assigned routing; unsupported fixed routes retain the setting but show it unavailable.



Cursor appearance and bounds fields include shape, color, size, visibility, user label/badge, label placement/font, badge shape/placement, optional idle hiding, and desktop/window/monitor/region bounds. `routing_displays` is read-only and can run without ProtoInput. Persist monitor interface-path IDs only when `stable_id` is true; never persist `HMONITOR`. Regions use physical desktop pixel coordinates and are clipped to the selected monitor/work area or desktop. Missing/disconnected required monitors and empty intersections fail before mutation. Extended overlay settings require a supported automatic or concurrent route; saved values remain visible when unavailable.



**Windows cursor** (`shape: "system"`) uses Windows cursor artwork with a per-user glow from `color`. In Configure user → Theme, choose Windows cursor; enable Follow application cursor to use supported target cursor state. Without following, it uses the configured Windows arrow. Use the complete `routing_settings` object and inspect `routing_state.users[].overlay.system_cursor_fallback` for conversion/fallback notes. Inverting pixels render solid while preserving glyph geometry; animated cursors use their first frame and theme changes require restart. This adds no tool, grant, or physical-pointer control.

In **Configure user → Theme**, visibility and inactivity settings accompany shape, color and size. **Overlay** holds name/badge settings, label placement and the shared text size. Placement is unavailable when the name label is off; text size remains available for a badge alone. The stopped preview shows the configured Windows arrow/glow or “Cursor hidden” and scales the sample to its compact canvas. The bounded 25-action virtual GUI gate accepts full sampled previews at 320/360 widths and per-user draft isolation; it adds no tool/grant and does not establish live Apply, general appearance combinations, performance or full Ultra parity. See repository `docs/appearance-workflow-assessment-2026-10-20.json`.

**Follow application cursor** (`follow_application`) is a per-user Theme opt-in and defaults off. Inspect the running MCP schema before sending it to an older build. `routing_settings` replaces the whole cursor settings object, so include every cursor value you intend to preserve. Following uses supported cursor state from the explicitly selected target thread; `routing_state.users[].overlay.application_cursor` reports capability, state, PID/HWND, attachment/association epochs, sequence, `provider_status`, kind and an optional image summary; it does not expose a target cursor handle or pixel payload. Capability alone does not mean active following: check the current selection and a positive sequence with provider status 1 (visible) or 2 (hidden). A hidden app cursor hides only that glyph while preserving the user label and badge. The configured Rust shape remains fallback when following is off, unavailable, stale or unsupported. The feature uses target-thread snapshots and virtual coordinates; it never queries host pointer position or moves the physical/RustDesk pointer, and it adds no MCP tool or permission. Arbitrary-app coverage, all cursor APIs, mixed-DPI behavior and same-process user isolation are not established.



`routing_output` separates button/key conversion from the original event stream. Configure replaces `buttons` and `keys`; omitted values reset to Legacy and an empty table. Each mouse button is Legacy, Key, or Disabled. The `keys` JSON object uses canonical decimal source virtual-key strings, for example `"118"`; duplicates and noncanonical strings are rejected. A Key output replaces that source button's mapped mouse output. Non-default output settings need a virtual mouse; Key rules or any nonempty key table (including Disabled entries) also need a paired virtual keyboard. Key output goes to the selected keyboard recipient, so select/focus with a Legacy click first. Raw recording, graph categories, and synthetic Text bypass conversion. Reconfiguration releases only changed conversion-owned source pairs and preserves unrelated owners/programs.

The GUI Output Rules editor retains a separate draft and unfinished key-entry text for each stable user. Apply needs a matching worker acknowledgement; stopped Save to profile updates memory, and Profiles → Save profile writes accepted changes to disk. Discard restores that user's latest accepted baseline. Connected `routing_output` updates preserve an edited GUI draft, so inspect acknowledged settings rather than assuming the visible candidate is live. Profiles keeps transfer/recovery/scope/backend controls in named disclosures, and target-scope edits require stopped routing. Legacy Macros & replication keeps its global Cancel behavior; Macro file and Playback options disclose secondary controls. See repository `docs/feature-workflows-ux-contract.md` for the candidate workflow and its evidence limits.




## Automation and replication



Legacy composite tools are `automation_record`, `automation_clip`, `automation_play`, and `automation_cancel`. Recording observes virtual session input only, never physical/global input. Inspect or load a validated version-1 clip; playback remaps events to current virtual devices. In standalone/local workflows, legacy Cancel is a global stop across all automation programs; connected composite mutations are unavailable. Macro Studio `cancel` stops only the selected user's program. Do not start one workflow's recording/playback while the other is recording/playing.



`macro_studio` provides per-user record/load/edit/generate/reset/play/cancel, a paged timestamped event timeline, JSON clip files, and circle/square/triangle/line/figure-eight/seeded-jiggle generators. Playback accepts speed, loop count or repeat-until-cancel, delay, virtual Escape key-down stop, and reversible keep-awake. Manual input overrides only that same user's macro. It does not observe physical/global input.



When connected to a GUI, `macro_studio` Inspect is read-only and needs no mutation grant. Load, Edit, Generate, Reset, Play, Record Start, Record Stop and per-user Cancel require the current session-control grant and an active permitted virtual user; copy/remap checks both endpoints. Play and Record Start require the selected allowed target. Record Start also requires no held input on that user and an idle lane; holds on other users do not block it. Only the owning client can Stop, including after focus changes. Recording captures only that client's successfully accepted logical virtual actions once, before output conversion; it excludes physical/manual/other-client input and replicated copies, and does not prove application consumption. Batch preflight failure records nothing, but earlier successful members may remain delivered and recorded if a later member fails; partial text effects cannot be rolled back. Capacity preflight rejects the overflowing action before emission. Empty/invalid takes preserve the prior clip/revision. Inspect after an error before retrying. Disconnect/EOF and that client's connected `ReleaseAll` discard only its working take; local `ReleaseAll` remains session-wide. Policy revocation or global Stop/Pause/Mode discards active takes; local GUI Discard cancels only the candidate and leaves direct agent input untouched. Busy lanes and interrupting settings changes are rejected. Clips are session-local; Load remaps actions to the selected lane. The connected recording assessment passed a bounded virtual gate: six native groups, 14 named outcomes, 28 GUI receipts, three owned frames, two x64 targets, no i686 execution. See repository `docs/connected-macro-recording-contract.md` for scope and limits; full UX/Ultra parity remains incomplete.



`routing_replication` remains a numeric-index replacement tool; invalid endpoints, keyboard targets, categories, duplicate edges, or cycles leave the old graph intact. Prefer `routing_graph` for stable seat IDs, editing, undo/redo, JSON persistence, and revision-checked Apply. Edge options filter movement, wheel axes, and five buttons; control anchored/source-relative placement and click delay/paced steps. Step interval is timing, not gain. Queue limits are 4096 event/motion jobs, estimated 8 MiB, and 4096 cadence reservations; native acceptance exercised the event-count cap, while byte/cadence limits have source/unit coverage. Cancel removes queued copies and replica-owned holds while preserving ordinary/source owners. Overflow rejects the triggering action and suspends copies until a valid Apply; ordinary source input continues and release cleanup is preserved. Compound mouse packets are unsupported.



Connected `routing_graph` Inspect is read-only. Apply/Cancel require session-control permission for every active virtual user and every PID in the nonempty automatic desktop allowlist, including current selected recipients. Supply `expected_revision` from Inspect on connected mutations. Apply replaces the session graph and releases held session input after graph and busy checks; Cancel clears queued replica copies/holds but does not stop programs or change the graph/revision. Unsuspended replication continues for future source actions; Cancel does not resume an overflow-suspended queue, which needs a valid Apply. The graph survives client disconnect/revocation, and an acknowledged external graph state updates the GUI applied graph without consuming an unsent draft. Connected graph behavior passed a bounded virtual gate (five groups, 33 phase observations, three original frames); the failed navigation prefix remains unchanged and a focused fresh Apply-to-Save continuation passed. See repository `docs/connected-graph-contract.md` for scope and limits.



`virtual_users` creates and removes ephemeral virtual replicas only in an active automatic desktop session and only for explicitly selected permitted PIDs. Clone slots are stable and append-only; stop prunes incident edges and replica ownership without removing original users. `annotation` manages session-local ink and exports `ink_only`, `selected_target`, `full_desktop` or `desktop_region` PNGs under the distinct grants above. The October 21 acceptance is limited to its owned virtual fixture; see repository `docs/annotation-desktop-assessment-2026-10-21.json`. `magnifier` captures selected targets or explicitly authorized desktop regions; MCP desktop capture requires an explicit `desktop_region`, because an input PID allowlist is not capture authorization. On the focused Magnifier page, Alt+B toggles the selected user's staged border setting and Alt+U toggles staged automatic refresh; explicit Apply activates settings in a running session, while stopped settings use Save-to-profile. The October 21 bounded acceptance covered the local mode menu and these staged controls, not magnified engine pixels. These local keys add no MCP methods and grant no capture permission. Magnifier desktop capture through a GUI connection remains unavailable. See repository `docs/magnifier-controls-contract.md` for scope and limits. `profile_document` validates, normalizes, or exports profile JSON (16 MiB UTF-8 limit) without file access, profile mutation, or routing.



### Typed Input Mapper (bounded foundation)

The [bounded foundation assessment](../../../docs/input-mapper-assessment-2026-10-29.json) accepts a qualified private x64 cohort with two separate owned virtual targets; broader Mapper/Ultra parity and the current Rust port remain outside acceptance. Check the running build's schema. Use `routing_graph` with these distinct `operation` values: `mapper_catalog`, `mapper_templates`, `instantiate_mapper_template`, `validate_mapper`, `inspect_mapper`, `apply_mapper`, `arm_mapper`, and `disarm_mapper`. Catalogue, template construction and validation run locally without a live session. They leave live authority and state untouched.

Inspect `automation.mapper` with `inspect_mapper`, then build a version-1 definition from the catalogue's prototypes and named typed ports, using stable node/user IDs. `validate_mapper` takes `mapper`, the complete definition object (`version`, `nodes`, `edges`). `apply_mapper` takes that same `mapper` and required `expected_mapper_revision` from the inspected mapper `revision`. Apply commits disarmed; inspect its acknowledgement and new revision before a separate `arm_mapper` request with `expected_mapper_revision`. Arm is off by default and never restored by Start, profile load or navigation. GUI authoring is at **Settings → Automation → Input Mapper**.

Connected Apply/Arm/Disarm are whole-graph mutations: they require the existing session-change grant, every active virtual user's grant, and the complete nonempty automatic PID scope, including current recipients. Pure validation does not replace those checks. Only admitted manual and authorized-agent virtual input is supported; physical input, legacy playback composition and simultaneous replication/forwarding are unavailable. The manual GUI session is one evaluation actor; each client has separate actor state. Pointer `position` is that actor's unscaled input trajectory: seeded once, integrating its own raw deltas, reset by absolute MoveTo or retirement. It is not the shared rendered cursor after other actors or speed/clamp settings.

Call `mapper_templates` for editable Route and Monitor starters. Use
`instantiate_mapper_template` with `mapper_template` and a stable
`mapper_source_user` ID. Include `mapper_target_user` only when
`requires_target` is true. `smooth_pointer` routes smoothed Position to a
destination. `speed_monitor` and `pointer_trail` observe the source
without consuming its input or producing output. The Ouroboros Templates tab
loads these same flows into the draft with Undo. Customize the returned
`mapper` before separate Apply and Arm.
[Motion starter assessment](../../../docs/mapper-motion-templates-assessment-2026-11-06.json)

To reuse drafts across profiles, open **Saved flows** in the Mapper or launch
standalone MCP with `--data-dir <directory> --allow-mapper-library`.
`mapper_library` supports `list`, `load` with `flow_name`,
and `save` with `flow_name` and the validated `mapper` definition.
Save replaces an exact name. Load returns a versioned `flow` document.
The GUI can import or export that JSON at an explicitly chosen path. Review
stable user IDs before any separate Apply and Arm.
[Flow library assessment](../../../docs/mapper-flow-library-assessment-2026-11-06.json)

GUI Save and Export use asynchronous storage. Verify a saved name with
`mapper_library` Load when using the opted-in standalone interface.
Detached GUI storage retains the installation and data maintenance leases
until its work ends.
[Storage lifecycle assessment](../../../docs/mapper-flow-lifecycle-assessment-2026-11-06.json)

Use `routing_graph` operation `edit_mapper_group` to organize a
`mapper` draft. Pass `mapper_group_edit` with one nested operation.
`create` takes `group` containing `id`, `label`,
`nodes` and `collapsed`. `rename` takes `group_id`
and `label`. `set_collapsed` takes `group_id` and
`collapsed`. `remove` takes `group_id` and preserves its
members. The result validates metadata and reports overall `valid` and
`validation_error` separately. Continue wiring an incomplete draft
before Apply. Groups persist with the flow, and collapse preserves routing.
[Named group assessment](../../../docs/mapper-node-groups-assessment-2026-11-06.json)

For visual discovery, open **Browse flows** in Templates or Saved flows.
With opted-in standalone MCP, call `mapper_library` with
`operation: "browse"`, optional `query`, `offset` and
`limit`. Limit is one to twelve and defaults to twelve. Results include
structural previews and per-entry errors. Search saved names without case
sensitivity, choose an available entry, then Load its complete document for
editing. Preview fields omit parameters, user bindings and live samples.
GUI Choose only selects the candidate. Load, Apply and Arm are separate.
[Flow Browser assessment](../../../docs/mapper-flow-browser-assessment-2026-11-06.json)

A `monitor` node has one typed input and produces no output.
`routing_graph` with `operation: "inspect_mapper_monitors"` reads the latest
sample values and counts for your current actor. Empty samples mean there
is no committed sample for that actor in the current run. The GUI Activity
view reads its own local virtual input. Ordinary Inspect, profiles and script
JSON omit monitor values.
[Monitor assessment](../../../docs/mapper-monitor-assessment-2026-11-06.json)

A `scope` node accepts one typed input. Apply the graph and explicitly Arm.
The same private inspection returns `traces` for your current actor. Each
trace has a node ID and up to 64 committed `samples` containing
`elapsed_us` since Arm and a typed `value`. Read `revision` and `run` with
the trace. Activity plots manual virtual input. Disarm clears the retained
history. [Scope assessment](../../../docs/mapper-scope-assessment-2026-11-06.json)

Add `led` for Hold, `gauge` for scalar State or Impulse, `xy_pad` for
Position or Delta, or `path_tracer` for Position. Wire the exact typed
`value` input. In `inspect_mapper_monitors`, read `samples` for LED,
Gauge and XY Pad and `traces` for Path Tracer. Use the current `revision`
and `run`. Display bounds are configured on the node.
[Visualizer assessment](../../../docs/mapper-visualizers-assessment-2026-11-06.json)

Use `remap` for scalar endpoints, `smooth` for Position or scalar State,
and `velocity` for Position to scalar desktop pixels per second. Read exact
ports and prototypes with `mapper_catalog` and validate the edited graph
before Apply. Scope on the input Position provides timestamps for checking
Velocity. [Signal conditioning assessment](../../../docs/mapper-signal-conditioning-assessment-2026-11-06.json)

Use `counter` for actor-local pulse counts. Set `with_reset: true` only when
wiring a `reset` pulse. Use `switch` to select matching `off` and `on` branches
with a hold on `select`. Seed both position or scalar-state inputs with a
constant or initialized converter. A selector change alone cannot replay an
earlier transient. Check the live catalogue for supported nodes and types.

Use `timer` with a live hold on `enabled` and a pulse destination on
`value`. Set `interval_ms` (50–60,000) and `count` (1–1,000). The first
pulse is delayed. Lower the enable to cancel, or cycle it low before
starting again after exhaustion. Inspect `pending_timers` and `timer_pulses`
with `inspect_mapper`. A timed delivery fault retires Mapper-owned work
and leaves any saved releases available for retry.
[Timer assessment](../../../docs/mapper-timer-assessment-2026-11-06.json)

After explicit Arm, send only authorized bounded virtual input and verify both acknowledged mapper state and the intended application effect. Finish with `disarm_mapper` using the latest `expected_mapper_revision`; inspect `armed` and `cleanup_pending` to confirm retirement. Disarm releases mapper claims, preserving unrelated owners. On rejection, timeout or uncertain delivery, inspect revision, errors and retained cleanup before acting; never blindly replay input or a mutation. See the [Input Mapper contract](../../../docs/input-mapper-contract.md) for copy/consume, signal semantics, limits and the reviewed bounded acceptance scope.

### Mapper scalar and vector math

The catalogue includes ten scalar functions for State and Impulse, vector
magnitude (Position → State, Delta → Impulse), and vector normalization
(Position or Delta preserved; zero stays zero). Trigonometric inputs are radians
and `sign(±0)` is zero. Inputs and results must be finite and within ±1e9;
negative square root and out-of-range results reject the actor wave atomically
before delivery. See the [Mapper math assessment](../../../docs/mapper-math-assessment-2026-11-06.json).

### Mapper catalogue discovery

Use `routing_graph` with `operation: "mapper_catalog"` to browse the same
catalogue used by the Ouroboros palette. Optional `query` is trimmed,
case-insensitive, limited to 128 UTF-8 bytes and rejects control characters.
Optional `category` uses one of the snake_case IDs `sources`, `constants`,
`vector`, `scalar`, `binary`, `routing`, `conversion`, `observers` or `outputs`.
The response includes each category's ID, human label and description, plus
complete typed prototypes and their ports. Search and category filtering are
pure local discovery; they do not require a live session or change a graph.
The nine categories describe implemented nodes. Unsupported LLM and hardware
controls remain unavailable; local OCR is described below. See the [Mapper palette assessment](../../../docs/mapper-palette-assessment-2026-11-06.json).

### Mapper Delay and Throttle

Use Delay for a typed value scheduled 1–60,000 ms after its input wave is
accepted, or Throttle for a leading value per 1–60,000 ms interval. Both support
Position, Delta, State, Impulse, Hold and Pulse. Delay queues Hold(false) in
order and omits Pulse(false); Throttle passes Hold(false) immediately while
preserving its active window, and Pulse(false) consumes no quota. Dropped
throttle values are never emitted later. Limits are 16 Delay nodes, 128 pending
values per actor and 256 per session. `inspect_mapper` reports ordinary pending
and processed counts; committed samples remain private in
`inspect_mapper_monitors`. Disarm or inspect after uncertain results. If a
rejected input arrives while Delay work or owned Delay-graph claims exist, the
run retires and performs saved-claim cleanup; do not replay it. Due work
rechecks authority and recipient lifetime and faults retire the run. Apply and
Arm do not fire scheduled work. See the [Mapper timing assessment](../../../docs/mapper-timing-assessment-2026-11-06.json).

### Selected-window Screen Tools

For GUI use, open **Screen tools** while automatic routing is active, choose an
active virtual user with an acknowledged selected allowed target, then capture
the snapshot. Inspect a client-relative pixel or crop a template up to 128 ×
128 from that in-memory snapshot and search the whole image or a client-relative
region. Changing session, user, target or target revision clears the snapshot
and template. This uses the GUI's existing capture authority, not Agent Access.

For MCP, call `routing_capture` with `user`, optional `analysis`, and optional
`return_image` (defaults true). `analysis` supports
`{"type":"pixel","x":0,"y":0}`,
`{"type":"image_match","template_png_base64":"…","region":null,"min_score":0.95}`,
or `{"type":"ocr","region":null}` for local Latin text extraction (early preview).
Templates must be PNG, at most 256 KiB encoded and 128 × 128 pixels decoded.
Regions are checked against the captured client image; the default similarity
threshold is 0.95. Search is exhaustive RGB mean-absolute similarity, ignores
alpha, keeps the first row-major tie and rejects work above 64,000,000
candidate-pixel operations. No match returns null; a budget error is explicit.
`return_image: false` omits the PNG but retains metadata and optional analysis.
Connected MCP requires and rechecks the current Agent Access capture grant,
client generation, allowed PID and exact selected target after analysis; input
or PID scope alone does not grant pixels. Standalone calls require explicit
PID scope and recheck the selected target. The local GUI uses existing capture
authority and its UI epoch/target binding, without an Agent Access checkbox.
Every path uses the existing five-second worker deadline. This is snapshot-only observation: it never sends input,
writes files or calls an external vision/LLM service. Do not treat a pixel or
template match as proof that an application control is actionable. See the
[Screen Tools assessment](../../../docs/screen-analysis-assessment-2026-11-07.json).

OCR uses a fresh authorized snapshot. In Screen tools, choose **Read text**;
**Text extraction options** accepts an optional client-relative region. Leave
all four fields blank for the whole client. **Check OCR models** retries a
background availability check. Missing or invalid bundled models disable the
local action and report a reason; there is no download or external provider.
The response contains text and clipped line/word boxes in client and desktop
coordinates. Limit the region to 1,048,576 pixels and 4096 pixels per side;
results are bounded to 4096 UTF-8 bytes, 64 lines and 256 words. Narrow very
wide lines with a region. Excessive work or output is rejected, not silently
truncated. OCR may misread text; do not infer a confidence score or actionable
control from recognition. The [bounded OCR assessment](../../../docs/ocr-text-assessment-2026-11-07.json)
covers two owned ordinary targets; the earlier Screen Tools assessment
retains its recorded pixel/image-match scope.

### Mapper screen observations

Use `routing_graph`'s existing `validate_mapper`, `apply_mapper`, `arm_mapper`,
`inspect_mapper` and `disarm_mapper` operations. `PixelColor` and `ImageMatch`
are triggered Observer nodes with an explicit stable `capture_user`; each needs
a live Pulse and virtual-source ancestry. Pixel Color returns RGBA State
channels and done Pulse. Image Match returns matched Hold, accepted score,
desktop-center Position and done Pulse. On no match, matched is false and score
zero; Position remains the last successful result, so gate its use with
matched. Capture failures are not no-match results.

ImageMatch PNG bytes are deliberately copied into shared persisted mapper
configuration and travel with the definition. Keep only templates needed by
the graph; live frames/results remain private and are never included in
inspection. Limits: 256 KiB encoded, 128 × 128 decoded per template, 1 MiB per
graph, eight screen nodes, one outstanding read per actor/node, eight queued
per actor and 32 per session. `validate_mapper` checks PNG bytes locally;
Apply validates/caches them but does not capture. Apply/Arm never initiate
autonomous reads; only an admitted input trigger dispatches one.

Connected MCP requires the separate current capture grant plus explicit
capture-user/PID read scope. Local GUI uses its existing active virtual user and
selected allowed PID, without Agent Access. All paths recheck target/window,
source ancestry, permission and recipient scope at admission, dispatch and
completion. A repeated trigger or full session queue drops only the new read;
the otherwise valid input effects still commit and `dropped_observations`
increments. Oldest retained work dispatches first; queued jobs do not expire by
age, and the five-second deadline starts at dispatch. Observe pending/completed/
dropped counts and `capture_worker_busy`; these contain no image data. Canceled
workers must drain before Apply or Arm becomes available. On a capture or
delivery fault, inspect acknowledged state and cleanup; do not retry an
uncertain trigger blindly. See the [Mapper screen assessment](../../../docs/mapper-screen-assessment-2026-11-07.json).

### Native application controls

For automatic desktop routing with a virtual mouse and paired virtual keyboard, select `application_focus: "native_controls"` before Start when native application control focus/capture is needed. Compatibility remains the default. This is separate from `focus: "focused"` recipient lookup and `enabled` source enablement. Standalone `routing_start.initial_keyboard` accepts one complete keyboard object per user in `users` order; omission preserves defaults. Configure GUI-owned sessions while stopped. For live `routing_settings`, replace the full acknowledged object and preserve `application_focus`; policy changes are rejected before held-input release, even paused or unattached. Stop and configure a new session to change it. Existing grants and 23 tools are unchanged. Target mouse capture is distinct from screenshot capture permission. Repository `docs/native-controls-contract.md` and its October 19 assessment accept a composed two-process x64 virtual focus/Tab/capture/GUI+MCP Stop workflow. A bounded October 21 native target sample also passed for two owned i686 ordinary EDIT controls under one x64 standalone controller; this is target compatibility only and does not execute an i686 worker. See repository `docs/native-controls-contract.md` for its evidence and limits. The intermittent GUI stall remains OPEN/P1; broader applications, hardware, full UX/Ultra parity and overall feature completion remain unverified.


The current development Native controls runtime requires a matching app, loader/backend and architecture-specific Rust observer provider from one accepted build. Missing provider capability rejects input; do not mix runtime generations or replay uncertain input. The bounded October 20 assessment at `docs/native-keyboard-assessment-2026-10-20.json` accepts a real Ctrl+S accelerator and two disposable classic WordPads with independent typing/focus, genuine Ctrl+S saves, exact file readback and normal Stop/closure. Use named key DOWN/UP for shortcuts, `text` for literal text, and inspect application effects. The later `docs/keyboard-theme-assessment-2026-10-20.json` accepts one English-US Shift+A case with GetKeyboardState agreement and unchanged state across non-removing peeks until consumption. Other keyboard patterns, arbitrary apps, hardware and general isolation remain unverified. The stopped Theme preview also shows configured Windows arrow artwork with user-colored glow without backend DLLs. The later `docs/appearance-workflow-assessment-2026-10-20.json` accepts sampled complete Theme/Overlay preview canvases at 320×360 and 360×360; arbitrary appearance combinations and full UX parity remain open.

### OCR text to Mapper action (early preview)

Discover the current catalogue's `ocr`, `text_contains` and `text_compare`
prototypes. OCR takes an explicit stable `capture_user`, optional client
`region`, and live `trigger` Pulse. It emits persistent `text` (Text),
`has_text` (Hold), and `done` (Pulse). For an action, connect Text to a literal
predicate, its Hold to a Pulse Gate's `open`, OCR `done` to the Gate's `value`,
and the gated Pulse to an existing KeyPulse sink. Apply commits disarmed;
Arm remains a separate revision-checked action. Validation, Apply and Arm do
not capture or infer. Local GUI availability checks run separately in the
background; static MCP validation is not proof that models are available.

`text_contains` requires a nonempty `needle`; `text_compare` takes `expected`
and `operation` (`equals` or `not_equals`). Each configured literal is at most
256 UTF-8 bytes and is shared in saved graphs/profiles/flows. `case_sensitive:
false` folds ASCII A–Z only, not general Unicode case. There is no regex,
numeric coercion. Empty OCR emits an empty string and
`has_text=false`; Equals/NotEquals still compare that empty value normally.
Combine `has_text` with the predicate using Hold logic when actual recognized
text is required. Scope and numeric visualizers do not accept Text; latest
Monitor does, privately through `inspect_mapper_monitors` for the requesting
actor. Ordinary state and structured JSON Script state contain no recognized
text. An explicitly requested GUI screenshot PNG may contain visible recognized
text.

The catalogue also exposes `type_text`: a Sink with an explicit stable output
`user` and `channel: {"type":"type_text"}`. Connect its required `text` Text
input and `trigger` Pulse input; for OCR, use OCR `text` and the gated `done`
Pulse. A true trigger types the current actor-private cached text once. A later
explicit trigger intentionally types that cached text again; updating Text,
Apply and Arm never type. Empty text is a no-op. Output allows LF/tab but rejects
CR and other control characters. The whole wave shares a 128-effect budget,
counting one per UTF-16 unit and two per key/button pulse. Over-budget text is
rejected whole before any wave input/output, with no truncation. The output
user needs its own permitted virtual keyboard and selected recipient; OCR's
capture grant is separate. Text remains in one retained recipient scope. A
failure can leave a typed prefix that cannot be undone: inspect the target and
cleanup state before deciding whether a new trigger is appropriate. Never
blindly replay an uncertain trigger. See the [Type Text contract](../../../docs/mapper-type-text-contract.md).

OCR shares the existing observation admission, separate capture permission,
exact-target checks, one physical Mapper worker, DROP-NEW limits and cleanup.
The original trigger origin is retained. A permission, target, inference or
delivery failure retires affected Mapper work and preserves saved-release
cleanup. Do not replay uncertain triggers. Model weights are a separate
six-file offline CC-BY-SA-4.0 bundle; preserve its notices when packaging.
See the [OCR text contract](../../../docs/ocr-text-contract.md). The bounded assessment covers two owned normal-pump Windows targets;
broader application, physical-device and isolation acceptance remain open.

## Profiles and validation



The stopped Profiles page can Load/Save the active profile, Import/Export a transfer path, Restore the `.bak`, Reset, or Undo. The six file paths run on one Rust worker and capture the clicked path and profile. Wait for completion feedback; file actions and Start are unavailable while a job is pending. Navigation or further input can prevent a pending Load/Import/Restore from replacing newer setup or hidden drafts. Save/Export still write their captured snapshot, and a stale Restore may have changed disk even when memory was kept. An initial-load error requires explicit Load or Reset before Start or other writes. Normal Quit waits for accepted writes; failure keeps the GUI open with feedback. Reset/Undo are in-memory only and cannot cancel a dispatched write; Undo does not reverse disk writes. Saves validate before replacement and keep prior valid bytes in the backup. If the initial profile is corrupt, Load a valid backup (or Reset before importing), then export it to a new path. Files are atomic individually; concurrent writers, forced crashes, and full-product backup are outside the verified contract.



For repeatable checks, use `scripts/test-desktop.ps1` and `scripts/test-mcp.py`; they create disposable targets and use virtual input. The regular runner checks discovery/schema for the current 25 tools and standalone behavior, not GUI connection/capture. Do not treat discovery, source tests, or historical feature assessments as full Ultra parity. Repository `docs/validation.md` is the concise index of producer checks, independent assessments, retained failed aggregates, and scope limits. Hardware transport, arbitrary application compatibility, full desktop isolation, physical-device behavior, all-Unicode and broader font coverage, custom Windows ballistics, gamepad output, and full Ultra parity remain unverified.

For concurrent assigned GUI routes, inspect `routing_state.users[].pending_releases`: a nonzero count means saved native UPs remain pending; null means the count is unavailable in that mode. Retries use the original recipient with a 250 ms minimum interval and never issue new DOWNs or expand scope. Typed release rejections keep the session/client live and leave acknowledged settings unchanged; inspect state before retrying. Home's pending-count button opens **Input release details**. Its **Release all input** is explicitly global, while connected `routing_input` `release_all` remains scoped to this client. Failed Stop still requires all affected original assigned processes to exit before an explicit fresh-target Load/Start. See repository `docs/assigned-release-recovery-contract.md` for virtual-only acceptance and remaining limits.

For virtual keyboard delivery, copy the complete acknowledged settings object from `routing_state.user_settings` before changing `routing_settings`; edit its nested `keyboard: {enabled, focus, application_focus}` field while retaining the current application-focus policy. Focus accepts `clicked`, `focused`, `native`, `active`, or `caret`. Nondefault keyboard options require a virtual mouse and paired virtual keyboard with automatic or concurrent assigned routing. Inspect `routing_state.users[user].keyboard` (`settings`, `clicked`, `resolved`, `fallback`, `status`); the resolved recipient is a read-only candidate, never delivery proof. Disabling excludes raw keys/text before recording, Escape handling and replication; mouse-to-key output remains active and saved UP cleanup remains available. Settings changes cancel queued keyboard paths and require held releases before commit; inspect a failed acknowledgement before retrying. GUI-added and MCP Start virtual pairs in supported automatic or concurrent assigned routing initialize Focused; legacy profiles and new fixed-relative GUI pairs keep Clicked, and generated clones inherit source settings. The GUI preserves edited drafts when external keyboard settings arrive. See repository `docs/keyboard-delivery-contract.md` and its bounded assessment: owned x64 custom-target evidence does not establish arbitrary-app focus/Tab behavior, physical/native fixed-relative support or full Ultra/UX parity. Minimum modes require ordinary scrolling; no simultaneous checkbox-plus-mode pixel claim.

### Input forwarding (bounded virtual acceptance)

Forwarding uses stable IDs and one outgoing edge per source for active virtual users in automatic desktop routing. Pointer actions traverse chains once to a terminal user; converging sources are supported. Optional keyboard follows enabled hops and stays local at the first disabled hop. Use the Ouroboros editor's staged draft and explicit Apply; a live change is applied only after the matching worker revision/edge acknowledgement. Stopped changes require the existing Save profile action. Forwarding and replication cannot both be active. Source-overlay hiding preserves saved visibility; GUI Clear restores configured visibility.

`routing_graph` exposes `inspect_forwarding`, `apply_forwarding`, and `clear_forwarding` without increasing the existing 23-tool count. Connected graph mutation requires the expected forwarding revision, session-control permission for every active user, and the full explicit nonempty automatic PID allowlist including selected recipients. Per-input permission independently checks source, every traversed hop, actual sink, and PID. Saved UP cleanup retains the original recipient; failed release can remain pending.

The October 21 bounded virtual-only GUI/MCP assessment passed for one owned application workflow with three ordinary EDIT probes and two normal worker cycles. Producer checks passed formatting, strict Clippy, 452 workspace tests (0 failed, 3 ignored), and x64/i686 app/observer builds. This is not evidence for physical devices, arbitrary applications, desktop-wide or same-process isolation, RustDesk, full Ultra parity, or a GUI-stall fix; historical GUI-stall P1 remains OPEN. See `docs/input-forwarding-contract.md` and `docs/input-forwarding-assessment-2026-10-21.json` for the bounded scope.
## GUI recovery inspection continuation

Recovery uses background native samples. After live connection admission or a policy change, `routing_state.recovery` may temporarily be null or lack rows; this is not evidence of clean Stop or permission to restart. Within your existing user/PID grants, an affected original must be observed while live before relying on that same client's retained terminal row. Missing rows can also reflect partial scope; keep the outcome unconfirmed and use GUI recovery details rather than expanding grants or replaying input. Sample age comes from the actual native poll, not the latest UI read. The bounded October 28 workflow passed 585 Rust tests and native A Running→Exited, C-only fresh-session delivery, visible Stopping and normal Quit; see repository `docs/recovery-ui-native-poll-contract.md` and `docs/recovery-ui-native-poll-assessment-2026-10-28.json`. Historical GUI-stall P1 remains open.

When a GUI worker stops while an already authenticated GUI-connected client remains open, that same client may become inspection-only. The retained GUI bridge accepts existing `Inspect` and `Disconnect` only; worker commands and new GUI connections are denied. Use `routing_state` and `agent_session` to inspect: check `routing_state.running` (false), `agent_session.connection.inspection_only` (true), and `routing_state.recovery` for its safe phase, `can_start`, and authorized original target rows. A partial scope may return phase/action without rows. Do not infer authorization from numeric PID reuse, request raw errors, or treat this view as a restart command. Use `routing_stop` to disconnect the MCP client. This worker restriction does not make unrelated local MCP tools unavailable.

`routing_stop` disconnects the local MCP bridge client. The stdio MCP server process may remain alive, so close its transport through the client's normal shutdown path as a separate step. Recovery inspection itself never restarts routing. A user-initiated GUI Start with a newly selected target first retires the old broker, then requires fresh target validation, descriptor, handshake, and GUI policy acknowledgement. Never reuse the old connection authorization or numeric PID as identity. Standalone recovery behavior is unchanged.

Revision 4 passed source review and all five producer checks (517 tests passed, 0 failed, 3 ignored). The earlier producer 002 result of 516 tests is historical. Independent review accepted one bounded owned virtual GUI recovery workflow: 45/45 controls, three captures, original failure through same-client terminal inspection and exit, explicit fresh Start with reset Agent page and new Enable/descriptor/handshake, replacement virtual delivery, then normal Stop, disconnect and Quit. The unchanged raw evidence retains its PENDING label; the separate acceptance receipt records the decision. The technical-details image shows the expanded header and start of the diagnostic; scrolling is source-supported, but full error visibility and native scroll proof are unverified. Historical GUI-stall P1 remains open; full Ultra parity remains incomplete.


## Cursor renderer selection

The renderer is `auto` by default or explicit `gdi`. Auto attempts hardware GPU drawing and falls back to GDI on initialization, drawing or device-health failure. Use **Configure user → Overlay → Cursor renderer selector**, then **Apply cursor settings** while routing. In MCP, find the authorized user's item in `routing_state.user_settings`, copy its complete `settings`, change only `renderer`, and send existing `routing_settings` with that item's `user`. Profiles store this at `seats[i].cursor.renderer`. No new tool or grant is required. Automatic desktop and concurrent assigned overlays support the preference; fixed switched routing rejects explicit GDI and disables the control. In the accepted cursor-settings build, a clean GUI form follows acknowledged connected MCP settings. A locally edited form keeps its draft and shows **Applied settings changed; draft kept.** The **Use applied settings** action replaces that draft with the latest acknowledged full object. **Apply cursor settings** sends the full draft and waits for a matching settings acknowledgement; later local edits remain staged. Stopped edits can be saved without Apply. See repository `docs/cursor-settings-coherence-assessment-2026-10-21.json` for the bounded virtual workflow and current-build provenance.

Inspect `routing_state.users[].overlay.presentation` for `raster_backend`, `gpu_draws`, `gpu_uploads` and `fallback_reason`. The normal GPU path reports `direct2d`, positive draws and zero completed CPU-canvas uploads. Pixel provenance is separately at `users[].overlay.pixel_telemetry_source`: `unavailable_gpu` means zero `pixel_hash`/`painted_pixels` is unmeasured, not proof of blank output. Use actual readback evidence for pixel correctness. GDI with a failure reason is fallback; explicitly selected GDI has no GPU-failure reason. The bounded engine/live-control/stopped-save acceptance is recorded in repository `docs/gpu-rasterization-assessment-2026-10-21.json`; general P1, performance, compatibility and full Ultra parity remain unverified.
