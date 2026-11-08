# Mapper screen observations

Pixel Color and Image Match are explicit asynchronous nodes in the typed Input
Mapper. Both require a live `Pulse(true)` trigger from a graph with virtual
source ancestry and a stable `capture_user`; a constant or Apply/Arm never
starts a read. Pixel coordinates and Image Match regions use the selected
client's coordinates. Pixel Color emits red, green, blue and alpha as State
values from one captured frame, followed by `done: Pulse(true)`.

Image Match emits `matched: Hold`, an accepted-match `score: State`, and
`done: Pulse(true)` for both match and no-match completion. A no qualifying
match sets matched false and score zero and does not fabricate a position.
Position is the desktop center of the latest successful match and changes only
when a match qualifies. Gate any action that uses Position with `matched`; a
later no-match can leave the last successful center latched. Analysis or capture
failure is an error/retirement path, never a no-match result.

Image Match templates are PNG configuration, at most 256 KiB encoded and
128 × 128 pixels decoded. The overall mapper definition remains limited to
1 MiB. In the GUI, deliberately copy a template into the selected disarmed
Image Match node; copied PNG bytes become shared persisted graph configuration
and may travel with a saved or exported definition. Live frames, pixels and
match results stay actor-private and are not serialized. MCP
`validate_mapper` checks PNG bytes and graph structure locally; it does not
capture or grant permission. Apply validates and caches templates, requires the
existing mapper mutation grants where applicable, and remains disarmed. Arm
starts no read; only a later accepted trigger can dispatch screen work.

Local GUI capture admission uses the existing active virtual user and explicit
selected allowed PID; it does not require an Agent Access capture checkbox.
Connected MCP requires its separate current capture grant, the requested
capture user and PID scope. Each trigger binds the read to current source
ancestry, the explicit capture user, selected window/watch and downstream sink
scope. Authority, target identity/generation and recipient lifetime are checked
at admission, dispatch and completion. A result for a changed or retired actor,
window or grant is discarded and does not write old coordinates into a new
run. Cleanup releases only that actor's saved claims.

There are at most eight screen nodes per graph, one outstanding read per
actor/node, eight pending reads per actor and 32 per session. A repeated trigger
or a full shared queue drops only the new read, increments
`dropped_observations`, and still commits an otherwise valid original input
wave and its effects. The oldest retained request is dispatched first; a queued
request has no queue-age expiry. The five-second worker deadline begins when
capture is actually dispatched. Pending, completed and dropped counts plus
`capture_worker_busy` expose worker status, never pixels or image results. Apply
and Arm remain unavailable until a canceled physical capture task has drained;
the runtime does not replace a blocked capture thread.

Screen-node capture uses the same selected client window and 16 Mi-pixel frame
bound, with a 64,000,000 candidate-pixel exhaustive matching budget. The raw
Mapper graph path does not inherit the standalone Screen Tools 2 MiB PNG wire
limit. Matching ignores alpha and reports no match separately from budget or
capture errors. The owned native cohort used two x64 target processes with two
roots each. It sent WM_CLOSE to target B's B0 root before a later trigger and
admission while B1 remained in the same process; that admission did not fall
back to B1. It did not exercise delayed completion. Pending/drop bursts,
delayed EOF, stale completion and tombstone cases are covered by fake-provider
and source tests only. This is bounded virtual evidence, not hardware or broad
application validation.

OCR, LLM vision, full-desktop Mapper capture and the remaining MouseMux
operators are still open. The historical GUI-stall P1 and full Ultra parity
remain open. [Mapper screen assessment](mapper-screen-assessment-2026-11-07.json)
