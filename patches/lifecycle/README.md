# Resident lifecycle compatibility overlay

`0004-clean-restart.patch` is incremental to the pinned ProtoInput tree after the
unchanged cumulative `patches/0001-backend.patch`. The overlay contains backend
transport, state reset and acknowledgement mechanisms; user ownership, selected
targets, restart eligibility and final commitment are implemented in Rust.

`scripts/build-backend.ps1 -PrepareOnly` temporarily removes a recognized overlay
while verifying the cumulative base, then reapplies the overlay. Its `finally`
block restores an already recognized overlay even when base verification fails.
Do not regenerate or silently replace the cumulative base to update this feature.
`0006-checked-message-filter-setup.patch` follows both this lifecycle overlay and
`patches/keyboard-state/0005-consumed-keyboard-state.patch`. Build preparation
removes it first and restores it last; the frozen predecessor patches are unchanged.
The patch retains the base tree's mixed newline conventions where required by
the recorded Git blobs; patch whitespace diagnostics are suppressed on apply.

The passive ABI is defined in
`ProtoInputHooks/pipeinclude/MuxLifecycleProtocol.h`; it uses fixed-width fields
and layout assertions for both native architectures. Loader exports are:

- `MuxLifecycleVersion() -> uint32_t` (version 1).
- `MuxLifecycleOpen(uint64_t borrowed_process, uint32_t pid, uint64_t creation,
  const wchar_t* folder, bool inject_fresh, uint32_t timeout_ms) -> uint32_t`.
- `MuxLifecycleBegin(instance, const BeginPacket*) -> bool`.
- `MuxLifecyclePrepareRetire(instance, const RetirePacket*) -> bool`.
- `MuxLifecycleClose(instance) -> bool`.

Open checks the original process handle, PID and creation FILETIME, creates a
first-instance local pipe, optionally injects through its own bounded hidden
helper, and checks the pipe client's PID. Connection and helper waits are bounded;
timeout cleanup can add at most a 250 ms helper-exit wait. Legacy injection exports
retain their old helper-wait behavior. Close disconnects the controller-side pipe
and erases its loader instance; it does not grant reuse or unload a target DLL.

Begin checks the actual pipe server PID and its process creation time against the
Rust lease marker. It maps Rust's pending session transaction read-only. Reply and
event handles are target-local duplicated handles. Once a loader command starts
writing, its `true` result transfers their ownership even after a failed or partial
write. The native receiver closes complete consumed handles on success or rejection.
A `false` loader result means no transfer was started. A transport result alone
is never the target acknowledgement.

Readiness lives at
`Local\\ProtoInputMultiPlex.Lifecycle.<pid>.<creation-decimal>`, with a coherent atomic
32-bit sequence snapshot, architecture, exact build fingerprint, generation and
session. There is one serialized resident pipe thread. It does not reinject an
already resident module or run an old cleanup thread beside a new generation.

States are Initializing (0), Unclaimed (1), Active (2), Prepared (3), Poisoned (4)
and Reusable (5). A complete Prepare acknowledgement yields only Prepared.
Prepared survives pipe EOF while the supervisor waits for successful worker exit.
The native receiver changes Prepared to Reusable only after observing Rust's
matching all-target transaction commit. If the supervisor crashes or cleanup fails,
the retained Pending mapping keeps the target non-reusable until target exit.
Active EOF poisons before fallback cleanup. A legacy connection poisons before
executing any legacy command and never becomes reusable through this ABI.

The new ABI accepts emulated application-local devices only. Rust omits hook 0
(`RegisterRawInputHook`), and the native whitelist rejects it. The existing virtual
transport directly calls `RawInput::ProcessPacket`/`ProcessText`; it does not need
to steal application Raw Input registrations. Hooks 1–10 and existing message
filters remain available. Physical-device and registration behavior continues
through the legacy path. The compatibility path rejects physical selections,
bypass/rename/controller and other unsupported commands. It cannot invoke the
backend's native GUI shortcut through virtual input.

Prepare stops emission and acknowledges cooperative focus-loop pause; waits for
the Rust cursor observer's asynchronous worker/callback/resource quiescence;
requires an empty queue-fence registry; removes hooks; wakes known own target queues
with `WM_NULL`; and requires `LhWaitForPendingRemovals() == 0`. Positive timeout
statuses are failures. It resets selected devices, synthetic held/async state,
cursor/bounds, selected window, forwarding/buffer state, filters, blocks and packet
history only after quiescence. Removed hook handles are pruned from ACL tracking,
and expired backend thread identities are removed before another generation.

Unknown/filtered message pumps, hook-removal failure, missing observer quiescence
exports and unfinished callbacks fail closed. Queue receipts retain their existing
ordinary unfiltered-pump scope. This source mechanism does not establish hardware
behavior, arbitrary application compatibility or desktop-wide isolation. Runtime
acceptance must be recorded separately against the built artifacts.

## Checked MessageFilter installation

The incremental 0006 overlay changes the exact resident build to
`0x2026102900000002` and advertises capability bit 0 in readiness `reserved[0]`.
Lifecycle ABI 1, existing message IDs and named-object spelling stay unchanged.
The Rust controller requires that capability and exact build before configuring
the target. The native reference can still use the existing Begin and void setup.

New message 33, `BeginV2`, appends two target-local duplicated handles to the
48-byte Begin: a persistent 64-byte setup reply mapping and an event. Begin
validates the channel before claiming the generation. The target retains these
handles until successful Prepare or pipe teardown; the host only reclaims them
when no Begin write started. New message 34 installs MessageFilter hook 2 and
carries creation, generation, session and monotonic request IDs, hook/operation
words, and zero reserved bytes. It carries no handles. An identical duplicate
returns the cached result without reinstalling; conflicting or stale requests
never invoke native installation.

The sequenced 64-byte reply reports the correlated identity, request, result and
raw native NTSTATUS. Applied means all four GetMessage/PeekMessage detours have
valid native traces and all observed EasyHook/ACL statuses are exactly zero. The
optimistic `Hook::IsInstalled()` flag is not evidence. A partial install or dirty
native status stops emission and poisons the generation. Success does not certify
application input delivery, callback quiescence, provider readiness or reuse.

Rust uses one deadline across command submission and reply waiting. Any uncertain
transfer or unconfirmed reply latches failure, prohibits input/unfreeze and
retirement, and allows only minimum safe Stop commands. A proven NotStarted
submission may abandon its consumed request and perform cleanup/retirement, but
cannot retry setup or publish input. A failed lazy target remains recorded for
cleanup while cached reopening, cursor startup and input publication are rejected.
The existing all-target Prepare/worker-exit/supervisor-commit contract is retained.

`scripts/verify-native-setup-ack.ps1` builds and executes the native protocol/status
fixture for both MSVC architectures, verifies the executable architecture, and
compares its bytes with `scripts/native/setup-ack-golden.txt`, also consumed by
Rust tests. The fixture covers partial detours, sticky errors, positive timeout
statuses, duplicates, stale identities and skipped requests. Rust tests cover
correlated replies, lost/cancelled replies, ownership, target exit, submission
gates and a failed recorded lazy startup. These fixtures do not replace actual
injected-session, provider, failure and architecture acceptance.
