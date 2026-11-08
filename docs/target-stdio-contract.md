# Target standard-stream preservation

The supported Release hook path preserves a selected target's standard streams
and console state through attachment, virtual input, ordinary Stop and resident
reattachment. Overlay `0007-preserve-target-stdio.patch` removes console ownership
and stdout/stderr diagnostic writes from the pinned backend. It neither saves
nor restores application handles. Applications retain responsibility for their
own stream and console changes.

The native debug console control is unavailable. Hook-owned diagnostic
expressions and four linked ImGui OpenGL error outputs compile out; injected
ImGui TTY logging is disabled. Explicit file logging remains. This is an isolated
backend compatibility patch; product policy and new behavior remain in Rust.

## Compatibility and operation

Readiness Build is `0x2026110700000001`; lifecycle ABI 1 and the SetupAck layout
are unchanged. Rust's exact Build check rejects a pre-fix resident DLL. An already
affected target needs an ordinary restart; the patch cannot repair its existing
streams. The four-target matrix did not separately exercise old-resident rejection.

The accepted source and ten qualified runtime images are published to this
checkout, with previous runtime bytes retained in the publication backup.
Normal development commands use the existing `runtime` folder. Both controller
architectures and paired observers were rebuilt. The virtual gate used the x64
controller and Rust dispatcher; the two C++ Loader DLLs were compiled and ABI
checked, but were absent from the gate runtime.

## Bounded native acceptance

Four disposable Rust EDIT applications covered x64/i686 and no-console/own-visible
console. Routing was automatic concurrent, with independent virtual keyboards,
clicked recipients and `native_controls` application focus. Each target completed
baseline, attached, stopped and reattached stream probes. OS handles/file types
and the probe's UCRT descriptors were compared independently to their own baseline.

Every phase read 64 distinct ASCII bytes through OS stdin and 64 through UCRT
stdin, and wrote 64 bytes per output through each API. All eight captured output
streams contained exactly 512 expected bytes and reached natural EOF. Both stdin
read paths reached EOF. Each real EDIT received two mouse/F6 pairs and `Mux7319`
twice, observed through GetWindowTextW. Both console states stayed unchanged.
All four MCP processes and four targets exited 0 with no retained owned processes
or cleanup errors. No input was replayed to recover an uncertain result.

The old-runtime baseline changed stdout/stderr from PIPE to CHAR despite unchanged
numeric handle values and added 43 diagnostic bytes to stdout. That failed attempt
stopped before any post-attachment stdin read. Its original result and exact bytes remain
retained; it is reproduction evidence, not a successful application workflow.

## Limits and evidence

This covers the supported Release attach/input/Stop/reattach call graph and the
owned Rust probe's UCRT model. It does not inspect Hook-private `/MT` FILE state
or establish every CRT model, arbitrary applications, physical devices, assigned
or fixed-relative routing, same-process isolation, GUI responsiveness, full Ultra
parity or release readiness. Visible-console evidence is the actual associated
console HWND/visibility state, not a screenshot or pixel claim.

The unchanged EasyHook TestFuncHooks diagnostic export is outside this call graph;
BlackBone Debug CONSOLE_TRACE is outside the Release claim. Cancellation retains
pending-I/O memory through final drain; a missed join fails acceptance. No forced
termination, global input, hardware/RustDesk input, clipboard or process-memory
inspection was used. The GUI-stall P1 remains OPEN and unattributed.

Required Rust checks passed 938 tests, zero failures and 19 ignored. The private
probe passed strict checks, two pure protocol tests and both builds. Native
qualification rebuilt eight PE outputs with 128 successful compiler/library/link
commands. It reused pinned static libraries and the previously approved NETFX
SDK bundle; the historical channel/catalog digest discrepancy remains explicit.

[Assessment](target-stdio-assessment-2026-11-07.json) links immutable qualification,
native results, independent acceptance and publication. Earlier failed build
attempts and the unapproved review-tool attempt are preserved and excluded.
