# Unsigned, unreleased candidate build

`.github/workflows/release-candidate.yml` is an unsigned build foundation. It
creates GitHub Actions review artifacts; it does not publish a GitHub Release.
Artifact access follows repository visibility. The authorized publication
repository is public: these sanitized candidate deliverables may be public.

The workflow checks out the triggering event's exact `github.sha`, recursively
checks out the pinned ProtoInput submodule, disables persisted checkout
credentials, and uses `contents: read`. Record the integrated commit and run ID.
No arbitrary branch, artifact, URL or executable input is accepted. There is no
signing environment, secret, certificate or account setup.

## First launch before merging the draft pull request

[GitHub's workflow_dispatch documentation](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#workflow_dispatch)
requires the workflow file to exist on the default branch to receive the manual
event. A new file on this feature branch must not rely on first-time manual
dispatch for its pre-merge qualification. After the current full Rust run has
reached a terminal result and the root has accepted the independently reviewed
v2 source, the root may introduce the workflow and documentation in a commit
on `sdk/application-events-20261108` and push that commit. The narrow bootstrap
trigger requires both this exact branch and a change to
`.github/workflows/release-candidate.yml`; it builds that push's exact SHA.
It does not suppress other workflows. Later unrelated Rust pushes do not match
the path filter, while future changes to this workflow on that branch can trigger
another candidate build. `workflow_dispatch` is retained for later use after
GitHub registration/default-branch requirements are satisfied; first launch uses
the narrow push trigger. No branch, push or dispatch is performed by this source
overlay. A green candidate build remains a build/staging result only.

The two jobs use separate standard `windows-2022` hosted runners. The bounded
source-check job runs existing pure packaging rejection checks and actual Git
patch/header inventory checks. That check intentionally leaves a private-source
rejection probe; its checkout is never used to build. The candidate job starts
from another fresh exact checkout. It limits Cargo to two build jobs, disables
incremental compilation and the compiler wrapper, sets a 180-minute timeout,
and allocates a fresh run/attempt-owned directory under `RUNNER_TEMP`. No target
or binary cache is restored. Existing runtime/backend outputs cause rejection.

## Prerequisites and exact builder

Use the repository's Rust 1.92.0 pin, Windows x64 host, i686 Windows target and
wasm32 target. The existing recipe requires VS2022/v143, Windows SDK, ATL headers
and x64/x86 libraries, Python 3.11+ and NSIS 3. The official
[Windows 2022 runner image manifest](https://github.com/actions/runner-images/blob/main/images/windows/Windows2022-Readme.md)
was read on 2026-11-08: image 20261101.353.1 lists VS2022, VC ATL/ATLMFC, x86/x64
C++ tools, Windows SDKs, Python and NSIS 3.10. The workflow checks actual installed
ATL/NSIS before compiling and records selected tool versions. It uses existing
installed tools, without `prepare-atl.ps1`, Chocolatey installation, driver
installation or another download-and-run helper. The hosted image is mutable;
its future concrete compatibility remains a build qualification item.

The only browser-binding download is official wasm-bindgen API asset 587769522,
SHA-256 `79e348c169d0c10c0b287647f1e753d0985353ca96800bc29be8f4261baf13e6`.
The workflow checks its digest before passing it to the accepted packager, which
rehashes the copied archive, extracts it and checks version 0.2.129. Cargo/rustup
and the existing pinned Cargo dependency/git resolution retain their normal
network semantics; `--locked` does not claim an offline or fully hermetic build.

On the future hosted runner, the exact build entry points are:

```powershell
./scripts/package-portable.ps1 -CargoTargetDir "$root/cargo-target" -OutputDir "$root/portable" -WasmBindgenArchive "$root/wasm-bindgen.tar.gz"
./scripts/package-installer.ps1 -BundleDir "$root/portable" -OutputFile "$root/review/ProtoInputMultiPlex-unsigned-unreleased-setup.exe" -MakeNsis $installedMakeNsis
```

The portable builder prepares only the existing approved `0001`, `0004`, `0005`,
`0006`, `0007` compatibility sequence and calls the existing v143 backend builder
for x64/x86. No vendor change is introduced here. It builds the x64 Rust app,
x64/x86 Rust observer DLLs, full-default/native-client SDK DLLs and matching
generated headers for x64/x86, and the web WASM/JS/types/example. The source ZIP
retains committed SDK examples; native qualification fixture EXEs remain excluded
by the accepted payload policy. There is no x86 GUI executable in this x64
portable product. No unsupported new payload is silently added.

The accepted packager freezes committed root/native HEAD inventory plus all five
final patch-created headers, using isolated Git-index/object replay. Its source
admission, native DLL/header Cargo JSON pairing, notices across all SDK targets,
OCR closure, source ZIP, per-path architecture/import/delay-import checks and
post-copy hashes are unchanged. Existing installer packaging embeds this complete
portable tree, including SDK paths. The installer is compiled but never run.

## Review artifacts and qualification limits

Only a complete portable ZIP, unsigned installer, bundle manifest, exact workflow
copy, selected nonsecret tool version record, explicit candidate status and
SHA-256 inventory are uploaded, with seven-day retention. No worktree, Cargo
cache, target tree, raw environment, raw diagnostic logs, profiles, credentials
or runtime connection descriptors are selected. Upload runs only after every
build/staging step succeeds. Current source ZIP workflow admission is unchanged;
this workflow is retained separately as `build-recipe.yml` in the review artifact.

Every output is `UNSIGNED_UNRELEASED_BUILD_ONLY`. Packaging does not launch the
product, run `test-portable.py`, install/uninstall, attach a target, send input,
exercise a driver or reboot. Build/import checks do not establish GUI or native
or browser SDK runtime acceptance, real-device compatibility, reboot acceptance,
legal redistribution clearance, reproducibility, signing or MouseMux Ultra parity.
Current hosted Rust/SDK qualification and the later coordinated GUI/runtime,
signing/release and real reboot work remain separate prerequisites. This source
workflow has not been executed or accepted as a successful candidate build.
