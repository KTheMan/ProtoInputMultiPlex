# Installation, upgrades and recovery

Use a complete distribution built for the current release. The October 14 portable evidence and October 15 distribution-verification record describe their own historical source and artifacts; they do not qualify this release candidate. The current bounded source/build qualification is summarized in [Windows version metadata](windows-version-metadata.md). It covers Rust checks and debug-build metadata only. The launch and distribution details below describe the previously tested staged distribution, not a qualified current release. Packaging, release binaries, installer execution, signing, login startup, reboot behavior and product-runtime acceptance remain separate gates.
Unsigned Windows x64 builds may produce a Windows reputation warning. The GUI
also requires a working Windows OpenGL display driver. No development checkout,
Python, Rust, Visual Studio or separate VC redistributable is needed to launch
the staged static-CRT distribution on the tested Windows host.

## Windows setup

Run `ProtoInputMultiPlex-0.1.0-setup.exe`. Setup installs for the current Windows user,
defaults to `%LOCALAPPDATA%\Programs\ProtoInputMultiPlex` for a fresh install,
and adds Start menu and Installed apps entries. Subsequent installs under this
product identity reuse their registered installation directory during upgrades.
It does not request elevation or enable login startup.
Close the GUI and every agent client before upgrading or uninstalling. The
native backend can remain loaded in applications previously used for routing;
close those applications too if setup reports a locked DLL. The Rust maintenance command names a locked file; setup shows a general
failure message. Both preserve the existing payload on failure. The
default user-data directory is `%LOCALAPPDATA%\ProtoInputMultiPlex`, separate from
program files. Application data, startup registration, installation markers,
and shell labels all use the ProtoInputMultiPlex identity. This revision starts a
new installation and configuration namespace. Earlier installations are not
upgraded in place and no compatibility executable alias is supplied. Preserve
their data until you explicitly import the profiles/macros you need, and update
your MCP executable paths and opt-in startup registration for the new install.
GUI profiles and macros now default beneath that data directory,
even when started from a shortcut or an unrelated working directory.

Re-run setup for an upgrade. The Rust engine verifies every manifest file and
its hash before publication, stages a complete payload, retains replaced
program releases, and swaps the active `app` directory. Versions can be installed
again; each successful replacement has a distinct retained directory. Changed
or unexpected program files cause a conservative failure. Preserve those files
and choose a fresh installation directory rather than deleting user files to
force an upgrade. Retained releases consume disk until uninstall.

Uninstall through Installed apps or `Uninstall.exe`. It removes owned program
payloads and setup's shell entries. User data, unknown files, backup trees and
the installation ownership/lock files remain. Historical portable data inside
`app/data`, `app/profiles` and `app/macros` is retained under `preserved-data`.
There is no implicit data-erasure option. Removing user data is a separate,
explicit manual action after selecting and backing up the intended directory.

Login startup remains an explicit local GUI preference. Disable it in the GUI
before uninstalling or moving an opted-in installation. Setup and offline
maintenance preserve the preference and do not change the Windows Run key.
A startup registration uses its saved absolute executable path. After a move,
use the GUI to opt out and opt in at the new path. Actual login/reboot
behavior has separate validation requirements.

For disposable/silent deployment checks, `setup.exe /S /NoShellIntegration
/D=C:\Disposable Root With Spaces` omits shell registration and shortcuts.
`/D=` must be last. Setup preserves the first installation's shell integration
mode on later upgrades. This test switch never changes startup registration.

## Portable and relocated launch

Extract the full bundle into a writable directory and use `launch-gui.cmd` or
`launch-mcp.cmd`. These wrappers select `data/app-preferences.json`; current GUI
profiles and macros live under `data/profiles` and `data/macros`. Preserve the
whole `data` directory. Migrate an old bundle explicitly as described below to
import its historical sibling `profiles` and `macros` directories.

