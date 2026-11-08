# Worker transport contract

**Status: required checks and bounded independent acceptance passed.** See the
[assessment](worker-transport-assessment-2026-10-15.json). The real nonreading-pipe
regression covers Transport plus production Worker/Job teardown; the routed MCP
workflow covers healthy full-supervisor Stop. Live routed backpressure and GUI
fault injection remain unverified. The separately frozen 329-test release source
predates this fix; its release builds do not include the new transport behavior.

The routing supervisor must remain responsive when its disposable worker stops
reading stdin. A process-wide worker stall can fill the pipe and block a write.
An ordinary native DLL call being busy does not establish this condition: the
worker has a separate stdin-reader thread. This change addresses that transport
boundary without changing the pinned native backend.

One writer thread owns worker stdin. Its FIFO outbox holds eight queued frames,
with at most one active write and one additional pending supervisor frame. When
the outbox is full, the supervisor retains the pending frame and continues
checking deadlines, child state, and Stop intent. Ordinary backpressure alone is
not an immediate failure. These bounds apply to the transport outbox; they do not
redesign the existing caller or worker request queues.

Controller and shared-handle session Stop requests set independently visible Stop
intent. The supervisor observes that intent without waiting for an ordinary request
backlog or outbox capacity. Delivery still follows already offered FIFO frames or
ends at a deadline. Emergency and disconnected-input Stop use the same path.
Stop supersedes the one frame not yet offered to the writer. Previously offered
frames retain FIFO order, followed by Stop; ordinary Stop cleanup remains
responsible for releasing session input.
Standalone MCP `routing_stop` stops its session. Connected MCP `routing_stop`
remains a client-local disconnect; this change grants no global Stop authority.

Startup, request, agent, and Stop deadlines are established before enqueueing.
Each transport frame retains its write deadline until its actual write and flush
finish. Completion timestamps distinguish a late write from a timely completion
observed after a supervisor scheduling delay. Other progress or telemetry cannot
extend a stalled frame's deadline. A successful flush only reports transport
progress: it is not a worker acknowledgement or proof of input release.

Shutdown closes the outbox and terminates/waits for the exact owned worker and
its job before joining the writer. The stdout reader drains before a terminal
event is published, preserving late attachment evidence. A blocked writer must
never be joined while its nonreading worker can remain alive. The cleanup
commitment rule is unchanged: only the worker's Stopped acknowledgement,
successful worker exit, and successful cleanup commitment permit a clean stop.
Timeout, pipe failure, and successful writes cannot commit lifecycle cleanup.

Agents must use the existing UI/MCP Stopping, Failed, and recovery state. A Stop
request is not Stopped. After failure, retain the original affected-process
identities and quarantine rules; an empty display or missing live worker does
not prove cleanup. Do not automatically retry or start a replacement session.
Follow existing recovery guidance for the original target applications before
retrying. This change adds no permissions, MCP schema, or diagnostic controls.

The focused ordinary Rust tests cover a controlled blocked writer, FIFO frame
bytes, full-outbox backpressure, original write expiry, and Stop intent behind an
ordinary request backlog. One additional regression uses an exact owned, hidden,
finite-lived child that does not read its real stdin pipe; it checks timeout and
the production Worker/Job teardown boundary. Cleanup precedes assertions on every
error path. Existing tests continue to check that Failed cannot commit cleanup
and that Stopped requires successful worker exit. All four new regressions passed
in the 333-test workspace run. The separate healthy MCP gate passed one Start,
eight virtual input calls and one Stop with native UPs, exact worker exit and
clean recovery. This evidence does not establish hardware routing, desktop-wide
isolation, full Ultra parity, or new shipping acceptance, and do not replay the
already accepted normal GUI Stop/Quit workflow.
