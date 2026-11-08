# Product target: Ultra functional parity

Independent startup loading and saving on Quit are available in Ouroboros
Application preferences and stopped-Setup MCP. Defaults load the selected startup
profile and leave exit saving off. An unchanged exit save preserves the recovery
backup; failures keep the window open. See the [contract](profile-persistence-contract.md)
and [bounded independent assessment](profile-persistence-assessment-2026-11-07.json).
The historical GUI-stall P1 and full Ultra/UI parity remain open.

Generated clone Stop now keeps the roster revision available for retry if a
release fails. The worker retires the clone once cleanup succeeds, while
preserving other users' independent input. The release fault was simulated.
[Clone Stop assessment](clone-stop-retry-assessment-2026-11-06.json)

The November 6 shared checkout combines the qualified Rust worker with Mapper
command receipts, per-user Pause and GUI activity diagnostics. One x64 run
exercised two virtual users, stopped and live MCP access, a stale Mapper
rejection, Pause and Resume, and normal Stop and Quit. Both app architectures
built, and 745 workspace tests passed. [Integration assessment](rust-port-feature-integration-2026-11-06.json)

The November 6 stopped-GUI observation encountered delayed script files
while the owned window responded and closed normally. It ended before the
Keyboard transition. [GUI stall observation](gui-stall-observation-2026-11-06.json)

The November 6 script writer prioritizes readiness and progress while
continuing to service routine snapshots. A fresh stopped x64 run on the
network share reached the Keyboard transition and closed normally.
[Script priority assessment](gui-script-priority-assessment-2026-11-06.json)

The stopped-GUI script can now disable screenshot readback and track requests,
renderer receipts and PNG publication separately. Both arms reached User B's
Keyboard page with two virtual pairs and closed normally. The historical stall
did not recur and remains open.
[Readback comparison](gui-capture-differential-assessment-2026-11-06.json)

Application-preferences completion now wakes the root viewport. Both
x64 builds closed normally in a controlled minimize-during-Quit run.
The earlier intermittent wait remains open. [Quit wake assessment](minimized-quit-wake-assessment-2026-11-06.json)

Per-user cursor validation now reads cached desktop geometry. Live Apply
leaves current native display validation to the routing worker. The
Keyboard page stayed responsive during a delayed scan, and one virtual
keyboard change passed GUI acknowledgement and MCP readback.
[Display validation assessment](gui-display-apply-assessment-2026-11-06.json)

Home now offers Alt+C for the selected user's configuration, Alt+S for
Settings, and Alt+H for Hide. Double-clicking a user number opens that
user's settings. The shortcuts passed an owned stopped-GUI gate, and
the double-click gesture passed focused egui testing.
[Home navigation assessment](home-access-assessment-2026-11-06.json)

Home now opens an Ouroboros Apps launcher for six built-in tools.
Choose a tool, view Info, and open its existing workspace. Alt+A opens
the launcher. The Apps visibility preference defaults on and can be
changed locally or with the existing MCP preference grant.
[Apps launcher assessment](apps-launcher-assessment-2026-11-06.json)

Input Mapper adds undoable node placement, Inspector duplication and keyboard
draft editing. The stopped Ouroboros workflow verified Duplicate, Undo and
Redo. Focused egui input tests verified drag and keyboard handling.
[Authoring assessment](mapper-authoring-assessment-2026-11-06.json)

Input Mapper now connects typed ports directly on the canvas. Use a drag or
two clicks to connect an output to a compatible input, and select a connected
input to disconnect its wire. Each edit can be undone before Apply.
[Canvas wiring assessment](mapper-canvas-wiring-assessment-2026-11-06.json)

Input Mapper now supports multi-node canvas selection, movement, duplication
and deletion. Each group edit is undoable. Duplication remaps wires within the
selection to the new stable node IDs.
[Canvas group assessment](mapper-canvas-groups-assessment-2026-11-06.json)

Mapper Scope plots recent committed input in the Ouroboros Activity view.
The existing private MCP inspection returns timed traces for the requesting
actor. An owned virtual Hold workflow verified actor separation and Disarm
cleanup. [Scope assessment](mapper-scope-assessment-2026-11-06.json)

Mapper Activity adds LED, Gauge, XY Pad and Path Tracer with actor-private MCP
readings. A virtual Windows run verified all four displays and scoped reads.
[Visualizer assessment](mapper-visualizers-assessment-2026-11-06.json)

Mapper adds scalar Remap, Position or State Smoother, and timed Velocity.
The virtual Windows workflow checked endpoint mapping, filter response and
speed against the original Position trace.
[Signal conditioning assessment](mapper-signal-conditioning-assessment-2026-11-06.json)

The Ouroboros Templates browser now separates Route and Monitor. Smooth pointer,
Speed monitor and Pointer trail add editable motion starters to both the GUI and
MCP. At minimum window size, the virtual-only test loaded the same Pointer trail
draft as standalone MCP and verified Undo.
[Motion starter assessment](mapper-motion-templates-assessment-2026-11-06.json)

The Ouroboros Saved flows view supports named Mapper drafts and JSON transfer.
Opted-in standalone MCP shares the chosen data directory. The virtual Windows
run verified a flow from MCP save through GUI load, export, second save, Undo
and import.
[Flow library assessment](mapper-flow-library-assessment-2026-11-06.json)

Mapper flow storage now retains the installation and data maintenance leases
during detached work. A blocked-save test verified that UI teardown completes
while maintenance stays excluded until the worker ends.
[Storage lifecycle assessment](mapper-flow-lifecycle-assessment-2026-11-06.json)

The Mapper canvas supports named, collapsible groups with group movement,
Undo and portable persistence. MCP exposes the same metadata edits and reports
graph readiness separately for incomplete drafts. The virtual Windows workflow
covered group authoring and the normal and compact collapsed canvas.
[Named group assessment](mapper-node-groups-assessment-2026-11-06.json)

The Flow Browser provides searchable template and saved-flow cards with
structural schematics. Saved browsing uses bounded pages and isolates invalid
entries. The virtual Windows workflow selected a template at compact width,
browsed a second saved page, and verified the separate Load and Undo actions.
[Flow Browser assessment](mapper-flow-browser-assessment-2026-11-06.json)

The October 29 x64 diagnostic assessment covers the minimum-size Ouroboros
disclosure and its readable counters. The historical GUI stall remains open.
[Diagnostic assessment](gui-activity-assessment-2026-10-29.json) · [Contract](gui-activity-contract.md)

Delivery/recovery has a separate Rust installation and configuration maintenance
slice, documented in [Installation and recovery](installation-recovery.md).
Its current-distribution evidence is separate from the historical portable
reports and does not establish broader Ultra parity.

The [October 29 loopback SDK assessment](loopback-sdk-assessment-2026-10-29.json)
accepts an isolated x64 Rust/browser command workflow: 58 UI actions, concurrent
client ownership, silent idle survival, revocation and normal shutdown. The
[SDK guide](loopback-sdk.md) records its scope. A separate
[Access/SDK layout assessment](agent-sdk-layout-assessment-2026-10-29.json) accepts
30 recorded UI actions and 11 captures with scrolling bodies and separate action footers
at 320×360 and 680×620, after 610 passing tests and a fresh isolated x64 build.
It removes the earlier SDK/policy crowding workaround in that bounded cohort;
full compact UX and the separate current Rust port remain outside acceptance.
The [current stopped-GUI diagnostic](gui-stall-current-assessment-2026-10-29.json)
also passed 12 controls and normal close; the historical intermittent stall
remains OPEN/P1. Neither result approves the separate Rust-port work or full
Ultra/UI parity.

The [October 29 reference refresh](parity-reference-refresh-2026-10-29.md)
separately tracks application-facing Windows/Web input SDKs and a general typed
processing graph. Accepted command RPC, replication and forwarding do not
complete those capabilities. The existing roadmap priority order is unchanged.

The November 6 Mapper group adds Counter and typed Switch to the same
Rust graph, Ouroboros editor and MCP commands. The owned native run verified
count thresholds, branch changes, reset and release ownership across two
virtual users. [Counter and Switch assessment](mapper-flow-assessment-2026-11-06.json)

