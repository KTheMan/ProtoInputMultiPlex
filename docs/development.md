# Development

The published release identifies its exact source commit and build recipe.
Use that source identity when investigating behavior specific to the RC.

## Development process

Implementation and documentation are developed with substantial LLM assistance.
The maintainer directs the project and approves releases. Tests and separate
review agents produce the linked verification records, which describe their
exact source, artifacts, observations and remaining limits.

ProtoInput supplies upstream native hooking and injection. The Rust application,
GUI, routing controls and integrations are maintained here, with third-party
provenance and licenses retained in the notices.

## Build from source

Requires Windows x64, Rust 1.92 or newer, and Visual Studio C++ build tools with a Windows SDK. The interface uses [Ouroboros UI](https://github.com/Type-zero-labs/ouroboros-ui), pinned to the same revision as OuroForge, on egui/eframe 0.34.

```powershell
git submodule update --init --recursive
cargo run -p protoinputmultiplex
```

## ProtoInput backend

Upstream is a Git submodule at `vendor/ProtoInput`, pinned to `dd29bf8d22349605d0e1950c1c5dc4893d72670e`. Its existing C++ implementation remains separate. All new application features here are Rust.

```powershell
./scripts/build-backend.ps1
cargo run -p protoinputmultiplex -- doctor --runtime runtime
cargo run -p protoinputmultiplex -- probe
```

`doctor` loads the x64 loader and resolves required exports, without injection. In the UI, add a user, pair a keyboard, then choose an application under **Output**. **Open routing test window** creates and selects a disposable target. **Start routing** checks assignments and runtime files before injecting. Each user needs a distinct process because upstream has one input state per process. For direct CLI and physical/legacy sessions, restart target applications before attaching again. Only eligible virtual-only supervised sessions can reuse a cleanly retired target; see the [clean-restart contract](clean-restart-contract.md). The resident DLL is not unloaded, and upstream still has no complete detach/unload API or removal API for selected devices. The UI reports configuration requests, not a hook-installation acknowledgement.

The Rust supervisor keeps native waits off the UI thread, times out startup/control operations, and owns workers/injector children in a kill-on-close Windows Job. A local backend patch removes hooks on pipe disconnection. Cleanup is best effort; it does not unload the DLL or fully restore a target's Raw Input registrations. The experimental `attach <profile>` CLI remains available but does not use the GUI supervisor.

Runtime files include `ProtoInputLoader64.dll`, `ProtoInputHooks64.dll`, `ProtoInputIJ64.exe`, and `ProtoInputIJP64.dll`; x86 targets additionally require their 32-bit hooks/injector/proxy. EasyHook is statically linked by this upstream revision. Keep any additional build-produced runtime DLLs together. The script stages output under ignored `runtime/`.

The backend requires ATL in addition to C++ tools. If ATL is absent, `./scripts/prepare-atl.ps1` can extract the MSVC 14.44 ATL packages from Microsoft downloads verified against the installed Visual Studio catalog. Build compatibility overrides and reproducible patches are documented in [backend build notes](backend.md). Both native architectures now build locally with v143 and the installed Windows SDK. Rust preview and profile editing work independently of the native binaries.


## Checks and workspace

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

- `mux-core`: profile validation, stable device identity resolution, pixel-space motion transforms and per-user cursor settings; portable and unit tested.
- `mux-windows`: Raw Input enumeration/capture, transparent desktop overlays, DPI/window geometry, dynamically loaded ProtoInput C ABI, session cleanup.
- `protoinputmultiplex`: Ouroboros/egui dashboard, routing supervisor/worker, disposable test target, and CLI.

See [the roadmap](roadmap.md) for the next milestones and their acceptance criteria. Upstream license and dependency notices remain in the submodule; see [third-party notices](../THIRD_PARTY_NOTICES.md).


## Clean-room implementation

ProtoInputMultiPlex follows a clean-room development policy. Public documentation
and published screenshots inform its functional and UI workflow goals. Development
and testing must not use or run MouseMux itself, inspect or decompile its binaries,
incorporate its source code or assets, or rely on other MouseMux cloning or
reimplementation efforts. New application behavior and UI use independently
authored Rust code, branding, visuals and wording.

The separately licensed ProtoInput backend, Ouroboros UI framework and other
declared open-source dependencies are used with attribution. See
[third-party notices](../THIRD_PARTY_NOTICES.md) for their provenance and licenses.


## Documentation and evidence

Use the [documentation index](README.md) to find user guides, feature contracts,
SDK references and validation records. The [README history](history/readme-2026-11-17.md)
preserves the former front page's full development notes, source/test counts and
bounded acceptance descriptions.
