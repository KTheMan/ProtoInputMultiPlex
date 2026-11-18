# Portable EXE packaging provenance

The public unsigned `v0.1.0-rc.1` portable EXE was assembled on a separate Linux
build host from the exact bundle produced by GitHub Actions run
[44208870622](https://github.com/KTheMan/ProtoInputMultiPlex/actions/runs/44208870622).
The application and installer were compiled in hosted CI. The outer portable
wrapper has this separate packaging provenance.

The [NSIS recipe](../packaging/portable-exe.nsi) and its
[761-member verification include](../packaging/portable-exe-rc.1-members.nsh)
are byte-identical to the inputs used for that wrapper. Their SHA-256 values are
`df919e7e3476583e8f9aa165f03811869ef84f4d36dcd18b07289c2e2db9a516` and
`826ec620d4fcabe99791d7df861dcd3ddeff6ba3bbd52c29e64e70d06e6d57ed`.
The verification include is specific to the RC bundle manifest with SHA-256
`b4ca9106f2cf2346a2399b556e7fd69365684859f674a65f3f86730529107bc6`. It checks extracted member lengths.
The separate qualification inventory verified every member's SHA-256 before
launch and is recorded in the [assessment](vm105-rc-assessment-2026-11-17.json).

Packaging used NSIS `v3.08-3+deb12u1` from Debian packages extracted into
an isolated tool directory. Package SHA-256 values were
`b9ca8de84341753dd1c071a3f65453dc06bd7b6f2230140a36755d7e402827c2` for `nsis` and
`f1c9e63389c947442fddd5ca446e22966e8f03fc10663f998cf7df58642f9b52` for `nsis-common`.
With that compiler, its matching NSIS resources, and the exact qualified bundle,
the compiler invocation was equivalent to the following command.

```text
makensis -V2 -DBUNDLE_DIR=/path/to/qualified-bundle -DOUTPUT_FILE=/path/to/ProtoInputMultiPlex-0.1.0-windows-x64-portable.exe -DVERIFY_INCLUDE=/path/to/portable-exe-rc.1-members.nsh /path/to/portable-exe.nsi
```

The resulting unsigned portable EXE SHA-256 is
`4df3f3fcb69367c774569ab8f4a450a32556bcdff5f5dac3964baf9331c3518e`. Exact compiler and payload inputs matter.
SignPath integration must build the wrapper in the provider-verifiable hosted
pipeline before requesting a Foundation signature. The published RC remains an
unsigned, independently qualified artifact while enrollment is pending.

The proposed signing scope covers project-owned binaries and package wrappers.
Upstream ProtoInput runtime images are included unsigned. Bundled offline OCR
weights use CC-BY-SA-4.0 and require provider eligibility review. See the
[code signing policy](code-signing-policy.md), [privacy policy](privacy.md) and
[third-party notices](../THIRD_PARTY_NOTICES.md).
