# Connected virtual-user contract

This contract describes the connected `virtual_users` implementation. It keeps the existing 23-tool MCP surface and request schema; no new permission or tool is introduced. The [bounded independent assessment](connected-virtual-users-assessment-2026-10-15.json) passed five groups, 18 actual GUI observations, 19 named outcomes and three original frames on three distinct x64 targets. The [producer](connected-virtual-users-rust-checks-2026-10-15.json) passed formatting, strict Clippy, 183 tests and x64/i686 builds; i686 was not executed.

## Inspection and mutation authority

`Inspect` is read-only. Connected `Start` and `Stop` require `session_control` and an explicit `expected_revision` from the current roster. Mutations are available only in automatic desktop routing with a nonempty configured PID allowlist. Every PID in that allowlist and each current selected mouse/keyboard recipient must be covered by Agent Access. All active virtual users must be permitted, and the selected Start source and target must be active and in scope. These checks happen before the operation mutates the roster or graph.

## Start and grant

`Start` appends a stable virtual user and the requested source-to-clone replication edge to the live GUI session. The clone is session-only: it is not saved to stopped profiles, and slots/device paths are not reused during the session. Start does not add the clone to `Agent Access` or grant it input. It appears unchecked in the existing access-policy UI. To use the clone directly or through a graph route, the GUI owner selects it and applies the existing access policy. Applying that policy revokes connected clients; reconnect with the current descriptor and Inspect the current policy before further writes. Until the new ID is granted, direct input and graph paths that reach it fail scope preflight.

## Stop and receipt exception

Normally, `Stop` requires permission for all active users and the same full PID scope. A client may also stop the exact clone that it successfully created while that client is still connected; this receipt permits omitting only that clone from the user-grant check. All other active-user, PID, selected-recipient, session-control, revision, and generated-user checks still apply. No caller can use this exception to stop an arbitrary ungranted clone or an original user.

Successful Stop tombstones the clone, prunes incident graph paths and pending clone-crossing work, and releases that clone's owned input. Device registrations remain reserved until worker shutdown. EOF, disconnect, policy revocation, or a global Agent Access reconfiguration clears the creator receipt but leaves the clone and graph owned by the GUI session. The receipt does not transfer to another client.

## Failure and persistence limits

Rejected preflight leaves the roster, graph, queue, and held input unchanged. Once an authorized native cleanup begins, an error may follow partial cleanup; there is no rollback guarantee. A cleanup error leaves the clone active at the same roster revision and retains the connected creator's receipt. Inspect before retrying. Already completed releases and canceled work remain in effect. [Clone Stop assessment](clone-stop-retry-assessment-2026-11-06.json)

The clone and its incident edges are removed from stopped profiles; edges between original users remain governed by the existing graph workflow. Client disconnect does not remove the live clone. Native scope sampled one Back replication path and ordinary hold preservation; no keyboard/text copy matrix, forced ACK/cleanup races, arbitrary applications or full Ultra/UX parity is claimed. The retired-device scheduled-tap guard has source review only in this group.
