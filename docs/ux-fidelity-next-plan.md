# Compact control-center UX

Status: bounded independent application-presentation acceptance passed. The current Home has an aligned identity/device/name table, tighter rows, clearer Start/Stop emphasis, a full-width Configure action and one routing-state indicator. Actual minimum-size Multi keyboard visibility, revised selection and persisted Light/75% rendering passed the [assessment](application-presentation-assessment-2026-10-14.json). Formatting, strict Clippy, 135 tests and both Windows builds pass in the [producer record](home-density-rust-checks-2026-10-14.json). Native lifecycle has a separate bounded [acceptance assessment](app-lifecycle-assessment-2026-10-14.json). Remaining feature-page UX is pending; this does not establish full MouseMux UX parity.

The user's October 14 comparison is the design constraint: match the reference product's concise workflows and density with original Rust/Ouroboros components. Feature availability alone does not satisfy this requirement.

The per-user tab/content layout passed bounded independent [acceptance](per-user-layout-assessment-2026-10-14.json): compact 540×520 entry, whole usable rows and pinned Apply at 320×360, selected-user values across tabs and switches, visible validation, and a short live Apply/active-edit guard workflow. The [compact chrome source review](per-user-compact-chrome-source-review-2026-10-14.json) and [producer checks](per-user-compact-chrome-rust-checks-2026-10-14.json) are separate evidence. Earlier failed/pending aggregates remain unchanged, including the initial minimum-height rejection and the scripted-click retry diagnosed before the acknowledgement repair. The assessment links those records and original pixels. This accepts the bounded nine-tab layout, not full feature-page or MouseMux UX parity. The [public per-user screenshot](https://www.mousemux.com/images/pages/manual/user-input.png) remains a structural reference; no reference assets are distributed.

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

The [per-user tools assessment](per-user-tools-assessment-2026-10-14.json) accepts the twelve-tab workspace with Draw, Magnifier and Macros bound to the selected roster user. It covers stopped saves, three live command acknowledgements to the intended user, corrected minimum tab/generator layouts and representative Dark/Light muted text. The initial narrow scrollbar finding remains recorded separately; full tool-page UX is still open.

The [four-form acceptance](per-user-forms-acceptance-2026-10-14.json) accepts the subsequent Input, Motion, Buttons and Wheel hierarchy pass: paired bounded sliders at normal size, stacked controls at minimum, aligned mappings with controls before help, concise wheel sections and unchanged selected-user settings behavior. Earlier incomplete captures and the initial NOT ACCEPTED assessment remain preserved. Representative minimum rows are accepted; this does not prove every field simultaneously visible or every scroll position.

The optional installed Windows Malgun fallback passed [bounded GUI CJK acceptance](gui-font-fallback-assessment-2026-10-14.json): representative Han/kana/Hangul names render in Dark/Light and compact layouts with unchanged Latin/icons. One sampled emoji remains a box. Native desktop cursor text and broad Unicode coverage are outside that gate.

The [Settings category assessment](settings-hub-assessment-2026-10-14.json) accepts the next navigation pass: five transient categories replace the all-destinations button wall, with a normal category rail and a stacked narrow chooser. All thirteen destination labels/targets remain; five representative routes and category memory have actual stopped-GUI evidence, while the other eight routes retain source/render evidence only. The initial Settings size is 540×480, and minimum Tools entries remain reachable across the two recorded scroll positions. Profiles, preferences and agent grants remain unchanged.

The [Draw/Magnifier hierarchy assessment](draw-magnifier-ux-assessment-2026-10-14.json) accepts controls/actions before default-closed Layer details and Capture details, visible invalid-draft errors and unchanged profiles/preferences across 28 stopped-GUI observations and eight original frames. Draw Apply/Clear and Magnifier Apply/Refresh have whole-control minimum-size pixel evidence. PNG minimum visibility, populated runtime details and live behavior were outside this gate. The forms still require scrolling; Draw's tool-selection styling and tall color controls remain broader polish gaps.

Next UI work remains on the other feature forms, keyboard/accessibility, broader font coverage and visual consistency. The later connected-magnifier batch also corrects the default stopped-session status wording; earlier screenshots retain their original text. Keep each pass bounded and reuse existing settings/MCP contracts. The user’s visual concern is hierarchy and density; avoid expanding behavior during a layout pass.

The independent October 14 visual critique identified three Home priorities: consolidate repeated routing/status text, tighten the roster into aligned identity/role/pointer/keyboard/name columns, and strengthen Start/Stop while keeping Configure and the footer secondary. The subsequent density pass addressed those priorities and satisfied full-control visibility in the current-image assessment. Preserve the earlier reviews in `runtime/acceptance-graph-application-presentation-zoom-tail-20261014-185342` and `runtime/acceptance-graph-application-presentation-long-home-20261014-185433`: the former passed eight appearance controls, while the latter captured a partly clipped Multi keyboard row without establishing the maximum scroll position. Those original results remain unchanged; the later gate accepts the correction separately.

Reviewed the [public main-window screenshot](https://www.mousemux.com/images/pages/manual/main-ui.png) and [manual](https://www.mousemux.com/pages/manual/) on October 14, 2026. Its main window has a small user table, a prominent routing action, subordinate mode controls, Configure, and a short bottom action row. Per-user configuration lives separately. Reference imagery remains research material and must not enter distributed assets.

## Historical gaps before the Home/presentation pass

The accepted local `control-center-home.png` is 420×600 logical client pixels. A two-user roster leaves a large unused area between routing controls and the pinned footer. The large product header and tagline consume space before the roster. Role/pointer/keyboard columns use terse letters that require a tooltip. Secondary pages all expand to 900×720, including very small forms; `control-center-selected-user.png` shows the mismatch. Global Settings presents several separate automation destinations instead of a coherent task hierarchy.

## Original implementation sequence

1. Give Home a restrained single-line identity/status header, an aligned compact roster with original role/pointer/keyboard icons and accessible labels, one obvious Start/Stop action, a quiet mode-options affordance, Configure user, Settings and Hide. Preserve accurate unavailable states and current virtual-only validation limits. User count should influence the initial content height within bounded minimum/maximum sizes; a long roster scrolls without hiding primary controls.
2. Replace the blanket 900×720 secondary-page size with deliberate compact sizes for per-user settings and application preferences. Keep spacious canvases for graphs, preview and timeline editing. Back/Home and Apply/Revert remain predictable. No per-frame resize that fights manual resizing.
3. Group global navigation into Application, Users/devices, Profiles, Tools and Agent access. Keep diagnostics in Tools. Consolidate overlapping automation entry points while preserving their implemented behavior; do not expose several competing top-level routes to the same task.
4. Use Ouroboros tokens/components consistently, reduce repeated headings/explanatory paragraphs, align labels and controls, and use progressive disclosure for advanced tuning. Long device paths and implementation details belong in expandable diagnostics.
5. Add real application Light/Dark appearance and bounded UI zoom with readable native fonts, stored separately from user profiles. These are public workflow elements that the present dark-only theme does not cover. Keep theme/zoom accessible in the GUI and through the app-preference MCP contract where appropriate; no new agent tool just for a count increase.

## Finite independent gate

The bounded gate covers Home density, navigation and application appearance. Remaining feature-page cleanup and broader interaction coverage are outstanding; this gate does not establish complete Ultra UX parity.

Review actual owned-window captures of a short roster, a long/long-name roster, one compact user configuration, global navigation and application preferences. Exercise actual relevant controls, Light/Dark and zoom boundaries, and verify primary actions remain visible at the supported minimum. Include one virtual routing start/pause/stop navigation tail only if changed control dispatch warrants it. Existing routing, macro, graph and cursor rendering suites are not replayed for visual changes. Preserve old accepted captures and provenance; save new captures separately.

An independent reviewer must judge density, hierarchy, legibility and workflow fidelity against the reference, separately from functional correctness. Do not mark the broader UI target complete from this finite gate: remaining feature pages, keyboard/accessibility workflows and unsupported integrations still require their own evidence.
