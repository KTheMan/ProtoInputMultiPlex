# Code signing policy

ProtoInputMultiPlex is preparing an application to the free SignPath Foundation
program. Acceptance and trusted release signing are pending. Current development
builds are unsigned; a checksum or successful test is not a trusted signature.

The proposed signing scope is the project's Rust application, Rust cursor helpers
and installer. The modified ProtoInput backend is separately identified upstream
software; no Foundation signature for those binaries is claimed. Eligibility of
the bundled backend and separately licensed OCR models requires provider review.
See [third-party notices](../THIRD_PARTY_NOTICES.md).

Kenneth Gordon ([KTheMan](https://github.com/KTheMan)) is the maintainer and proposed
author, reviewer and release approver. GitHub account MFA has been verified. SignPath enrollment, account MFA and signing configuration have not been qualified. Signing must follow review of the exact
source, build provenance and final release artifacts, with explicit maintainer
approval. Final package hashes must describe the signed bytes.

If accepted, release pages will identify the provider using its required credit:
“Free code signing provided by SignPath.io, certificate by SignPath Foundation”.
That credit does not describe current builds.

See the [privacy policy](privacy.md), [program terms](https://signpath.org/terms)
and [release page](https://github.com/KTheMan/ProtoInputMultiPlex/releases).
