# Target standard-stream compatibility overlay

0007-preserve-target-stdio.patch is incremental to the pinned dd29bf8d22349605d0e1950c1c5dc4893d72670e
ProtoInput tree after the cumulative 0001-backend patch and existing 0004, 0005,
and 0006 overlays. The predecessor patches are unchanged. This is necessary
backend interoperability plumbing; application policy and new product behavior
remain in Rust. Compilation and native acceptance are separate pending gates.

## Preserved target state

The injected DLL no longer allocates a console, reopens stdout/stderr, captures
the application's console HWND, or changes its visibility. ConsoleHwnd remains
null. The native diagnostic GUI shows an unavailable console-control label;
its retained internal console functions are no-ops. It does not attach/detach
consoles, set standard handles, restore saved handles, or modify caller CRT or
iostream globals.

105 diagnostic output expressions in 14 hook-owned files and four ImGui OpenGL
shader/link error outputs are replaced by no-op expressions. Conditions, return
values, lifecycle error recording, and cleanup remain unchanged. Arguments of
discarded diagnostics are not evaluated; their only nested calls were diagnostic
queries and read-only name/container accessors. No formatter, logger thread,
debugger-output channel, or new native export is added.

IMGUI_DISABLE_TTY_FUNCTIONS is defined only for the injected-hook project in all
four configurations. Direct compiler adapters must pass that same define when
compiling the injected ImGui translation units. Explicit file logging remains
available; the macro does not disable ImGui LogToFile or its generic file I/O.

## Audited closure and exclusions

The supported closure is ProtoInputMultiPlex attachment, configuration, virtual input,
ordinary Stop and resident reattachment. Hook startup, GUI-thread startup, pipe
setup/errors, input paths, Dinput feature logs and cleanup are included in the
suppressed source inventory. Shipping native qualification uses Release.

The cached EasyHook library is unchanged. Its TestFuncHooks diagnostic export
is not called or imported by product code and is outside this supported closure.
Its fallback printf and explicit requested-file fprintf calls are not changed.
No arbitrary invocation of EasyHook test/analysis exports is covered.

BlackBone Release builds define BLACKBONE_NO_TRACE. Debug CONSOLE_TRACE builds
are not covered by this preservation claim. OpenXinput library code has no
standard-stream or console calls; its standalone example is not linked into
the injected product. GLEW utility entry bodies are commented out and its
file-reporting helpers have no injected hook caller; they are unchanged.

Hook-owned code has no stdin consumer. EasyHook's udis86 default file-input
implementation exists, but both actual disassembler callsites replace it with
an explicit memory buffer before disassembly. UD_DEBUG stderr diagnostics are
not enabled by the shipping build definitions. Generic library file I/O is not
rewritten or globally intercepted.

## Build and resident compatibility

The overlay is last in scripts/build-backend.ps1's ordered list. Build preparation
removes overlays in reverse order, verifies the frozen base, and restores them
in predecessor order. Do not rewrite the cumulative base or reset the patched
vendor tree. Mixed source newline/EOF conventions are retained by the patch.

Readiness Build changes from 0x2026102900000002 to 0x2026110700000001 in the native
header and Rust guard; the two Rust native fixture constants follow it. Lifecycle
ABI 1, packet layouts and SetupAck capability bit 0 are unchanged. A pre-fix
resident DLL is rejected by the exact Build comparison; already damaged streams
cannot be repaired by this patch and require normal target restart.

Freeze the private patched vendor tree, overlay/script inputs and toolchain
before root-owned native compilation. Build both native architectures with the
reviewed adapter and unchanged pinned static libraries; the direct adapter must
carry the injected-only ImGui TTY define. Standard MSBuild preparation remains
available through build-backend.ps1. Keep new runtime artifacts/inventory separate
from all prior accepted artifacts until independent publication approval.

Acceptance must observe actual process OS handles and the owned Rust probe's
UCRT descriptors/read-write behavior independently, plus exact stdin/stdout/stderr
traffic before attachment, during input, after Stop and reattachment. Hook-local
/MT CRT internals are audited rather than directly queried. The probe does not
inspect FILE* internals or validate all CRT models; no production test exports or
process-memory probes are added. No helper-DLL test is needed because diagnostics compile
out. Console/no-console and architecture coverage must be reported precisely.
Historical GUI-stall P1, arbitrary applications and full parity remain separate.
