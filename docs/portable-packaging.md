# Portable packaging

## Application SDK release closure

The builder now requires Rust targets `i686-pc-windows-msvc` and
`wasm32-unknown-unknown` alongside the Windows x64 toolchain. Supply the official
[wasm-bindgen 0.2.129 Windows x64 archive](https://github.com/wasm-bindgen/wasm-bindgen/releases/download/0.2.129/wasm-bindgen-0.2.129-x86_64-pc-windows-msvc.tar.gz)
using `-WasmBindgenArchive`; SHA-256 must be
`79e348c169d0c10c0b287647f1e753d0985353ca96800bc29be8f4261baf13e6`,
matching `.github/workflows/rust.yml` and the pinned Rust dependency. Acquire the
archive before invoking packaging. The builder verifies the copied archive and
tool version, uses this tool directly, and never installs a replacement CLI.
Cargo builds retain `--locked`; dependency downloads use the existing build
semantics. Before source-input freezing or compilation, the builder runs online
`cargo metadata --locked --features mux-application-sdk/native-client
--format-version 1 --filter-platform <target>` for each of the same x64, i686 and
wasm32 platforms used by the final notice inventory. This warms the full
workspace metadata graphs, including dependencies of packages that are not built
for that platform, such as the i686 GUI graph. A failed prefetch stops before
expensive builds. Final staging still requires `--locked --offline` metadata for
all three platforms; the notice dependency union and offline rejection remain
mandatory. This does not change features, the lockfile or compiler targets.

Use a target directory without `sdk-release-build` or `sdk-release` children.
Each native SDK builds in its own fresh Cargo target directory with full default
`native-ffi`/`native-windows` features plus `native-client`. The builder records
the DLL and header from the exact Cargo JSON artifact/build-script pair and
rejects ambiguous or misplaced outputs. This replaces header directory globbing
and does not select a similarly named debug or feature-reduced artifact.

The final bundle includes `sdk/x64` and `sdk/x86` with their respective
`mux_application_sdk.dll`, generated `protoinputmultiplex_application_sdk.h` and
`metadata.json`. `sdk/web` includes the example HTML/bootstrap, generated web JS,
WASM and both TypeScript declarations under `pkg`, plus metadata. The browser
example needs an HTTP server; connection setup remains explicit and carries no
packaged credentials. Native lifecycle fixture executables are qualification
tools and are excluded from this payload.

Each SDK metadata record binds its target, release profile, features, source
commit, frozen input digest and fixed payload hashes. Web metadata also records
the pinned binding tool archive hash. Staging rejects missing, extra, mixed or
modified SDK payloads, validates PE architecture/native-client header identity,
and repeats SDK hash checks after copying. Every native DLL participates in the
existing PE import and colocated dependency checks, even when x64/x86 DLL names
match. The bundle manifest includes these SDK records and all staged hashes.

Notices cover the union of x64, x86 and wasm Cargo metadata with native-client
enabled; missing notices fail packaging. The corresponding-source ZIP adds the
relevant Rust workflows, source/tests for the root Rust client example and
explicit agent skill files. Example evidence directories, runtime connection
descriptors and private state are not added to that source allowlist. Installer
packaging consumes the completed portable tree, including `sdk`, using its
existing bundle embedding path.

Source admission uses paths committed in Git `HEAD`, including the native
submodule's own `HEAD` inventory when present. Sources created by the builder's
approved compatibility patch sequence also need to accompany a normal pinned
submodule. The packager requires native `HEAD` to match the pinned commit and
each supported patch to be committed and unchanged from root `HEAD`. It verifies
the root patch set, then replays the complete builder order (base `0001`, followed
by `0004`, `0005`, `0006`, `0007`) through Git's patch engine in a fresh temporary
index and object database. The native object database is a read-only alternate;
the real index, working tree and Git configuration are not modified. Temporary
Git overrides apply only to replay commands.

Only final added source files under `src/ProtoInput` are candidates for this
exception. Their prepared working bytes must match the final indexed blobs after
all later patch edits, allowing LF/CRLF differences. Git handles both unified
new-file sections and `diff --git` additions; no partial patch text parser is
used. Missing, changed, outside-tree or non-source additions fail. The frozen
source inventory records the committed patch files and complete prepared source
hashes. Committed flattened snapshots use their existing committed inventory.

Other new indexed or nonignored untracked files matching the source allowlist
cause packaging to fail before freezing or archiving. Commit the complete
release source first and keep private
acceptance/configuration files outside source directories. In a flattened
checkout, commit the pinned vendor files, even when upstream ignore rules would
normally hide them. `source-baseline.json` is provenance data and cannot add
files to the archive. Required SDK/build source paths must be committed; missing
working files fail packaging. The root example's `src` and `tests` additions
admit only committed Rust source files. Current bytes of already committed
compatibility patch paths remain included and frozen, preserving the existing
backend preparation workflow.

This recipe has not yet been executed or accepted as a complete release.
Packaging hashes and feature identity do not establish native/browser runtime
acceptance. Preserve the outstanding qualification caveats in
[Application event SDK](application-event-sdk.md).

The current installer and Rust maintenance commands extend this workflow; see
[Installation and recovery](installation-recovery.md). The October 15 source
adds both cursor observer DLLs to the eight ProtoInput runtime components,
executable-relative runtime discovery and data-root-relative GUI defaults.
Historical evidence below qualifies only its recorded earlier snapshot.

Current bundles use the ProtoInputMultiPlex app name and
`protoinputmultiplex.exe`, the `protoinputmultiplex` Cargo package, and
architecture-specific `protoinputmultiplex_cursor_observer32.dll` and
`protoinputmultiplex_cursor_observer64.dll` helpers.
Historical archive names, executable paths and hashes below refer to their
original ProtoInputMux artifacts.

Build from the isolated source snapshot (or a complete checkout with the pinned
ProtoInput submodule and committed local patches). Windows x64 is the host;
the native payload supports x64 and x86 targets. Use Visual Studio 2022 C++ build
tools, Windows SDK, ATL, Rust and Python 3.11+. Existing ATL setup is
`scripts/prepare-atl.ps1` when ATL is not installed. The existing backend script
applies the isolated compatibility patches and builds both architectures.
The packaging script first runs `build-backend.ps1 -PrepareOnly` to apply the
existing compatibility patches, then freezes inputs and rejects source changes
during compilation. The task snapshot already has those patches. Additional
feature patches in subdirectories must be applied as required by their own docs;
the recorded task baseline includes its existing feature patches unchanged.

```powershell
Set-Location 'E:\Kenny\Documents\GitHub\ProtoInputMultiPlex'
.\scripts\package-portable.ps1 `
  -CargoTargetDir 'E:\Kenny\Documents\GitHub\ProtoInputMultiPlex-portable-target' `
  -OutputDir 'E:\Kenny\Documents\GitHub\ProtoInputMultiPlex-portable-dist' `
  -WasmBindgenArchive 'E:\tools\wasm-bindgen-0.2.129-x86_64-pc-windows-msvc.tar.gz'
python .\scripts\test-portable.py `
  --bundle 'E:\Kenny\Documents\GitHub\ProtoInputMultiPlex-portable-dist' `
  --output 'E:\Kenny\Documents\GitHub\ProtoInputMultiPlex-portable-verification.json'
Compress-Archive -LiteralPath 'E:\Kenny\Documents\GitHub\ProtoInputMultiPlex-portable-dist' `
  -DestinationPath 'E:\Kenny\Documents\GitHub\ProtoInputMultiPlex-portable.zip'
```

Always choose a new output directory. The script refuses overwrites and refuses
the shared `E:\.cargo\target` (including children). It restores its process
environment when finished. Rust uses `--locked`, an explicit x64 target, release
mode and a static CRT. The native build uses existing Release conventions. No
product Rust source, UI, routing, permission or startup-registration changes are
needed. Python staging can also be rerun on already-built outputs:

```powershell
$env:CARGO_TARGET_DIR = 'E:\Kenny\Documents\GitHub\ProtoInputMultiPlex-portable-target'
python .\scripts\package-portable.py --target-dir $env:CARGO_TARGET_DIR --output 'E:\new-bundle' --build-inputs "$env:CARGO_TARGET_DIR/portable-build-inputs.json"
```

This is repeatable staging with recorded hashes, not a byte-identical MSVC build
claim. Build toolchain versions, source hashes, lockfile and runtime hashes are in
the manifest; rebuild with the same tools and inputs.
The PowerShell workflow records source hashes before compilation and checks them
again before staging. Python-only staging requires the same frozen input record
and SDK metadata from the builder. Arbitrary prebuilt SDK outputs are rejected.
Staging requires the local
Cargo dependency cache populated by the build. Unknown non-system PE imports
fail packaging. Import and delay-import tables are read from every final EXE/DLL;
dynamic ProtoInput filenames are the audited eight-file runtime contract. Each
binary's architecture and colocated dependencies are checked. Doctor resolves
the loader's required exports. Dynamic loading inside arbitrary target processes
still requires separate runtime/routing acceptance.

The bundle contains the app, eight native runtime files, launch scripts,
instructions, root notices, native and Rust dependency notices, Rust toolchain
notices and a corresponding-source ZIP. The ZIP preserves native source headers,
project files, patches, build scripts, application source and lockfile. Upstream
OpenXinput contains GPL-3.0-or-later headers and nested BlackBone code includes
LGPL notices; the ProtoInput MIT notice does not replace them. This records the
source facts and retained notices without asserting external redistribution
clearance. Rust notice collection conservatively includes the filtered Cargo
resolution graph, which may contain build/development dependencies too.

Launch scripts anchor CWD to the extracted folder. GUI runtime defaults then find
`runtime`, while saved profiles/macros remain in `profiles`/`macros`. Both scripts
choose `data/app-preferences.json`; direct EXE invocation retains existing
LocalAppData defaults. Data directories are initially empty and should be kept
when updating files manually. Wrappers forward optional CLI arguments and add no
MCP write permission. Preferences are written only after an explicit save or
authorized MCP configure operation.

`test-portable.py` copies to a clean disposable directory with spaces and uses
an unrelated CWD plus Windows-only PATH. It checks hashes, CLI and MCP startup,
DLL-free discovery, missing-runtime errors and preferences isolation. It never
starts routing or sends input. GUI smoke checks need coordination with the main
session's native test slot and disposable preferences with no shortcuts. GUI
launch, loader checks and actual routing acceptance must be reported separately.
After the main session grants the native slot, the focused GUI check is:

```powershell
python .\scripts\test-portable-gui.py `
  --bundle 'E:\Kenny\Documents\GitHub\ProtoInputMultiPlex-portable-dist' `
  --output 'E:\Kenny\Documents\GitHub\ProtoInputMultiPlex-portable-gui-verification'
```

It launches the GUI wrapper from unrelated CWD with sanitized PATH, once with
runtime and once without, using disposable preferences with no shortcuts and a
disposable startup test-key path. It sends no pointer/key events, records only
the app's own egui screenshot/state, then closes the exact owned window. No
routing, backend attachment, startup setting, or acceptance of real targets is
part of this test.

For this task, `source-baseline.json` records the original main checkout's file
hashes, unborn HEAD status, pinned ProtoInput commit and submodule modifications.
The per-file baseline remains authoritative even when HEAD is absent. The main
checkout and `E:\.cargo\target` are not packaging output locations. See the separate
verification record for exact tested bundle and limitations.

## Subsequent actual-routing validation

The unchanged portable ZIP was subsequently tested through its bundled MCP
launcher against owned x64 and x86 probe targets using virtual devices only.
See `portable-routing-verification.json` and the separate raw evidence record.
This is bounded ordinary-probe routing acceptance, not general application or
hardware-isolation validation. The original packaging/launch report remains a
historical record of the earlier check that did not start routing.

Historical invocation below is retained for provenance. For a fresh check, use
current artifacts and executable names and reserve the main session's native slot:

```powershell
python .\scripts\test-portable-routing.py `
  --archive 'E:\Kenny\Documents\GitHub\ProtoInputMux-0.1.0-windows-x64-portable.zip' `
  --x86-probe 'E:\.cargo\target\i686-pc-windows-msvc\debug\protoinputmux.exe' `
  --x86-sha256 a92fe912958b69c8c573e11cb320e7d2e326f01a09f81e7b45a165c13db10f52 `
  --output 'E:\new-portable-routing-evidence'
```

The x86 executable is copied read-only as a disposable target fixture; it is
neither a bundle dependency nor the controller/runtime under test. Omitting it
tests two x64 targets instead. The runner extracts a clean copy from the ZIP,
checks payload hashes, removes developer PATH entries, and starts from unrelated
CWD. Native hook and loader module file identities and hashes must match the
extracted runtime. Both peers must publish fresh reports for delivery and
no-crossover assertions. Explicit Stop tests the acknowledged shutdown path with
held input; EOF tests observable release, overlay and process cleanup without
claiming an independently exposed fence acknowledgement. Each phase uses fresh
PIDs and finishes by closing all owned probes, workers and injector helpers.

The initial textual DLL-path assertion stopped a first run after successful
selection clicks. The corrected run records Windows `\\?\` module paths and
uses file identity plus packaged hashes. The failed aggregate and its cleanup
remain retained; no executable, native DLL or portable archive was changed.

## Offline OCR model bundle

Distributions containing local OCR must carry `assets/ocr` beside
`protoinputmultiplex.exe`, independently of the eight-file ProtoInput runtime.
Keep all six files: `text-detection-ssfbcj81.rten`,
`text-rec-checkpoint-s52qdbqt.rten`, `MODEL_CARD.md`, `provenance.json`,
`LICENSE-CC-BY-SA-4.0.txt`, and `ATTRIBUTION.md`. These unmodified model weights
are separately licensed CC-BY-SA-4.0; OCR code remains MIT OR Apache-2.0.
The app build script stages this closure into Cargo's profile `assets/ocr`;
portable staging must retain that closure and record its hashes in the bundle
manifest. OCR resolves only the executable-relative bundle, never CWD or a
source tree, and verifies the fixed model lengths and SHA-256 before parsing.
Missing or altered models report unavailable; there is no runtime download.
Preserve the notices in [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md).
Historical portable receipts above do not qualify OCR asset staging or
recognition. See [OCR text contract](ocr-text-contract.md); the [OCR assessment](ocr-text-assessment-2026-11-07.json) records source
checks and a bounded virtual cohort. A complete portable release is not
qualified by those checks.
The small OCR component adapter's MIT terms are retained separately from the
model licenses. The packager verifies the fixed upstream notice and stages it
as `licenses/rust/ocrs-0.12.2/LICENSE-MIT.txt` when the crate has no notice files.
The pure notice-copy check does not qualify a complete portable release.
