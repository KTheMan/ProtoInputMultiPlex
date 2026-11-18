# Known limits and reporting problems

## Release scope

The unsigned Windows x64 software DX12 RC passed recorded VM105 acceptance
for portable extraction/launch, default per-user install/uninstall, six Close/Quit
cases per package and a 54-barrier two-seat GUI workflow per package. Input came
from virtual mouse and keyboard pairs and reached two owned target windows.
Mode changes, seat A keyboard suppression, target reuse after Stop and cleanup
were checked. See the [qualification summary](https://github.com/KTheMan/ProtoInputMultiPlex/releases/download/v0.1.0-rc.1/qualification.json)
for exact package/runtime hashes, checks and evidence hashes.

Separate review agents inspected the recorded VM evidence. The acceptance
describes those bounded tests and review records.

## Compatibility and control

- Automatic concurrent routing currently accepts app-local virtual devices.
  Physical-device discovery and assignment exist, while physical/mixed-device
  operation and hot-plug behavior need their own hardware validation.
- A target process has one native input state. Give independent users separate
  processes. Desktop-wide and same-process multi-user isolation remain open.
- Normal desktop input remains active and can interact with foreground apps.
- Attachment installs hooks in the selected process. A runtime DLL can remain
  loaded until that process exits. Use controlled test applications while
  evaluating compatibility.
- Target ownership, held input and cleanup have explicit rules. Inspect recovery
  status after a failure before attempting further input. See
  [failed cleanup recovery](failed-cleanup-recovery-contract.md).
- Arbitrary applications, real logon/reboot, i686 controllers, mixed-DPI and
  display-driver combinations, simultaneous arbitration and full Ultra/UX
  parity remain outside the current RC acceptance.
- The historical intermittent GUI stall remains open. The RC's successful
  shutdown tests have their own recorded scope.
- Native/web SDK integration is under development. The additional
  [tag-triggered hosted run](https://github.com/KTheMan/ProtoInputMultiPlex/actions/runs/44221162502)
  failed its GUI-owned-window SDK gate during GUI preparation, before native
  window qualification started. The failed receipt is preserved. This leaves
  additional SDK qualification work alongside the earlier accepted bounded
  GUI routing and package tests.

## Report a problem

Open a [GitHub issue](https://github.com/KTheMan/ProtoInputMultiPlex/issues) with the release version, Windows version,
portable or installed launch path, routing mode, number of virtual users, target
application and whether it is a separate process per user. Include the actions
that led to the problem, what you expected, and what happened.

For a startup or shutdown issue, include whether the GUI reached Home, whether
Start had been used, and which Close/Quit action you used. If practical, reproduce
with the built-in test windows and describe that result separately.

Review screenshots and logs for private paths, window titles, document contents
and credentials before sharing. GUI connection descriptors contain credentials
and belong in private storage. The [privacy policy](privacy.md) describes local
data and operator-connected clients.