The EXE resolves its bundled native runtime relative to the EXE, independent of
the caller's working directory. The distribution includes both 32/64-bit
ProtoInput runtime families and both cursor observers. Workers and the emergency
Stop broker reuse the EXE. Direct EXE launch uses LocalAppData by default:

```powershell
& 'C:\Apps With Spaces\ProtoInputMultiPlex\protoinputmultiplex.exe' gui
& 'C:\Apps With Spaces\ProtoInputMultiPlex\protoinputmultiplex.exe' gui --data-dir 'D:\My Input Data'
```

The parent of a chosen data directory must exist. A shared sibling lock prevents
migration/restore while GUI/MCP instances using that root are active. Explicit
`--preferences C:\path\app-preferences.json` remains supported; its parent is
also the root for default GUI profiles/macros. `--data-dir` and `--preferences`
are alternatives. An explicit document whose parent does not yet exist retains
legacy read-only discovery behavior without creating directories; that custom
document is outside the managed data-root maintenance lock contract for this
process. Close such clients before maintenance. No default launch starts routing
or grants agent writes.

Copying only the EXE is insufficient. Keep `bundle-manifest.json`, `runtime`,
notices and corresponding source together. Relocate with all processes stopped.
For an installed tree, move the whole installation root, including its ownership
marker; old Start menu/Installed apps paths require setup at the chosen location
or deliberate shortcut repair. Private GUI descriptors expire with their owning
process and must be obtained again after relaunch.

## Agents

Configure an MCP client with the actual executable path and argument array:

```json
{
  "command": "C:\\Apps With Spaces\\ProtoInputMultiPlex\\app\\protoinputmultiplex.exe",
  "args": ["mcp", "--data-dir", "D:\\My Input Data"]
}
```

For a portable bundle omit the `app` path component. No shell or working
directory is required. `--runtime` remains an explicit override. Standalone
preference inspection needs no write grant; writes still require
`--allow-app-preferences`. A live GUI connection instead uses `mcp
--gui-connection C:\private\descriptor.json`, with the GUI's existing grants.
Keep that descriptor private and never copy it into backups or client examples.
See `docs/agents.md` in the corresponding-source archive for routing scope.

## Configuration migration, backup and restore

Close instances using the selected data root. Commands below operate only on
explicit paths, write JSON results, and never change startup registration.
Create backup parent directories first; every backup destination must be fresh
and disjoint from its data root and other inputs.

```powershell
$exe = 'C:\Apps With Spaces\ProtoInputMultiPlex\app\protoinputmultiplex.exe'
& $exe config inspect --data-dir 'D:\My Input Data'
& $exe config migrate --data-dir 'D:\My Input Data' --backup-dir 'D:\Backups\Before Migration'
& $exe config backup --data-dir 'D:\My Input Data' --backup-dir 'D:\Backups\Known Good'
& $exe config restore --data-dir 'D:\My Input Data' --backup 'D:\Backups\Known Good' --backup-dir 'D:\Backups\Before Restore'
```

Migration upgrades the data-container schema to 2, validates independent
version-1 preference/profile/macro documents, materializes legacy profile IDs,
and preserves existing grants, startup opt-in and unknown user files. Future
schemas and malformed documents fail before publication. Legacy portable import
copies only data/profiles/macros, leaving its source unchanged. Choose an empty
destination outside the old bundle (for example a new bundle's empty `data`
directory); import does not merge over existing data:

```powershell
& $exe config migrate --data-dir 'D:\My Input Data' --legacy-root 'D:\Old Portable Bundle' --backup-dir 'D:\Backups\Before Import'
```

Backups are directories with a checksummed inventory and payload. They preserve
file bytes and empty directories, including unknown files and corrupt current
data, while excluding private GUI connection descriptors. They do not preserve
ACLs, owner or timestamps. Treat backups as sensitive user data and choose a
private destination. Limits are 32 MiB per file, 256 MiB total, 4096 entries and
32 directory levels. Symlinks/reparse points, path aliases, traversal and
overlapping roots are rejected. Checksums detect corruption; they do not
authenticate an untrusted backup or distribution.

