# Target-exit recovery contract — October 15, 2026

## Current fix and limits

When a selected target window disappears and releasing a held input token fails, the failure remains attached to that user. The router clears the stale keyboard recipient but retains the failed token and the user's PID focus/lease for retry or process-exit cleanup. Another user's routing continues. A failed release is reported as pending; retargeting must not be treated as proof that the old hold was released.

When the target process exits, process-exit handling clears that process's native held inputs and releases its PID lease. This does not mean the client's logical claims disappear automatically: that originating client still needs to send its own `ReleaseAll` to clear them. The change does not promise recovery for every vanished window/process, expand grants, or add a new MCP operation. MCP remains at 23 tools.

The [source review](target-exit-fix-source-review-2026-10-15.json) approves the focused production transition and its retry behavior. The [fixed-build producer record](target-exit-recovery-rust-checks-2026-10-15.json) passed formatting, strict workspace/all-target Clippy, 186 workspace tests and x64/i686 builds. Producer checks are not native acceptance.

## Bounded native acceptance

The fixed-build run passed [independent bounded assessment](target-exit-recovery-assessment-2026-10-15.json) (`APPROVED_BOUNDED`). Four groups and nine GUI interactions verified that A's normal exit clears its selected/keyboard PID and capture while routing continues; B retains F7 and receives an exact F5 pair; A's client-scoped `ReleaseAll` ACK preserves B; explicit selection of preauthorized C yields an exact fresh F6 pair; and real GUI Stop delivers exactly one plain native B F7 UP, destroys the two known overlays, and removes the Agent descriptor. Native counters and tagged public-pipe client records are authoritative. All owned processes/handles closed, scoped CIM count was zero, the private GUID was absent, and the exclusive native slot returned.

The [producer record](target-exit-recovery-rust-checks-2026-10-15.json) separately passed formatting, strict workspace/all-target Clippy, 186 tests and x64/i686 builds. The native gate used x64 cooperative owned processes only; i686 was built but not executed. Optional owned-window PNG copies do not establish pixel or phase-freshness acceptance. The raw evidence file intentionally retains `PENDING INDEPENDENT DATA REVIEW`; the independent data-review record and canonical assessment supply the accepted status.

Preserve the three earlier attempts as separate history: the first and second recorded eight GUI clicks each with seven accepted samples, and both remain `NOT ACCEPTED` (the second failed its pre-exit screenshot criterion); the third eight-click diagnostic run remains `NOT ACCEPTED` despite eight accepted GUI samples, and confirmed the dead-HWND release error plus B's UP before cleanup. Across the four runs there were 33 phase clicks and 31 accepted GUI samples, not 33 distinct controls; only the final nine belong to fixed-image acceptance. Earlier builds used 185-test images and do not substitute for the accepted fixed build.

The [skill installation record](target-exit-skill-install-2026-10-15.json) documents the validated installation and source-only wording check; public MCP tools remain 23. The separate [source review](target-exit-fix-source-review-2026-10-15.json) covers the recovery wording. The installation record provides no native acceptance.

Do not claim arbitrary target recovery, automatic grant expansion, or full P1 reliability from this slice. Full MouseMux Ultra parity remains open. After the P1 gate is resolved, the next scope checkpoint is the proposed native cursor-label Unicode baseline/fallback in [the P4 audit](../runtime/p4-cursor-fidelity-next-audit.md); do not replay previously accepted cleanup families solely to advance the milestone.
