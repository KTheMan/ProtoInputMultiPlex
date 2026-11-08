# Connected stable-ID routing graph contract

Connected `routing_graph` Apply/Cancel passed a [bounded independent assessment](connected-graph-assessment-2026-10-15.json): five groups, 21 accepted prefix observations plus a focused 12-observation fresh Apply-to-Save continuation, three original frames and two x64 targets per phase. The original navigation failure and raw continuation status remain unchanged. The [producer](connected-graph-rust-checks-2026-10-15.json) passed formatting, strict Clippy, 176 workspace tests and x64/i686 builds. The existing 23-tool schema is unchanged; i686 was not executed.

## Permissions and revisions

`routing_graph` Inspect is read-only and does not require `session_control`. Connected Apply and Cancel require an enabled current client, the current session-change grant, automatic desktop routing, and authorization for every active virtual user. The GUI session's automatic desktop PID allowlist must be nonempty, and Agent Access must cover that entire list. Any currently selected mouse or keyboard recipient is checked against both scopes before forwarding. Out-of-scope selected recipients and routing-query errors reject the request. A user without a selected target is allowed; Apply does not require every user to have focus. This is whole-session permission because Apply replaces the shared graph and releases held session input, while Cancel clears the shared replication queue.

Connected Apply and Cancel must supply `expected_revision` from Inspect. The worker remains authoritative and rejects stale revisions. Standalone calls keep optional-revision compatibility. Legacy numeric `routing_replication` and unrelated Automation mutation commands remain unavailable through a GUI connection.

## Apply and Cancel effects

Apply validates the candidate graph and requires recording/playback lanes to be idle before cleanup. Permission, invalid-graph, busy-lane, or stale-revision preflight rejection leaves the graph, queue and held input unchanged. On accepted Apply, the existing whole-session graph replacement releases held input, installs the graph, advances the revision and resumes a suspended queue. A native failure during authorized cleanup can occur after some holds have been released while the old graph remains. There is no rollback guarantee; Inspect the resulting state before retrying.

Cancel may run while recording or playback is active. It clears queued copies and replication-owned holds only; ordinary/manual/program input, active programs, graph definition and revision remain. If replication is not suspended, future source actions can replicate under the unchanged graph. Cancel does not resume an overflow-suspended queue; a valid Apply is required to resume it. Cancel is not a global Stop.

A successful graph mutation is session-owned and persists if the editing client disconnects or its policy is revoked. Disconnect still removes only that client's input/program ownership. The correlated Automation acknowledgement reports the resulting graph state. The GUI merges acknowledged external graph state into the applied graph/profile without consuming a local pending request or replacing unsent local edits.

## Limits

This contract adds no tool, grant, per-client graph or virtual-user mutation. Native coverage is one representative delayed Back edge, explicit grants, real held-input release/cancellation, recording preservation, EOF, dirty GUI draft and actual profile Save. Local-pending ACK races and empty/unrestricted scope have deterministic/source coverage; playback-preserving Cancel, generated/retired endpoints, translated recipients and old-held-target combinations were not separately exercised natively. Hardware, broad application compatibility, same-process isolation and full Ultra/UX parity remain unverified.
