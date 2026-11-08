# Feature workflow UX delivery — October 15, 2026

Implementation is in the isolated current-file checkout `ProtoInputMux-ux`. Required Rust checks and independent offline acceptance passed. Live changed-workflow observations passed, with a separate Stop qualification and evidence limit described below. Integration uses the agreed hunks; a read-only apply check against main passed without changing its files.

## Changes and boundaries

| Workflow | Result |
| --- | --- |
| Output mapping | Stable-user drafts and unfinished key entries survive switching, navigation, appended users and external updates. Apply requires its matching acknowledgement; Save to profile changes memory; Discard restores the latest accepted settings. Compact pinned actions remain reachable while the form scrolls. Invalid or unadded key entries block submission with an explanation. Unsupported physical/keyboardless configurations remain unavailable. |
| Profiles | Save/Load are primary. Transfer, recovery, targeting scope and backend details start closed. Dedicated wrapped feedback explains completed actions and errors. Target-scope changes require stopped routing. Existing atomic file, backup, reset and memory-only Undo behavior is preserved. |
| Legacy automation | Named From/To selectors, separate rows for action controls, a primary Play action and closed secondary sections improve the existing form. Global Cancel behavior and worker commands are unchanged. Modern Macro Studio, Graph and Virtual Replicas remain preserved. |
| Navigation and agent guidance | Output follows Home's selected user on entry while preserving drafts. All thirteen Settings destinations were exercised. Opt-in app-local snapshots support evidence collection; no public MCP tool or default grant was added. The narrow repository skill paragraph documents commit boundaries and external-update behavior. |

The [inventory](feature-workflows-ux-inventory.md) records the wider audit and reused accepted pages. The [contract](feature-workflows-ux-contract.md) defines the precise behavior. Public MouseMux manual screenshots informed hierarchy and workflow patterns; code, wording, visuals and branding are original Rust using the pinned Ouroboros framework. Vendored native sources were not changed.

## Before and after

These links contain original product pixels copied without editing. The [manifest](feature-workflows-ux-evidence/manifest.json) records their original paths and SHA-256 hashes. Independent review verified all ten image copies.

| Page | Before | After |
| --- | --- | --- |
| Output, normal | [Baseline](feature-workflows-ux-evidence/before-output-normal.png) | [Draft validation and pinned actions](feature-workflows-ux-evidence/after-output-normal.png) |
| Output, minimum | [Baseline](feature-workflows-ux-evidence/before-output-minimum.png) | [Reachable actions](feature-workflows-ux-evidence/after-output-minimum.png) |
| Profiles, normal | [Baseline](feature-workflows-ux-evidence/before-profiles-normal.png) | [Grouped workflows](feature-workflows-ux-evidence/after-profiles-normal.png) |
| Profiles, minimum | [Baseline](feature-workflows-ux-evidence/before-profiles-minimum.png) | [Keyboard Save and visible acknowledgement](feature-workflows-ux-evidence/after-profiles-minimum-qualified.png) |
| Legacy automation, minimum | No new baseline capture | [Contained selectors and actions](feature-workflows-ux-evidence/after-automation-minimum-qualified.png) |
| Settings, representative | Existing accepted layout reused | [Actual route frame](feature-workflows-ux-evidence/after-settings-representative.png) |

Minimum captures measure 320×360 logical client pixels at 96 DPI; normal captures measure 680×620. Baseline and candidate fixture IDs differ, and baseline Output starts at a different scroll position. These are layout references, not identical-profile or exhaustive scroll comparisons. Output's minimum frame shows an ordinary vertical viewport boundary; it does not show every field simultaneously. Profiles' full path/status details continue vertically beyond the visible acknowledgement. Neither limits the evidenced primary controls.

## Validation and independent acceptance

Final producer `143350` passed `cargo fmt --all --check`, strict workspace/all-target Clippy, all 290 workspace tests, and x64/i686 debug builds. All 78 frozen source inputs and ten native runtime files remained stable. The [verification record](feature-workflows-ux-evidence/verification.json) and [test log](feature-workflows-ux-evidence/3.log) preserve results. Builds used the separate `ProtoInputMux-ux-target` directory with two jobs. i686 was built; no i686 runtime execution is claimed.