The November 6 Mapper Timer adds a finite series of delayed pulses with
release and Disarm cancellation. One authenticated virtual actor routed
between two owned x64 windows, with ordinary input ownership preserved.
[Timer assessment](mapper-timer-assessment-2026-11-06.json)

The Ouroboros Mapper browser and MCP now share three editable starter flows.
The native run sampled normal and compact browser layouts and exercised
draft load, Undo, Redo and a same-user key remap through Apply and Arm.
[Starter-flow assessment](mapper-templates-assessment-2026-11-06.json)

The typed Mapper Monitor now displays latest local samples and exposes a
private MCP read for the requesting actor. A native virtual-key run verified
one connected actor and the local GUI had separate readings, while a second
connected actor received no sample.
[Monitor assessment](mapper-monitor-assessment-2026-11-06.json)

The [typed Input Mapper assessment](input-mapper-assessment-2026-10-29.json)
accepts a bounded foundation in a qualified private x64 cohort after 657 passing
tests: 26 UI actions and 50 SDK calls with two virtual users in two separate
owned targets, including a numeric parameter edit/Apply, explicit Arm/Disarm and
scoped hold retirement. The five original captures cover 680×620 and 320×360.
Broader native operator/authoring coverage, payload magnitudes and profile reload
remain outside this acceptance; full Mapper/Ultra parity, same-process isolation,
the current Rust port and historical GUI-stall P1 remain unaccepted or open.

Build a standalone multi-user Windows desktop tool comparable to MouseMux Ultra, using ProtoInput as the initial backend and Rust for new features. No artificial seat counts, session timers, paid feature gates, or activation dependencies are planned.

