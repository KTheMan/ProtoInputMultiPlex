# GUI macro file Save/Load contract

**Bounded GUI workflow accepted.** This contract covers the legacy GUI Macro file controls in one development-build workflow. It does not make a general filesystem-latency or product-wide responsiveness claim.

Save and Load use a detached, single-worker service with one pending operation. A click captures its exact path and, for Save, an Arc snapshot of the selected clip. Duplicate file actions are rejected while pending. Load reads at most 32 MiB plus one detection byte before parsing; Save bounds serialized JSON to 32 MiB. Save writes a same-directory temporary, syncs it, then atomically replaces the destination; write or commit errors preserve prior destination bytes. Windows path NULs are rejected.

Each request carries session, GUI-mutation, and macro-revision context. Load dispatch requires the original context still match and a ready, non-stopping session with recording/playback inactive and Macro Studio inactive. Stale results are invalidated. The worker’s internal compare-and-load checks the expected macro revision before mutation, guarding against an undrained remote ACK. macro_revision is additive serde-defaulted state metadata; the public MCP request mapping is unchanged.

Drop detaches rather than joins potentially blocked filesystem work. Process exit may leave an incomplete temporary or detached Save; complete temporary cleanup is not claimed. Macro Studio file workflows and general constructor/load work are outside this change.

## Bounded evidence

The [October 21 assessment](gui-macro-file-assessment-2026-10-21.json) records 11 real GUI clicks and 11 fully drained sample barriers. Four sequential file-operation tickets exercised a valid Load (two events acknowledged), Save replacing a pre-existing sentinel with the exact canonical 185-byte clip, rejected invalid Load retaining the clip, and a second Save with identical bytes. Stop and ordinary close completed; all three owned processes exited 0, the worker was absent, and no fallback cleanup was used. Both virtual targets recorded zero native input events.

The independent review accepted the bounded workflow. It also records three Python Handle.Close WinError 6 tracebacks after successful completion, caused by duplicate runner-side close attempts; product and fixture stderr was empty and the independent staged-process audit was clear. A 273-byte UI-script temporary remained in the run folder. The invalid-load error prefix was checked by the reviewed runner; its transient error state was later replaced by the second Save snapshot. Complete temporary cleanup is not established.

Producer attempt004 passed formatting, strict Clippy, 470 workspace tests (0 failed, 3 ignored), and x64/i686 app and observer builds. Producer attempts001–003 remain failed and preserved; attempt003’s workspace-test exit 0xC0000409 has unknown cause. Native attempts001 and002 remain NOT ACCEPTED: timing-oracle failures before any macro file operation. Historical GUI-stall P1 remains OPEN; this assessment does not establish its cause or resolution. No macro playback/routed input, physical-device isolation, same-process isolation, shipping readiness, or full Ultra parity is claimed.

Source review: runtime/gui-macro-file-actual-source-review.json, addendum runtime/gui-macro-file-actual-source-review-addendum.json. Native independent review: runtime/gui-macro-file-ui-attempt003-review.json (SHA-256 eacc2f11b380b5dee21ed3021c965f48808bf87e8b4ed6412a08e05546c735ae). Raw run: runtime/gui-macro-file-ui-attempt003/evidence.json (SHA-256 b296dca9bdaa25cc60aafcb87f3adbb5b8927b2c591ec2da46d0e8bac2d5121c).

