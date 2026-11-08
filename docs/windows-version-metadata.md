# Windows version metadata

The Rust build embeds Windows version resources in the ProtoInputMultiPlex application executable and cursor-observer DLL. These resources describe this project's Rust binaries; the separate ProtoInput native backend is not changed by this step.

## Build requirements

- Windows targets must use the MSVC toolchain. Windows GNU targets fail with a clear build error. Non-Windows targets skip Windows resource compilation.
- Install a Windows SDK that provides rc.exe. Put it on PATH, or set PROTOINPUTMULTIPLEX_RC to the resource compiler executable before invoking Cargo.
- Cargo's package version supplies the textual version strings. Major, minor and patch components must each parse as an unsigned 16-bit number. The embedded numeric file/product version uses those three components plus a fourth component of zero.
- The build emits resources only for the application executable and cursor observer. The native ProtoInput backend is outside this metadata change.

For a normal Windows MSVC debug build, use the repository's standard Cargo build commands after installing the Rust targets and Windows C++/SDK toolchain described in the main setup guide. No runtime launch is needed to compile the resource.

## Bounded qualification

The accepted November 7 source group contains three Rust files within the frozen 162-input source inventory. Formatting, the standalone helper format check, strict Clippy and workspace tests passed: 955 passed, 0 failed, 19 ignored. Fresh x64 and i686 MSVC debug builds completed for the application and cursor observer. Windows version fields were read back for all four artifacts, and COFF headers confirmed machine type and executable-versus-DLL bit.

This is debug build and resource-metadata evidence only. It does not qualify a release package, static CRT/import closure, installer, signature, fixed-resource flags/OS/Translation, product-runtime behavior, login startup, reboot behavior or broad Windows compatibility. The exact independent acceptance record and its pinned verification reference are maintained with the release evidence; these are bounded source facts, not a general release approval.