The [ordered roadmap](roadmap.md#current-priority-order--october-14-2026) prioritizes agent use and two-user desktop use: session reliability → agent control/observability → everyday setup/UI/compatibility → cursor/display/accessibility → productivity gaps → GPU/performance → delivery/recovery → remote collaboration → dedicated app/browser integrations → extended-device adapters. App/browser integrations are explicitly second-lowest; pen, touch, touchpad and gamepad adapters are lowest. This table tracks scope and evidence, not priority order.

The combined October 15 source passed [bounded integration acceptance](combined-integration-assessment-2026-10-15.json): Rust checks and four app/observer build pairs, 29 Ouroboros observations, 12 virtual input calls, config-root persistence and clean Home Stop. The [320-pixel original](combined-integration-controls-320.png) accepts the corrected From/To chooser carets and whole replication actions; its clipped footer is excluded. GPU status reporting passed this smoke; GPU pixels/performance, final combined distribution, hardware, full UX and Ultra parity remain separate open gates.

Worker transport liveness passed [bounded acceptance](worker-transport-assessment-2026-10-15.json): asynchronous supervisor pipe writes, independently visible session Stop intent, bounded outbox backpressure, a real nonreading-child teardown regression and an eight-input standalone virtual Stop workflow. Existing Ouroboros/MCP failure and recovery semantics remain in use. Live routed backpressure/GUI fault injection, broader P1, hardware and full Ultra parity remain open; the frozen 329-test release source predates this fix.

References reviewed October 13, 2026: [official edition comparison](https://www.mousemux.com/pricing/) and [public UI/workflow manual](https://www.mousemux.com/pages/manual/). Parity is a target, not a current-build claim. Vendor support, licensing entitlements, and third-party hosted services are not capabilities we inherit.

| Target | Current state | Next implementation |
| --- | --- | --- |
| Unlimited users and keyboards | No profile seat cap; observer works | Load and hardware testing |
| Switched/concurrent interaction; independent typing | Rust click-selected ownership, switched root arbitration, concurrent per-process routing, independent keyboard focus, capture and window drag/resize. The [native keyboard assessment](native-keyboard-assessment-2026-10-20.json) also accepts a Ctrl+S accelerator and two owned classic WordPads with independent typing, persisted saves and normal Stop/close. A separate [Shift/table/peek gate](keyboard-theme-assessment-2026-10-20.json) accepts one English-US native Shift+A case. | Physical-device validation and broader compatibility |
| Cursor themes, labels, GPU rendering | Base and extended appearance retain their existing bounded assessments. Default-off per-user application cursor following passed [bounded independent acceptance](app-cursor-following-assessment-2026-10-15.json): original Rust stock shapes, supported custom BGRA/hotspots, hidden glyph with labels/badges retained, ordinary EDIT following, target/Stop recovery, legacy fallback and the Ouroboros preference/save path. Configured shape is fallback when off, unavailable, stale or unsupported. Windows cursor artwork with per-user glow, small/large IBeam visibility and cache/cleanup behavior also passed [bounded virtual acceptance](cursor-keyboard-assessment-2026-10-20.json); inversion is disclosed as solid glyph geometry. The historical [Windows-arrow preview](keyboard-theme-assessment-2026-10-20.json) accepted glyph/halo only. The later [compact Theme/Overlay gate](appearance-workflow-assessment-2026-10-20.json) accepts relocated controls, per-user draft isolation, hidden state and the full sampled preview canvas at 320/360 widths without ProtoInput DLLs. Direct GPU cursor drawing, per-user Auto/GDI controls, MCP switching and renderer persistence passed [bounded independent acceptance](gpu-rasterization-assessment-2026-10-21.json) with virtual users and owned windows. | Broader cursor APIs/apps, mixed-DPI, performance testing and full visual parity remain open |
| Monitor/window bounds and DPI | Legacy/Desktop/Window/Monitor/work-area/Region controls and stable display identities passed independent virtual acceptance. The [Window Lock workflow](window-lock-workflow.md) has bounded x64 GUI acceptance for transient per-user picks and dynamic selected-client following/clamping. A separate [compact Lock layout assessment](lock-layout-assessment-2026-10-20.json) accepts the five-mode menu, contextual Apply, validation and individually reachable Region rows at 320×360 and 360×360. | Real monitor reconnect/mixed-DPI, simultaneous visibility of the complete Lock form, and broader display validation remain open. |
| Explicit window selection and recipient-targeted input | V10 source qualified; passive worker selection commits before the MCP response; first key DOWN and each text action capture a recipient before lazy attachment. | Bounded x64 native selection acceptance passed; the separate [Window Lock assessment](window-lock-assessment-2026-10-20.json) accepts a bounded GUI setup/live workflow. Two owned i686 ordinary EDIT targets passed bounded virtual native-input acceptance under one x64 standalone controller in the [October 21 assessment](native-i686-targets-assessment-2026-10-21.json); this does not execute an i686 worker. A separate [bounded i686 controller/worker core](live-settings-rejection-assessment-2026-10-21.json) accepts concurrent owned EDIT routing, nonfatal settings rejection, independent capture/release and normal Stop/cleanup. Its aggregate remains NOT_ACCEPTED at a post-unhook ambient-key assertion; post-Stop same-target readiness/reuse was not observed. Arbitrary applications, broader EDIT-focus behavior, GPU PrintWindow, hardware and full Ultra parity remain open. See [contract](explicit-window-selection-contract.md) and [assessment](explicit-window-selection-assessment-2026-10-16.json). |

| Speed, acceleration, remapping, scrolling | Independent X/Y speed/inversion, original per-packet response curves, unique-output mapping with composed legacy swap, side buttons, horizontal input policy, mouse-to-key/keyboard remapping and conditional vertical-to-horizontal conversion passed bounded virtual acceptance. The [Output Rules assessment](output-rules-assessment-2026-10-14.json) covers automatic concurrent routing and ordinary-pump Stop only; [conditional wheel acceptance](conditional-wheel-assessment-2026-10-14.json) covers automatic and concurrent assigned virtual routes to owned probes. All five raw source-button triggers were sampled on x64; representative math, origin and delayed-delivery cases ran on x86. Three earlier aggregate artifacts retain NOT ACCEPTED status, with their passed subgroups and tails recorded separately. The [wheel behavior record](conditional-wheel-plan.md) documents limits. Physical-device validation, arbitrary application scrolling, and Windows pointer ballistics/calibration remain unsupported or unverified |
| Motion coalescing | Default-off per-user count/timer buffering, Ouroboros controls and existing MCP settings passed [bounded virtual acceptance](motion-coalescing-assessment-2026-10-15.json). Native A motion messages fell from 8 to 2 with the endpoint preserved while B remained at 4; separate settled barriers, idle timer and stopped-GUI fresh Save passed. | Foreground/high-DPI filtering unavailable; fixed-relative native, forced races and hardware/performance coverage remain open. See [contract](motion-coalescing-contract.md). |
| Pen, touch, touchpad, gamepad I/O | Planned | Device/output adapters |
| Magnification and annotation | Annotation and magnifier passed bounded virtual native/Ouroboros acceptance. Shared-GUI selected-target magnifier workflows passed with distinct grants and asynchronous export. Explicit full-desktop/region annotation export passed the [owned-fixture assessment](annotation-desktop-assessment-2026-10-21.json). Draw access keys passed bounded app-local virtual acceptance in the [October 21 assessment](draw-access-keys-assessment-2026-10-21.json); see the [contract](draw-access-keys-contract.md). | Focused Magnifier Alt+B/Alt+U draft controls and the normal/compact mode-menu workflow passed bounded app-local native acceptance in the [October 21 assessment](magnifier-controls-assessment-2026-10-21.json); see the [contract](magnifier-controls-contract.md). Broader keyboard coverage, compact-form polish beyond the observed frames, engine-pixel validation, application/device compatibility, retained capacity under revocation and provider-fault behavior remain open. Arbitrary desktop/display breadth, hardware, same-process isolation, P1 responsiveness and full Ultra parity are unverified. |
| Macro recording/playback | **Bounded virtual acceptance passed.** Shared-GUI authoring and scoped playback additionally passed [connected acceptance](connected-macro-studio-assessment-2026-10-14.json); connected owner-only recording additionally passed [bounded acceptance](connected-macro-recording-assessment-2026-10-15.json), including transactional commit, input exclusion, GUI Discard and ownership-preserving cleanup. This records accepted virtual commands only. The legacy composite workflow records, saves/loads JSON, and plays synchronized input. Macro Studio adds per-user clocks, typed event insertion/replacement, timeline/range edits, copy-to-user, reset, geometric patterns and jiggle, delay/speed/repeat, incoming virtual Escape key-down stop, scoped cancellation, and reversible keep-awake requests. | Expand hardware/application validation; full Ultra parity remains incomplete |
| Replication and chains | **Partial; advanced options and live generated-user lifecycle passed bounded virtual acceptance.** Connected virtual-user Start/Stop additionally passed [bounded acceptance](connected-virtual-users-assessment-2026-10-15.json), with ungranted creation, explicit user grant/reconnect, creator-only abort and GUI-owned EOF/tombstone behavior. Connected stable-ID graph Apply/Cancel additionally passed [bounded acceptance](connected-graph-assessment-2026-10-15.json) for whole-session scope, current revisions, ownership, GUI draft/profile synchronization and actual Save. The stable-ID visual editor and advanced filters, source-relative offsets, click delay, paced steps, queue telemetry/cancellation and recovery passed the [advanced assessment](advanced-replication-assessment-2026-10-14.json) using virtual seats and disposable owned cooperative targets. That gate recorded 23 native group occurrences and 49 actual UI control events across two phases; earlier NOT ACCEPTED prefix aggregates retain their original status. The native gate exercised the 4096 event-count cap, source-down rejection, ordinary uncopied input during suspension and release cleanup. The 8 MiB byte accounting and separate 4096 cadence bound have source/unit evidence only. Legacy edges retain anchored, immediate, all-filter defaults; missing endpoints block Apply/Start, and node layout remains session-local. The [live `virtual_users` assessment](virtual-users-assessment-2026-10-14.json) passed five native groups and 35 GUI control occurrences from an accepted 26-control prefix plus nine-control cleanup tail; the prefix aggregate remains NOT ACCEPTED. The feature uses append-only session slots, stable IDs, explicit selection within existing PID scope, graph options and per-replica cleanup, and is not written to profiles. The historical clone source passed 87 tests and x64/x86 builds; see [its producer verification](virtual-users-rust-checks-2026-10-14.json). The historical 20-tool GUI-agent source passed 95 tests, formatting, strict Clippy and x64/i686 builds; bounded GUI-connection acceptance passed, with producer details in [its check record](gui-agents-rust-checks-2026-10-14.json). Compound packets, hardware, assigned-process cloning and broad application compatibility remain outside scope. | Validate physical-device and broader application behavior separately |
| Remote desktop/screen sharing | Planned | RustDesk and transport/compositor |
| Multi-user apps/browser workflows | Planned | Independent integrations |
| Profiles, tray/startup, hotkeys | Independent startup load and save-on-Quit policies passed the bounded [persistence assessment](profile-persistence-assessment-2026-11-07.json). Profile maintenance passed its bounded [assessment](profile-maintenance-assessment-2026-10-14.json). Separate application preferences, tray behavior, seven typed shortcuts, explicit local startup opt-in and cleanup/recovery passed bounded independent virtual acceptance in the [application lifecycle assessment](app-lifecycle-assessment-2026-10-14.json); the [producer record](hidden-lifecycle-rust-checks-2026-10-14.json) records formatting, strict Clippy, 135 tests and x64/i686 builds. Eight earlier aggregate artifacts remain NOT ACCEPTED; the accepted assessment uses passed subgroups and later focused tails without relabeling them. MCP can inspect the persisted login preference but cannot alter startup registration. | Broader profiles, migrations, backups, accessibility and real logon/reboot validation |
| External automation interfaces | Current Rust MCP source exposes 23 tools, including `routing_output` and `app_preferences`. Output Rules passed bounded virtual acceptance in the [independent assessment](output-rules-assessment-2026-10-14.json). Application preferences and lifecycle passed bounded independent virtual acceptance in the [lifecycle assessment](app-lifecycle-assessment-2026-10-14.json); app-preference writes use a separate explicit grant, and startup registration remains local-only. Standalone agents keep a separate supervised session. Optional GUI connection uses the dashboard session for virtual input, current state, read-only inspection and selected-target capture when explicitly enabled. GUI access and capture passed bounded virtual acceptance in the [independent assessment](gui-agents-assessment-2026-10-14.json), limited to one x64 GUI controller and cooperative targets. Capture and session-control permissions default off. See the [scope contract](gui-agent-plan.md). Earlier Macro Studio, stable-ID graph and GUI-agent assessments refer to their respective frozen 16-, 17- and 20-tool builds. | Broader live mutations, remote workflows and broader agent workflows remain |

Initial saved-shortcut registration conflicts now have bounded recovery acceptance: the GUI stays visible with the complete set inactive, Start disabled, and local Retry or preference correction available. See the [contract](startup-shortcut-recovery-contract.md) and [assessment](startup-shortcut-recovery-assessment-2026-10-15.json). Tray/native-window startup failures and actual Run-key/logon/reboot validation remain open.

The October 14 appearance/bounds group passed its [independent assessment](appearance-assessment-2026-10-14.json), including actual native window geometry and the Ouroboros workflow. Extended cursor layout/idle behavior separately passed the bounded [cursor-layout assessment](cursor-appearance-assessment-2026-10-14.json). Compact Home/navigation and later appearance/density passed separate bounded assessments ([Home](control-center-cleanup-assessment-2026-10-14.json), [presentation](application-presentation-assessment-2026-10-14.json)); those individual assessments do not establish exact MouseMux visual equivalence, broad Unicode coverage or broad feature-workflow parity; the separate host-bound native Unicode baseline below accepts sampled glyphs. The [application lifecycle assessment](app-lifecycle-assessment-2026-10-14.json) passed bounded virtual preferences/tray/hotkey/startup/cleanup workflows; it does not establish real logon/reboot behavior. Annotation passed its [independent assessment](annotation-assessment-2026-10-14.json), including native input policy and decoded PNG pixels. Magnifier passed its [independent assessment](magnifier-assessment-2026-10-14.json). Macro Studio passed its [independent assessment](macro-studio-assessment-2026-10-14.json) for bounded virtual workflows; the [57-control Ouroboros run](macro-studio-ui-2026-10-14.json) and [control-center capture](macro-studio-control-center.png) are preserved. A 31-control assigned prefix retains its original aggregate NOT ACCEPTED result; a separate 15-control capture/stop tail and fresh EOF checks were included in the independent assessment without relabeling the prefix. See [validation](validation.md) for exact evidence and scope limits.

Virtual-only supervised clean Stop/restart now passed bounded native acceptance on the same original x64/x86 targets, with a separate GUI Start/Stop/Start/Stop and actual Busy rejection. Failure and worker-loss checks preserve quarantine until original exit. Direct CLI, physical/fixed-relative paths, hardware and arbitrary applications are outside scope; full P1 and Ultra parity remain open. See the [contract](clean-restart-contract.md), [assessment](clean-restart-assessment-2026-10-15.json) and [validation](validation.md#clean-stop-and-virtual-restart--october-15).
The shared emergency Stop broker passed bounded virtual/native and GUI acceptance for concurrent standalone and default-GUI participants using the same executable path/build and Windows logon/desktop. Emergency activation stops all participants; ordinary GUI Stop, standalone MCP Stop and EOF remain session-local. Broker loss fails closed through ordinary Stop; GUI Start remains disabled while the enabled default binding lacks coverage, until an explicit healthy Retry or an accepted policy change removes/replaces that binding. Start health-checks when the binding is enabled. Hardware key activation, mixed binaries/paths, other desktops/logons and arbitrary applications remain unverified; the captured footer status is cropped and full P1, UX and Ultra parity remain open. See the [contract](shared-emergency-stop-contract.md), [validation](validation.md#shared-emergency-stop--october-15) and [bounded assessment](shared-emergency-stop-assessment-2026-10-15.json).
## UI implementation policy

Keyboard configuration passed [bounded independent acceptance](keyboard-configuration-assessment-2026-10-15.json). Seven numeric settings across eleven responsive call sites use an application-owned adapter around the pinned Ouroboros slider; arrows, endpoint/Page keys, disabled gates and responsive identity have egui-frame coverage. Real virtual GUI navigation and sampled Windows UIA names/range/SetValue passed, with generic Apply completion and a separate stopped Save/Load continuation. The [contract](keyboard-configuration-contract.md) records the native sample, interrupted original Save, screenshot limits and remaining accessibility work. This does not establish full keyboard or screen-reader coverage.

Disconnected-client cleanup passed [bounded independent acceptance](orphan-client-cleanup-assessment-2026-10-15.json): failed saved releases survive EOF, cleanup continues across users, other clients retain shared holds, and process exit completes the pending obligation. Ouroboros pending/error and clear-state originals passed at 680×620; the status region is separate from the permission form. See the [contract](orphan-client-cleanup-contract.md) for process-local state, native-data versus later UI provenance and limits. Broader keyboard accessibility, reliability and full Ultra parity remain open.

Shared failed-cleanup recovery passed [bounded independent acceptance](failed-cleanup-recovery-assessment-2026-10-15.json): standalone and GUI quarantine affected original processes, exclude unused allowed targets, block Start after failure/profile changes/late delivery, and allow explicit fresh-target Start after exit. Compact Ouroboros recovery details and disabled Start passed two 360×360 captures; the separate offscreen target-status row and scrolling remain unverified. The [contract](failed-cleanup-recovery-contract.md) records process-local state, source-only assigned-partial/ACK-timeout coverage and broader reliability limits. P1 and full UX/Ultra parity remain open.

Follow the public interaction model: compact colored user roster, device status, mode controls, per-user settings, shared profile management. The historical compact Home/navigation passed bounded acceptance with 420×600 default and 360×480 minimum geometry, a pinned Configure user/Settings footer, and Settings → Tools routes to Input lab and Agent Access. The [assessment](control-center-cleanup-assessment-2026-10-14.json) covers 51 recorded control occurrences across navigation, visual, lifecycle and routing-options tails; three original aggregate artifacts remain NOT ACCEPTED. This validates a cleaner Home hierarchy and navigation only, not complete feature-page UX parity or exact MouseMux visual equivalence. Implement independently with ProtoInputMultiPlex branding, original palette, proportions, icons, and explanatory text. Reference screenshots are research material, not distributable product assets.

The current presentation batch implements a 360×360 short Home, a 320×360 minimum, an aligned identity/device/name table, original vector status marks, clearer Start/Stop emphasis, a full-width Configure action, Ouroboros mode radios, task-grouped Settings and responsive compact forms. Repeated Home status chrome is removed while errors and warnings remain visible. Bounded independent [presentation acceptance](application-presentation-assessment-2026-10-14.json) passed corrected wide-field alignment, actual Light/Dark and zoom controls, complete minimum-size Multi keyboard visibility, persisted Light/75% rendering and the revised user selector. The assessment preserves earlier failed captures and records separate source/build provenance. Saved Light/Dark and 50–200% UI zoom also have optional fields in the existing MCP preference command. The [presentation producer record](home-density-rust-checks-2026-10-14.json) and [lifecycle producer record](hidden-lifecycle-rust-checks-2026-10-14.json) each passed formatting, strict Clippy, 135 tests and both Windows builds. The [bounded per-user layout assessment](per-user-layout-assessment-2026-10-14.json) accepts compact entry, minimum-size usable rows, preserved user values, validation and moved Apply. The [per-user tools assessment](per-user-tools-assessment-2026-10-14.json) additionally accepts selected-user Draw, Magnifier and Macros navigation, stopped saves, live ACK ownership, corrected minimum tab/generator geometry and representative Dark/Light muted-text readability. The [four-form acceptance](per-user-forms-acceptance-2026-10-14.json) separately accepts Input/Motion/Buttons/Wheel hierarchy and representative minimum-size usability. Shared-GUI annotation Configure/ClearUser/ClearAll separately passed [bounded MCP/GUI acceptance](connected-annotation-assessment-2026-10-14.json), including current-scope shortcut guards and input ownership. The [Settings category assessment](settings-hub-assessment-2026-10-14.json) accepts progressive disclosure into five categories, preserved thirteen routes, five representative actual destination entries, remembered category and minimum Tools reachability; it does not certify the destination pages themselves. An optional installed Windows Malgun fallback passed [bounded GUI CJK acceptance](gui-font-fallback-assessment-2026-10-14.json) for representative Han/kana/Hangul names in Dark/Light and compact layouts, preserving existing Latin/icons. One sampled emoji remains a box; native cursor labels, broader font coverage, other feature-page layout, accessibility and full UX parity remain open. See the [UX scope](ux-fidelity-next-plan.md).

Expose working user/device setup, profile management, preview, mode/keyboard controls, live target/role status and cursor settings. Desktop routing currently uses app-local virtual input and click-selected eligible processes; the agent interface requires an explicit PID scope. Legacy composite automation, the earlier numeric replication flow, Macro Studio and both stable-ID graph groups passed their recorded bounded virtual gates. Advanced graph scope and evidence are in [validation](validation.md) and the [assessment](advanced-replication-assessment-2026-10-14.json); they do not cover physical-device behavior or broad app compatibility. Composite and per-user workflows cannot start input programs over one another; explicit legacy Cancel stops all programs, while Macro Studio Cancel is scoped to the selected user. Keep remote and unsupported device controls unavailable until their Rust services work.

The opt-in [Native controls workflow](native-controls-contract.md) passed [bounded composed acceptance](native-controls-assessment-2026-10-19.json): separate current-binary Ouroboros configuration originals plus a fresh two-process x64 GUI/standalone run. It covers persistent ordinary EDIT focus, Tab, simultaneous native mouse capture with independent releases, policy-change denial preserving held input, ordered MCP keyboard setup and clean Stops/reuse. Compatibility remains the default. An earlier GUI stall and three later transient responsiveness timeouts remain OPEN/P1; this does not complete ordinary-desktop defaults, broad application compatibility, full UX or Ultra parity.

## Delivery and acceptance

### Window Lock checkpoint — October 20, 2026

The [bounded assessment](window-lock-assessment-2026-10-20.json) accepts stopped per-user Window staging without saved HWND/token state, serial startup handoff, live selection and bounds Apply, dynamic client-rectangle following/clamping, stale identity denial and clean Stop. It used one x64 GUI, two virtual user pairs and two owned x64 probes. Sampled 540×520, 360×360 and 680×620 screenshots do not establish full compact layout. Mixed-DPI/reconnect, hardware, broad compatibility, same-process isolation, general P1 reliability and full Ultra/UX parity remain open. The [workflow contract](window-lock-workflow.md) gives operational and agent boundaries.

The follow-on [compact Lock layout assessment](lock-layout-assessment-2026-10-20.json) accepts the Movement bounds menu, draft/applied summary, contextual mode-specific Apply, monitor and invalid-Region feedback, and whole-response reachability at 320×360 and 360×360. The 75-action run used virtual-only owned probes, made one live A Region settings commit, and inspected acknowledged GUI/MCP state without routed input. Scrolling is part of the accepted workflow; this does not establish all controls at once, complete compact-page UX, or full Ultra parity. Failed native attempts 001–006 retain their FAIL results.

The later [Draw/Magnifier hierarchy pass](draw-magnifier-ux-assessment-2026-10-14.json) accepts stopped-GUI ordering, real default-closed diagnostic disclosures, visible invalid-draft errors and unchanged saved settings. Its eight frames include representative whole actions at minimum size; populated runtime details, live routing and complete feature-page polish remain separate. This does not establish complete MouseMux UX parity.

1. Resolve native build blockers and verify two physical mice/keyboards against disposable targets. Test x86/x64 targets, unplugging, exit, partial failure, and normal stop.
2. Add a supervised Rust worker, readiness acknowledgements, timeouts, emergency stop, and recovery while the UI remains responsive.
3. Implement desktop overlays, DPI/monitor bounds, target picker, switched ownership, and concurrent per-process routing. Publish a tested application compatibility matrix.
4. The legacy composite macro workflow, deterministic replication ordering, bounded queues, feedback-cycle prevention, Macro Studio, stable-ID graph baseline, advanced replication options and live virtual-user lifecycle passed their recorded bounded virtual assessments; settings transforms, mapping composition and safe replacement passed their native, UI, assigned-process and boundary gates. Macro Studio's 22-check native run, origin/recording supplement with two substantive and two setup checks, 57-control Ouroboros run, assigned capture/stop tail and fresh EOF group are detailed in [validation](validation.md). Its 31-control assigned prefix retains NOT ACCEPTED aggregate status despite its accepted controls; it is not relabeled. The graph assessment preserves its 37-control UI prefix and the advanced assessment preserves earlier NOT ACCEPTED native/queue/UI prefix aggregates; passed subgroups and later tails are recorded separately without relabeling the prefixes. The generated-user assessment likewise preserves its 26-control UI prefix aggregate as NOT ACCEPTED and separately accepts the nine-control cleanup tail; see [the live lifecycle assessment](virtual-users-assessment-2026-10-14.json) and [behavior contract](generated-clone-plan.md). Physical-device validation remains separate.
5. Add remote and extended-device adapters. Preserve seat identity across reconnects; evaluate each integration's license before distribution.
6. Add installer and migration coverage, accessibility, performance testing, and separate real logon/reboot validation for startup behavior. Passing disposable-provider tests is not real startup validation. No artificial licensing cap does not imply unlimited hardware capacity.

ProtoInput alone does not supply independent focus within one process, remote collaboration, or desktop-wide isolation. Keep backend extension points so the UI does not depend on a single seat-input provider.

The utility-page hierarchy passed [bounded independent acceptance](productivity-ux-assessment-2026-10-15.json), and shared utility chrome passed [bounded independent acceptance](shared-utility-chrome-assessment-2026-10-15.json): Ghost Back to Home, destination-derived category breadcrumbs, and the concise Graph queue summary with default-closed diagnostics. The [shared utility contract](shared-utility-chrome-contract.md) and [validation record](validation.md#shared-utility-chrome--october-15) document the 25 phase observations, retained originals, producer checks and limits. Earlier failed aggregates remain unchanged. The focused target-exit recovery slice passed [bounded independent acceptance](target-exit-recovery-assessment-2026-10-15.json) on its fixed build; the raw evidence keeps its original pending-data-review label. See the [recovery contract](target-exit-recovery-contract.md) and [validation](validation.md#target-exit-recovery--october-15). Broader P1 reliability, including persistent-alive dead-window interleavings, assigned recovery and remaining coverage, remains open alongside page density, accessibility/display coverage and full UX/Ultra parity.

The native cursor-label [baseline assessment](native-cursor-unicode-baseline-assessment-2026-10-15.json) independently accepts readable Latin and representative Han/kana/Hangul at the sampled 14/32 px sizes on this host. Three owned overlay originals also confirm label geometry and hotspot alignment; ordinary Stop released both virtual keyboards and destroyed the known overlays. Existing GDI rendering passed these samples, so no fallback change was needed. This does not establish emoji, all-Unicode, locale-specific Han forms or other-host font coverage. Per-user application cursor following has separate [bounded acceptance](app-cursor-following-assessment-2026-10-15.json).

Concurrent assigned-route release recovery passed [bounded independent acceptance](assigned-release-recovery-assessment-2026-10-15.json): saved key/button UP ownership, healthy-user continuity, nonterminal GUI/MCP errors, and conservative failed-Stop recovery. A separate GUI-only correction passed 320x360 pixels for both user rows and a contained release-details dialog. This does not establish full compact polish, native fixed-relative behavior, hardware or arbitrary-application compatibility. See the [contract](assigned-release-recovery-contract.md).

The [keyboard delivery assessment](keyboard-delivery-assessment-2026-10-15.json) tracks per-user raw-source enablement, Clicked/Focused/Direct/Active/Caret recipient policies, retained releases, live Ouroboros controls, and existing MCP configuration/state. Its native scope is two owned x64 processes using an explicit focus-preserving custom target; application-driven focus changes and Tab navigation under the pinned backend remain unverified. The minimum form shows a whole checkbox and Apply action, with recipient modes reached by scrolling; simultaneous checkbox-plus-mode visibility is not established. Physical-device, native fixed-relative, broad application compatibility and full UX/Ultra parity remain open. See the [contract](keyboard-delivery-contract.md).

The October 20 [discovery assessment](gui-discovery-assessment-2026-10-20.json) accepts background Dashboard inventory/readiness/picker scans and optional GUI-connected MCP freshness metadata. The 20-action virtual-only workflow retained navigation and Stop/normal close during a delayed live scan. Current-generation Start/picker guards, bounded queues and stale-result rejection preserve target scope. The later [background GUI Start assessment](start-preflight-assessment-2026-10-20.json) separately accepts supervisor-owned authoritative preflight, pending Stop/close, same-GUI fresh restart and read-only MCP inspection in an 18-action virtual workflow. See the [pending Start capture](start-preflight-pending.png). Cancellation is cooperative between phases; Raw Input capture startup, arbitrary blocked Windows calls, the historical stall cause, general P1 reliability, performance, full UX and Ultra parity remain open.

## Input forwarding — bounded virtual acceptance

Stable-ID, version-1 forwarding supports active virtual users in automatic desktop routing, with at most one outgoing edge per source. Pointer follows chains once to the terminal user and chains may converge; optional keyboard follows enabled hops, stopping locally at the first disabled hop. Ouroboros controls keep changes staged with explicit dirty, pending, and unavailable states. Live Apply requires a matching worker acknowledgement; stopped edits require the existing Save profile action. Forwarding and replication are mutually exclusive. Runtime overlay hiding preserves each source's saved visibility preference, and GUI Clear restores configured visibility.

The existing 23-tool MCP surface extends `routing_graph` with `inspect_forwarding`, `apply_forwarding`, and `clear_forwarding`. Connected graph mutation requires the expected forwarding revision, session-control permission for every active user, and the full explicit nonempty automatic PID allowlist including selected recipients. Each connected input is separately authorized for source, every hop, actual sink and PID.

The [October 21 bounded assessment](input-forwarding-assessment-2026-10-21.json) accepted one owned virtual GUI/MCP workflow with three ordinary EDIT probes, two normal worker cycles, partial keyboard forwarding, retained motion anchor across graph revision, convergence hold cleanup, stopped Save/restart, MCP Apply, GUI Clear and normal process cleanup. This is one bounded virtual development workflow. Earlier native attempts 001–003 remain FAIL with original reports preserved; the later attempt004 result does not relabel them. Physical devices, arbitrary applications, desktop-wide or same-process isolation, RustDesk, historical GUI-stall P1 resolution, and full Ultra parity remain unvalidated. The P1 remains OPEN.


## Opt-in UI-script PNG capture liveness — bounded acceptance

The opt-in `--ui-script` screenshot path sends renderer-owned frames to one worker with a single replaceable latest-frame slot and a 16 Mi-pixel bound. Request-time tickets reject stale/mismatched frames and preserve PNG modification-time freshness. The worker encodes outside the GUI hook, writes a same-directory temporary, then atomically replaces the PNG; failures preserve the prior file. Drop clears pending work and signals shutdown without joining a potentially blocked writer. Other synchronous telemetry and renderer behavior remain outside scope.

The [October 21 bounded assessment](gui-capture-liveness-assessment-2026-10-21.json) accepts one owned configuration flow with no ProtoInput DLL staged: six real clicks and six sample acknowledgments, with a complete 540×520 RGBA PNG whose request timestamp was 2.8298164 seconds after the Native controls event; the latest published generation was 2, with no failed or rejected generation reported. The GUI returned Home and closed normally; no ProtoInput DLL was staged, and no Start or routing action was used. This does not establish general GUI responsiveness or resolution of historical native-controls GUI-stall P1, which remains OPEN with cause and fix unproven.


## UI-script live I/O liveness — bounded acceptance

Ongoing opt-in UI-script snapshots, click/phase/ack appends, flush and `sync_data`, and bounded sample-count polling run on one worker. Snapshots are bounded and coalesced by path; critical records remain FIFO and durably gate exact-ticket click advancement. A matching in-flight pointer release may finish while its phase record is pending. Failure cancels/releases an unfinished virtual press once and reports status through the existing UI-script status path. Initial `Script::load` filesystem work and bounded payload serialization remain outside the worker, with serialization still on the GUI thread.

The [October 21 bounded assessment](gui-script-io-assessment-2026-10-21.json) accepts one owned virtual-seat configuration flow: six clicks/sample acknowledgments, progress frames 288→311 while sample 3 held, durable sequence 30 after final click sequence 29, and a fresh 540×520 RGBA PNG 2.6357358 seconds after the Native controls event. The flow closed normally; one 160-byte temporary snapshot file remained, so complete temp-file cleanup is not claimed. This does not establish resolution of historical GUI-stall P1 (OPEN; cause/fix unproven) or general GUI responsiveness. See the [contract](gui-script-io-contract.md).


## Standalone i686 controller/worker — failed attempt

The first standalone i686 attempt remains **NOT_ACCEPTED**: `routing_start` failed before a worker was observed, and its sanitized error was not retained. Source inspection of that frozen build identified an x64-only guard and Loader64 selection, explaining the startup failure at source level. Later checked-in source supports x86-to-I386 with Loader32, superseding that build's unsupported status without invalidating the historical diagnosis. A separately reviewed bounded core accepted an actual i686 controller and direct-child worker for concurrent virtual routing, settings rejection, capture/release, and ordinary Stop/cleanup. The native009 aggregate remains **NOT_ACCEPTED** because its post-unhook probe sampled ambient Space; post-Stop A-reusable/C-fresh eligibility was not observed. See [reconciliation](native-i686-worker-core-reconciliation-2026-10-21.json).

## GUI Macro file Save/Load — bounded acceptance

One development-build GUI workflow completed valid Load, Save replacing a pre-existing sentinel with exact clip bytes, rejected invalid Load retaining the clip, and a second byte-identical Save. The 11-click flow had 11 drained sample barriers, four sequential file operation tickets, matching two-event Load ACK, normal Stop/close, worker absence, zero native input events in both virtual targets, and exit 0 for all owned processes. Independent review accepted the bounded result. Three duplicate Python handle-close tracebacks occurred after completion; a 273-byte UI-script temporary remained. See the [contract](gui-macro-file-contract.md) and [assessment](gui-macro-file-assessment-2026-10-21.json).

This does not establish general filesystem latency, macro playback/routed input, physical or same-process isolation, shipping readiness, full Ultra parity, or resolution of the historical GUI-stall P1, which remains OPEN. Native attempts001/002 and producer attempts001–003 remain failed; they are not relabeled. This evidence is pinned to the frozen Macro producer004 source/build record and does not cover later GUI-drain integration.



## GUI session event drain — bounded acceptance

The accepted change drains at most 256 FIFO session events per GUI callback and requests another repaint after a full batch. Required formatting, strict Clippy, 472 tests (0 failed, 3 ignored), and x64/i686 app and observer builds passed. Focused tests verified the 257th event remains queued while its producer is live and that ACK/Stopped order is preserved across two drains. The existing handler and Stop suppression rule are unchanged. See the [assessment](gui-session-drain-assessment-2026-10-21.json).

This is a finite event-count and batch-allocation bound only. It does not bound handler time, total backlog, Stop/terminal-ACK latency, or general GUI responsiveness. No native cohort was run; historical GUI-stall P1 remains OPEN and its cause/fix is unproven.

The [current-build GUI regression](gui-current-regression-assessment-2026-10-21.json) independently accepts one historical-position, no-Start 12-step configuration flow after the PNG and Script I/O changes: fresh owned frames and ordinary close passed. One transient WM_NULL timeout recovered; a private capture temporary remained. This is a regression checkpoint only. Historical GUI-stall P1 remains OPEN; its cause and fix are unproven.


## GUI Stop cancellation ordering — October 21, 2026

The [bounded ordering assessment](gui-stop-order-assessment-2026-10-21.json) independently accepts publishing session Stop and the GUI stopping-state latch before synchronous Agent Broker cleanup. A deterministic regression holds cleanup blocked and verifies the production latch state and Stop intent precede it; formatting, strict Clippy, 478 Rust tests and both architecture builds passed. No native input matrix was repeated. Broker cleanup still joins synchronously: callback/repaint/Stop latency and the historical configuration-only GUI-stall P1 remain unproven/open. Terminal-event and Agent Access disable cleanup are unchanged; this does not establish full UX or Ultra parity.

The [typed rejection feedback assessment](input-rejection-feedback-assessment-2026-10-21.json) independently accepts a bounded x64 standalone Settings rejection, preserved held Control and its original UP, normal Stop/closure, and a separate later read-only private-image absence audit. Command prevalidation now has distinct reporting from saved-release failure; 481 tests and both architecture builds passed. The original native aggregate remains NOT_ACCEPTED. Mode and GUI feedback were not natively exercised, and this does not establish a GUI-stall fix, broader compatibility or full UX/Ultra parity.

The [background Agent Access lifecycle assessment](gui-broker-lifecycle-assessment-2026-10-21.json) accepts the bounded normal x64 virtual lifecycle core: enable/connect, disable/cleanup/EOF, fresh descriptor/reconnect and Home Stop, with five fresh GUI captures and later read-only cleanup confirmation. Formatting, strict Clippy, 489 tests and both app/observer architecture builds passed. Raw native attempts remain FAIL/NOT_ACCEPTED (fixture startup, then a stale Script-state oracle); the separate composed acceptance preserves them. Blocked OS cleanup, the historical GUI stall (OPEN/P1), hardware and full Ultra/UX parity remain open.

The [compact Agent Access assessment](agent-connection-handoff-assessment-2026-10-21.json) accepts three 680x620 states, prominent connection controls, applied/draft permission separation and one scoped read-only MCP handoff. All five Rust/build checks passed with 494 tests. Exact JSON/path and feedback helpers have source/unit coverage; OS clipboard contents, clicked feedback and a rendered one-client count are unverified. The raw run remains FAIL/NOT_ACCEPTED after exhausting the existing 64-phase diagnostic log; separate composed acceptance includes later read-only cleanup confirmation without replay or cap changes. Historical P1 and full Ultra/UX parity remain open.

The [guided Quit recovery assessment](guided-quit-recovery-assessment-2026-10-21.json) accepts four current-build x64 virtual cases: visible, product-hidden and minimized Quit, plus native close while quarantined. Each reveals the existing recovery dialog without clearing quarantine; original target exit permits normal Quit. All 17 controls and four fresh 680×620 captures passed independent acceptance, with no forced cleanup. Required checks passed with 494 tests and both architecture builds. Ordinary tray Open and historical GUI-stall P1 remain open; this does not establish full Ultra/UX parity.

Stopped tray Open — bounded observation, October 21, 2026: one current-build stopped GUI was hidden by the product's close-to-tray action, then restored by one actual owned tray Open. The same root returned visible and noniconic within eight seconds, and a fresh 680×620 Home capture passed bounded independent review. This records one baseline behavior sample only; it does not explain or resolve the earlier NOT_ACCEPTED Restore failure, establish general reliability, or close the historical GUI-stall P1. See the [bounded assessment](stopped-tray-open-assessment-2026-10-21.json).

Recovery application labels passed [bounded independent acceptance](recovery-application-labels-assessment-2026-10-21.json): the Routing recovery dialog presents the captured original executable basename, PID and existing status, with a default-closed technical-error disclosure. The same name remains with the original identity after exit. Standalone `routing_state` optional-name serialization has source/unit coverage only; no new MCP tool, permission, grant, lifecycle behavior, or full Ultra parity is claimed. The global status footer may still display diagnostics behind the modal. Historical GUI-stall P1 remains open.

Additional official capability dispositions (including items whose parity status remains unassessed) are recorded in [ultra-feature-dispositions.md](ultra-feature-dispositions.md). This supplement does not change priority or claim implementation or acceptance.


The independently accepted [GUI recovery continuation workflow](gui-recovery-continuation-assessment-2026-10-21.json) covers failed cleanup, same-client read-only inspection, original-target exit, explicit fresh Start, cleared Agent Access state, new authority, replacement virtual delivery, and normal Stop/disconnect/Quit. All 45 native controls and 517 Rust tests passed. This bounded group leaves historical GUI-stall P1 and full Ultra parity open; hardware and broader application isolation remain unvalidated.

Cursor settings shared with agents passed [bounded independent acceptance](cursor-settings-coherence-assessment-2026-10-21.json): a clean GUI form follows acknowledged MCP changes; a locally edited form retains its draft and offers Use applied settings. Local Apply requires a matching settings acknowledgement. The 27-control virtual workflow accepted full-object preservation, the conflict action and stopped profile saving. Historical GUI-stall P1 and full Ultra/UX parity remain open.


GUI profile-file operations passed [bounded independent acceptance](gui-profile-file-assessment-2026-10-21.json): startup loading and Save, Load, Import, Export and Restore now run on one Rust worker. Jobs capture the clicked path and profile; late results preserve newer edits and hidden drafts. A failed initial load requires explicit Load or Reset recovery. Quit waits for an accepted write and retains errors instead of silently closing.

Acceptance composes the original startup/Save-edit component with a focused remaining-operation tail. Both raw GUI aggregates remain NOT_ACCEPTED: an invalid snapshot-order assertion stopped the original, and final bookkeeping expected an unused backup after the completed tail. Independent acceptance composes the supported components; no full raw 19-control pass is claimed. Historical GUI-stall P1, application-preferences/lifecycle waits and full Ultra/UX parity remain open.


Application preference transactions passed bounded composed independent acceptance on October 27: local Apply, GUI-connected MCP Configure, shortcut retries and lifecycle shutdown use one Rust owner. The observed virtual workflow retained invalid local shortcut edits across a successful agent change, offered Revert to current values, cancelled an unclaimed request on Stop and kept the GUI responsive during an artificial eight-second worker delay. A revoked client received a generic bridge error, not a cancellation acknowledgement; the owner snapshot separately confirmed cancellation and unchanged file/test-key bytes.

The raw 26-control native run remains NOT_ACCEPTED because its final Shutdown ticket/result was not observed in the published UI snapshot. Separate acceptance combines the observed controls with correct final preferences, normal owned GUI/worker/probe/client exits, no forced cleanup, and the reviewed shutdown-before-close source gate. Direct shutdown-transition telemetry is excluded. Shortcut retries, provider failures and activation races have deterministic source/test coverage; no native retry-button workflow was sampled. Formatting, strict workspace Clippy, 557 tests (zero failures, one ignored) and x64/i686 app/observer builds passed. Only virtual devices and a disposable startup test key were used; no routed input or hardware/RustDesk pointer was used. See the [assessment](gui-app-preferences-assessment-2026-10-27.json). Bootstrap preference loading and lifecycle startup, the historical GUI-stall P1, broader reliability and full Ultra/UX parity remain open.


The reviewed October 27 build passed the [stopped-GUI frame and restore regression](gui-frame-restore-assessment-2026-10-27.json): Keyboard → Native controls after a 320×640 right-edge resize, product close-to-tray/Open, minimize/Open, stopped idle painting and one normal Quit. The same owned HWND produced fresh renderer images and answered direct responsiveness checks. The corrected driver sampled all five clicks; its earlier bookkeeping failure remains NOT_ACCEPTED. This virtual-profile-only check staged no backend DLLs and started no routing, target or MCP process. It reused the accepted 557-test x64/i686 producer without a Rust change or repeated Cargo suite. The historical failure did not reproduce in this sample; cause, general reliability and the broader P1/Ultra parity claims remain unproved.

### Stopped GUI profile setup — October 27, 2026

The [stopped Setup workflow](stopped-setup-contract.md) passed [bounded composed independent acceptance](stopped-setup-assessment-2026-10-27.json). A default-off dedicated `profile_setup` connection can inspect/refresh discovery, configure a whole stopped profile with revision/local-draft protection, and Save/Load/Import/Export through the GUI-selected paths. Current-source acceptance observed all 38 GUI control steps, four file jobs, revocation/reconnect, broker retirement before Start, default-off live Agent Access, normal Stop, re-enable, and connected-client cleanup through a verified owned native Quit command with a fresh positive handler receipt and normal exits. Formatting, strict Clippy, 574 tests (zero failures, one ignored), and paired x64/i686 builds passed.

Prior hide/actual tray Open and hidden Inspect are separate supported observations; failed raw aggregates remain NOT_ACCEPTED. This is not a new popup/accessibility pass or a one-cohort full UI pass. The raw005 cleanup Quit timeout, historical GUI-stall P1, hardware/application breadth and full Ultra/UX parity remain open. The transport retains its 16 MiB encoded-frame bound, so not every profile within the separate file-size limit can transfer; oversized frames are rejected explicitly.

### Responsive GUI startup — October 28, 2026

The [startup workflow](gui-bootstrap-contract.md) passed [bounded composed independent acceptance](gui-bootstrap-assessment-2026-10-28.json). Rust workers now acquire startup leases, read preferences, load optional font/script data and initialize the lifecycle service while a small Ouroboros window remains responsive. The error view wraps its full path and cause above Retry and Close; failed preferences remain unchanged. Retry reached the same-window Dashboard with the saved light appearance, 125% zoom and three-user virtual profile. A verified Quit handler receipt increased from 0 to 1 and the GUI exited normally. The unchanged pending-Close/no-resurrection case uses separately bound original evidence; the original clipped-error cohort remains independently NOT_ACCEPTED.

Formatting, strict workspace Clippy, 580 tests (zero failures, one ignored), x64/i686 paired builds and source/artifact integrity checks passed. Only disposable GUI/profile/test-key fixtures were used; no routing, target, MCP, backend DLL or physical-input operation was issued. Cancellation remains cooperative between native phases. This closes the specific pre-window startup blocking paths, not the historical intermittent mid-session stall, earlier cleanup Quit timeout, general GUI reliability or full Ultra parity.

### Background recovery polling — October 28, 2026

The [recovery polling workflow](recovery-ui-native-poll-contract.md) passed [independent bounded acceptance](recovery-ui-native-poll-assessment-2026-10-28.json). GUI recovery reads and Stop use a short cached publication while the session relay performs native process checks. A fresh 37-control virtual-only cohort showed actual Stopping pixels, advancing frames and owned-window responses, quarantine until the original A process exited, positive same-client A Running→Exited status, same-GUI fresh C-only authority and an exact native click pair, then normal Stop and positive Quit. GUI/A/C exited zero with no force and integrity postflight passed.

Formatting, strict workspace Clippy, 585 tests (zero failures, one ignored), and x64/i686 app/observer builds passed. Fresh samples retain original poll time and connection/policy/recovery identity; unavailable status grants no restart authority. Tests cover a held native Registry, Stop ordering, relay starvation/owner cleanup, source sample age and cursor reset. Earlier native001–003 remain NOT_ACCEPTED, including 003's forced harness cleanup; 002's C no-attachment cause remains unproved. Its Routing-active image is not pending-Stop evidence. This removes a concrete UI blocking path without proving historical stall causation, a deadline for Windows calls, physical-device behavior, arbitrary applications or full Ultra parity.


## Per-user Pause/Resume checkpoint — October 29, 2026

[Bounded independent acceptance](per-user-pause-assessment-2026-10-29.json) passed for the private Rust/Ouroboros candidate: selected-user Pause/Resume, existing MCP `routing_input` actions `pause_user`/`resume_user`, both-client source quarantine across Resume, continued B input, scoped permission rejection, global Pause precedence and normal Stop/Quit. All required Rust checks passed with 678 tests passed, zero failures and one ignored test; the native gate used 21 UI actions, 64 SDK calls, two virtual pairs and two owned x64 targets, with original 540x520 and 320x360 captures. See the [contract](per-user-pause-contract.md).

Concurrent assigned behavior, failed releases/queued-work isolation, client EOF and other-user program continuity have focused source/unit coverage; they have no new assigned or injected-fault native acceptance. Unsupported compositions and switched/shared-keyboard/physical/fixed-relative routing remain outside this group. Native text contents and coordinate/wheel payload magnitudes are unmeasured. The physical folder rename stays user-deferred. P1, complete P3/application coverage, full UX/Ultra parity and release readiness remain open. [Current checkout integration](rust-port-feature-integration-2026-11-06.json)

The bounded Input Mapper math addition provides typed scalar functions, vector
magnitude and normalization in the existing graph catalogue. It does not change
the MCP tool count or claim broader MouseMux parity, physical-device coverage,
same-process isolation or a GUI-stall fix. [Math assessment](mapper-math-assessment-2026-11-06.json)

The Input Mapper now has an original node discovery palette and searchable,
category-filtered catalogue for its implemented graph nodes. This bounded
workflow adds no MouseMux assets and does not complete full Ultra parity;
the historical GUI-stall P1 remains open. [Node palette assessment](mapper-palette-assessment-2026-11-06.json)

The bounded Input Mapper timing work adds typed Delay queues and leading-edge
Throttle behavior to the existing graph. The owned virtual workflow does not
establish hardware support, arbitrary-application behavior, a GUI-stall fix or
full Ultra parity. [Timing assessment](mapper-timing-assessment-2026-11-06.json)

The bounded Screen Tools workflow captures an explicitly selected client window
and returns pixel or exhaustive template-match observations from that snapshot.
Full desktop vision and LLM operation remain open. Selected-window OCR
has the separate bounded scope described below. Mapper Pixel Color
and Image Match nodes are separately covered by the bounded Mapper
screen-observation workflow below. This does not establish hardware support, arbitrary-application behavior,
full Ultra parity or a GUI-stall fix. [Screen Tools assessment](screen-analysis-assessment-2026-11-07.json)


The bounded Mapper screen-observation workflow adds Pulse-triggered pixel and
template-match nodes for explicitly selected client windows. It remains
virtual-only evidence. LLM vision, full-desktop capture, remaining
operators, hardware validation and broader application coverage remain open;
this does not close the historical GUI-stall P1 or claim full Ultra parity.
[Mapper screen assessment](mapper-screen-assessment-2026-11-07.json)


Optional GUI Script capture metadata now correlates screenshot request/receipt
and PNG publication through the existing MCP and Ouroboros diagnostics. It does
not time GPU readback, establish ordinary repaint health or close historical
GUI-stall P1. [Capture-observation contract](capture-observation.md) ·
[Assessment](capture-observation-assessment-2026-11-07.json)


## Selected-window OCR and typed Text (November 7)

Local Latin OCR and an explicit OCR → TextContains → gated KeyPulse graph
passed bounded independent acceptance on two owned normal-pump targets.
The same existing capture tool returns authorized snapshot text/boxes;
Mapper results remain actor-private. Authored literals are shared config.
Required checks passed 923 tests, zero failed and 19 ignored. This does
not close historical GUI-stall P1, broad OCR/application quality, hardware,
full Ultra parity or release readiness. [Contract](ocr-text-contract.md) ·
[Assessment](ocr-text-assessment-2026-11-07.json)


## Independent page and user-tab scroll positions (November 7)

Fresh page/user/tab bodies start at the top; returning contexts restore their own
position. Stable user identity isolates Keyboard and other tabs through roster
reordering. Explicit root scroll IDs isolate Application preferences, Output and
Virtual Users. Four real-wheel egui regressions and one stopped virtual-only
native workflow passed independent acceptance; required checks passed 927 tests
with zero failures and 19 ignored. This changes no MCP/SDK or routing behavior
and does not close historical GUI-stall P1, full Ultra/UI parity or release
readiness. [Assessment](page-scroll-assessment-2026-11-07.json)


## Triggered Mapper text output (November 7)

Mapper Type Text types current actor-private Text only on an explicit true
Pulse into a selected virtual user's retained keyboard recipient. Actual OCR
phrase and supplementary Unicode output were read from an owned EDIT control;
normal and compact Ouroboros authoring exercised picker focus, two port
connections and Undo. The 128 sink-output-effect budget and fault cleanup have
focused source/unit evidence; native coverage is bounded. This gate used an
authenticated out-of-band pipe for EDIT readback. Target stdio preservation has
separate bounded acceptance below. Required checks
passed 938 tests, zero failures and 19 ignored. Historical GUI-stall P1,
broader application/hardware support, full parity and release readiness remain
open. [Contract](mapper-type-text-contract.md) · [Assessment](mapper-type-text-assessment-2026-11-07.json)


## Target standard-stream preservation (November 7)

An isolated compatibility overlay removes injected-backend console ownership and
standard-stream diagnostics. Four owned Rust targets covered x64/i686 with no
console or an already-visible own console. OS/UCRT stream probes, actual mouse,
F6 and text delivery, ordinary Stop/reattachment and natural EOF passed independent
acceptance. All eight captured outputs were exact 512-byte streams; all four MCP
and four target processes exited 0. The old-runtime baseline reproduced PIPE→CHAR
mutation with unchanged handle numbers. Required checks passed 938 tests, zero
failures and 19 ignored; both native architectures rebuilt and the current runtime
was updated. This is bounded Release/UCRT/virtual evidence; broad application/CRT
coverage, GUI-stall P1, hardware, full Ultra/UI parity and release readiness remain
open. [Contract](target-stdio-contract.md) ·
[Assessment](target-stdio-assessment-2026-11-07.json)
