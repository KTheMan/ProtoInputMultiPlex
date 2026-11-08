# Clean stop and virtual-session restart contract

## Status

Implemented in the Rust supervisor and versioned native lifecycle boundary; the bounded native and GUI acceptance described below passed on 2026-10-15. Overall P1 reliability and full Ultra parity remain open. See the [canonical assessment](clean-restart-assessment-2026-10-15.json).

## Scope

The lifecycle protocol supports clean retirement and later reuse of an already-hooked process for a **virtual-only session**. It does not provide same-process multi-user isolation, desktop-wide isolation, a general attach-to-all-processes operation, or hardware/fixed-relative restart support. Each selected target remains an explicitly selected process.

The native boundary is a small, versioned compatibility protocol. Rust owns target selection, leases, quarantine, retries, and the final decision to make a set of targets reusable. A readiness record is read-only evidence about one PID and process-creation identity; it is not a delivery acknowledgement or permission to attach by itself.

## Start and reuse

A start transaction validates only its required starting target PIDs. An automatic allowlist may identify potential scope; it does not make every listed process an attachment target. GUI-started sessions use the required starting-PID cache. MCP Windows readiness/eligibility snapshots are read-only and do not reserve a target or change its state.

A fresh process may be attached under existing eligibility rules. Reuse is limited to virtual-only supervisor sessions whose process identity, versioned native readiness, lease, and committed lifecycle state are compatible. One controller owns a target at a time. A legacy resident hook without the lifecycle capability is rejected and requires restarting that application. Direct CLI attachment and physical/legacy sessions retain fresh-target-only behavior.

## Stop and all-target commit

During stop, Rust disables routing/input and performs the existing per-target cleanup, including required release delivery and queue-fence/native retirement acknowledgements. The supervisor retains the shared transaction as `Pending` for every target. It changes that transaction to committed only after the worker reports `Stopped`, exits successfully, and all target cleanup has been acknowledged. Native targets may then report `Reusable` for a later eligible virtual-only transaction. A successful release call means transport submission, not proof that an arbitrary application consumed the input.

`Drop`, pipe EOF, controller loss, a `Failed` terminal event, a failed cleanup acknowledgement, or an unsuccessful worker exit is not a commit. Those paths leave targets quarantined/dirty and require the original target process to exit before recovery. A live orphaned controller or shared lock is insufficient evidence that cleanup completed; target-scoped leases and resident markers prevent a second controller from taking over an active or uncommitted lifecycle.

The target process is not reinjected and its resident DLL is not unloaded. For compatibility, the DLL and idle helper threads remain loaded after clean retirement. The clean state means no active hooks, input routing, or observers remain; it does not mean all resident code or idle waiting threads have exited.

## Limits and validation

Bounded acceptance covered two explicit clean Stop/restart cycles on the same original x64 and x86 target PIDs and creation identities, with successful worker exits, original-recipient release checks, no extra old key/button input edges, and the current observer disabled by the new profile. A GUI Start/Stop/Start/Stop workflow and an actual Busy rejection of a competing controller also passed. Failure and worker-loss cases kept targets unavailable until original-process exit. The [canonical assessment](clean-restart-assessment-2026-10-15.json) links the producer record, source review, raw evidence and independent reviews; earlier NOT ACCEPTED aggregates remain unchanged.

The tested controller was x64; the i686 controller was built but not run. Ordinary pump fixtures do not prove all applications, hardware, fixed-relative routing, or transient release behavior in a live target. GUI originals at 360×360 and 320×360 show the tested Start/Stop workflow, but the one-line bottom status is cropped, so full error/readiness text pixels and full compact polish are not accepted. Same-process multi-user isolation, desktop-wide isolation, broad application compatibility, and full UX/Ultra parity remain unverified. On failed or ambiguous retirement, keep the target unavailable until the affected original target exits; do not infer success from timeout, controller exit, or a readiness snapshot.