Independent [review](feature-workflows-ux-source-review-2026-10-15.json) binds the final source and binaries. Offline run `141902` completed 115 actual control observations covering draft preservation, keyboard actions, unsupported states, memory/disk persistence, Import/Export/Reset/Undo/Load and thirteen Settings routes. Its raw result remains `PENDING INDEPENDENT VISUAL REVIEW`: visual review found two minimum-layout issues after functional execution succeeded. The bounded source correction changed only legacy layout; run `144654` then completed 21 focused observations and two replacement frames. Both reviewers accepted those originals. Four unaffected frames and the completed functional observations were reused, rather than replaying the entire suite.

Earlier failed producers and offline runs `134134`, `135414` and `141537` retain their original failures. They are not aggregate passes. Exact owned-process, private startup-key and image-path cleanup passed for the completed offline runs. Test events were app-local; no hardware or RustDesk input was moved or intercepted.

Live run `145712`, using harness `b1d70cb7…`, accepted 45 observations: matching successful GUI Apply for A; a successful authenticated connected update for B and fresh returned/state settings; GUI committed/profile state following B while preserving B's independent draft and A's settings; and ordinary virtual F6 held only on B across both updates, then explicitly released. Exactly two owned x64 probes were used. Session control was limited to those users/PIDs; capture and application-preference grants stayed off. [Original live evidence](feature-workflows-ux-evidence/live-evidence.json) and [all 46 event receipts](feature-workflows-ux-evidence/live-events.jsonl) are preserved.

The automated live aggregate remains **NOT ACCEPTED**: its final Stop predicate expected historical `ui.state.json` to become empty. That file records the last state event and is not a stopped-state oracle. The 46th Stop click is recorded. Independent [source/data qualification](feature-workflows-ux-live-stop-qualification-2026-10-15.json) found fresh stopped recovery (also read by the owner before the timeout), no error or targets, released probe input, absent Agent descriptor, gone captured overlay HWNDs, and exact owned-process/key cleanup. Overlay absence was measured after cleanup, not in an accepted Stop-time sample. The unchanged backend's stopped event follows shutdown/drop. This supports the bounded Stop outcome while preserving that measurement limit and the failed raw verdict. The corrected `2270e7ab…` harness passed `py_compile` and mock checks; it was not rerun natively. Run `145323`'s earlier harness route failure is also preserved.

The independent [assessment](feature-workflows-ux-independent-assessment-2026-10-15.json) approves this finite workflow scope with the recorded limits. This is not a claim that the full automated live gate passed.

## Explicitly unresolved parity requirements

Full MouseMux Ultra/whole-product UX parity is not claimed. Physical device/application validation, arbitrary targets, same-process and desktop-wide isolation, mixed-DPI/display reconnect behavior, broad Unicode/font coverage, full keyboard/screen-reader coverage and unsupported remote/integration adapters remain open in the tracker. The thirteen-route navigation check does not certify every field on all thirteen forms. Annotation/export, GPU presentation and packaging work remain owned by their coordinating chats and are not certified by this patch.

The legacy selector's small dropdown triangle currently renders as a missing-glyph box. Source-only inspection found a literal U+25BE in the shared Button-based `choice_menu`, rather than an Ouroboros Select icon. Font-table inspection found that character absent from the configured proportional fallbacks (Ubuntu, Noto Emoji, emoji-icon, Phosphor Light and installed Malgun); Hack contains it but is not in that proportional stack. Phosphor Light has a `CARET_DOWN` icon at U+E136; a combined follow-up can use Ouroboros's `icon_right` API. This nonblocking affordance/font gap remains explicit and unresolved in this patch. The named controls and popup choices were exercised successfully; no product edit or replay was added for the glyph.

Integration must preserve the main-owned lifecycle Open-budget overlay and current annotation/Agent guidance. The UX patch excludes those backend and annotation files; it contains the agreed UI hunks, narrow guidance paragraph and new evidence/documents/scripts.