Select the user-data directory, such as `app/data`, rather than an installation
root or portable program directory. Configuration commands reject recognized
program roots, and restore rejects backups containing a program-root signature,
before creating a recovery backup or changing data. Use `--legacy-root` for an
old portable bundle.

Restore validates the entire backup before changing current data, takes a fresh
recovery backup even when current files are corrupt, then publishes a staged
tree. Migration/restore retain the original directory beside the data root,
including its original metadata and private descriptors, until you explicitly
remove it. A failed publication attempts rollback. If interrupted, GUI/MCP launch
fails with a recovery instruction:

```powershell
& $exe config recover --data-dir 'D:\My Input Data'
```

Recovery rolls back to the original tree and retains any staged candidate. Keep
the journal, original and staged trees together until recovery succeeds. A
malformed or unsupported preferences document is deliberately preserved; use a
known-good whole-data backup and `config restore`, or select a fresh data root
and explicitly import validated documents. Existing per-profile `.bak` workflows
remain available. Backups with invalid/future known documents can be retained
for diagnosis but are not accepted for normal restore.

## Program recovery

Use the EXE from an extracted complete bundle or setup's retained
`maintenance.exe`, outside the active `app` directory. `installation inspect`
checks the installed payload. An interrupted install must be recovered before
launch; a successfully installed but unsuitable version can be rolled back
explicitly:

```powershell
& 'D:\Clean Bundle\protoinputmultiplex.exe' installation inspect --root 'C:\Apps With Spaces\ProtoInputMultiPlex'
& 'D:\Clean Bundle\protoinputmultiplex.exe' installation recover --root 'C:\Apps With Spaces\ProtoInputMultiPlex'
& 'D:\Clean Bundle\protoinputmultiplex.exe' installation rollback --root 'C:\Apps With Spaces\ProtoInputMultiPlex'
```

`recover` is a no-op without an interrupted transaction. `rollback` selects the
latest retained successful release and carries portable data forward. Neither
operation silently deletes corrupted or unexpected program files. For damaged
program binaries, retain the damaged root, install into a fresh root, and select
or restore the preserved user data. An interrupted uninstall is completed by
retrying uninstall. Do not manually remove ownership/journal files to bypass
checks. Normal maintenance is per-user; concurrent elevated/hostile file edits,
power-loss durability on every filesystem and general-device routing are outside
the bounded qualification.

## Reproducing a distribution

Freeze current working files, including uncommitted and untracked source:

```powershell
python .\scripts\freeze-distribution-source.py --output 'E:\Build\Current Frozen Source'
Set-Location 'E:\Build\Current Frozen Source'
# Install ATL with scripts/prepare-atl.ps1 if the C++ toolchain lacks it.
.\scripts\package-portable.ps1 -CargoTargetDir 'E:\Build\Isolated Cargo Target' -OutputDir 'E:\Build\Fresh Portable Bundle'
.\scripts\package-installer.ps1 -BundleDir 'E:\Build\Fresh Portable Bundle' -OutputFile 'E:\Build\ProtoInputMultiPlex-0.1.0-setup.exe'
```

Use the pinned Rust/Ouroboros/native dependencies and the recorded toolchain.
To rebuild from `source/corresponding-source.zip`, extract it to a fresh source
directory, run `git init`, `git add -f .`, and create a local snapshot commit
before invoking the build scripts. The native source is already flattened and
patched; do not replace it with an unpatched submodule checkout. Install the
documented build tools/ATL and populate the locked Cargo dependency cache.
NSIS 3 is needed only to create setup. The existing portable workflow prepares
the separate compatibility patches, freezes inputs, builds static-CRT Rust and
both native architectures, checks PE imports, collects notices/source and
rejects source drift. It never uses the shared Cargo target. New staging/output
paths must be fresh. The manifest records sources, tool versions and all payload
hashes. This is reproducible build/staging procedure and verified input identity,
not a claim of byte-identical MSVC or NSIS output across machines/toolchains.
