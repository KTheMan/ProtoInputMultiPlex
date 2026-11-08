# Product and repository identity

The current product is **ProtoInputMultiPlex**. Its executable, Cargo package,
application source directory, and distributable skill use `protoinputmultiplex`.
The repository's intended canonical location is
`https://github.com/KTheMan/ProtoInputMultiPlex`, maintained with the Git author
identity `KTheMan <k@knnygrdn.com>`.

This rename is a clean break. Current builds do not ship a legacy executable
alias, read the former default settings location, reuse the former startup or
installation registry identity, or accept the former SDK framing field. Existing
profiles can be opened explicitly through the normal profile-file workflow;
the rename does not change their device-interface-path format. Existing program
directories should be preserved and a fresh bundle/install directory selected.

Current canonical identities include:

| Surface | Name |
| --- | --- |
| Cargo package and executable | `protoinputmultiplex` |
| Application source directory | `crates/protoinputmultiplex` |
| Skill name and directory | `protoinputmultiplex`, `skills/protoinputmultiplex` |
| Default user-data directory | `%LOCALAPPDATA%\ProtoInputMultiPlex` |
| SDK framing field | `protoinputmultiplex_sdk` |
| Product environment-variable prefix | `PROTOINPUTMULTIPLEX_` |
| Product IPC/window-class prefix | `ProtoInputMultiPlex` |
| Installation marker | `.protoinputmultiplex-install.json` |
| Rust observer DLLs | `protoinputmultiplex_cursor_observer32.dll`, `protoinputmultiplex_cursor_observer64.dll` |

The native name alignment is isolated and documented in
[`../patches/branding/README.md`](../patches/branding/README.md). The pinned
third-party **ProtoInput** project retains its upstream identity and notices.

## Historical records

Historical acceptance evidence, baseline inventories, changelog-style delivery
records, original archive filenames, and archived patches retain the exact
names and hashes of their original snapshots. This is provenance, not a naming
contract for a current build. In particular,
[`portable-bundle-manifest.sample.json`](portable-bundle-manifest.sample.json)
is the actual manifest from the original portable validation, including its
source commit and file hashes. It is retained byte-for-byte as a historical
record and must not be used as a current packaging template.

The active illustrative format is
[`protoinputmultiplex-bundle-manifest.example.json`](protoinputmultiplex-bundle-manifest.example.json).
Its placeholders are deliberately not installable. Generate real manifests
with `scripts/package-portable.ps1`; never substitute historical hashes.
