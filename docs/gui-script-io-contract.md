# UI-script live I/O contract

**Evidence status: bounded acceptance passed for one owned virtual-seat configuration flow.** This does not establish general GUI responsiveness or resolution of historical native-controls GUI-stall attempt004. The P1 remains OPEN; its cause and fix are unproven.

## Scope and worker behavior

One detached worker handles ongoing opt-in `--ui-script` filesystem work: telemetry snapshots, click/phase/ack appends, flush plus `sync_data`, and bounded sample-count polling. Initial `Script::load` filesystem work remains outside the worker and live callback. Bounded payload serialization remains synchronous on the GUI thread. The earlier PNG-only publication path is unchanged; normal GUI telemetry, renderer behavior, Agent Broker, macro save, and session drain are outside this group.

Snapshot values are bounded to 2 MiB each and 8 MiB total, coalesced to the latest pending value per path, and published through same-directory atomic replacement with request-time mtime. Critical click/phase/ack records use a FIFO capped at 64 items, 2 MiB queued bytes, and 64 KiB serialized payload plus newline per record. Each record is flushed and synced with `sync_data` before the acknowledged sequence advances; a click advances only after its exact event ticket is durable. After four consecutive critical writes, a pending snapshot gets service. Within snapshot selection, readiness and progress go ahead of routine telemetry. After two such selections, the oldest routine snapshot gets the next slot when one is pending. Both groups keep FIFO order by path, and queued values still coalesce to the latest per path. A due sample poll has priority and reads at most 65 bytes every 50 ms.

The worker gates clicks on exact durable tickets and next-step observation on the held-sample count. One matching in-flight pointer release can complete while its prior phase write is pending. New input and next-step observation remain gated. Queue, path, serialization, or write faults fail closed; an unfinished virtual press is cancelled/released once and failure appears in existing status. Drop clears pending jobs and signals shutdown without joining potentially blocked I/O.

## Accepted bounded evidence

The November 6 priority check exercised both driver paths, latest-value
coalescing and routine service during replenishment in the real worker with a
controlled writer. The observer first read Ready and Progress on the network
share about 4.2 and 4.4 seconds after the owned x64 GUI window appeared.
The Compatibility and Native controls clicks had matching tickets.
Authenticated callback counters advanced and the GUI closed normally. The
historical stall remains open. [Priority assessment](gui-script-priority-assessment-2026-11-06.json)

The [October 21 assessment](gui-script-io-assessment-2026-10-21.json) records six actual GUI clicks and six sample ACKs. With sample 3 held, frames advanced from 288 to 311 while event and acknowledged sequence stayed at 19. A stable 540×520 RGBA app-owned PNG had request-time mtime 2.6357358 seconds after the Native controls event. After the sixth sample-ack phase, progress showed submitted/acked sequence 30, greater than final click sequence 29, no pending critical records, and no fault. Ordinary close exited 0 with original HWND and staged-image process absent. No ProtoInput DLL was staged and no Start, routing, MCP, probes, desktop capture, physical input, RustDesk, or global input was used.

The producer passed formatting, strict Clippy, 463 workspace tests (0 failed, 3 ignored), and x64/i686 builds; seven focused tests passed. The [independent review](../runtime/gui-script-io-ui-attempt001-review.json) is SHA-256 `fdd0ecc0cdfced1bbf3ae6cd654db4dd013923d476b10b9c3c9bfb78a2ff7106`; the producer report is SHA-256 `bf3807c1a3c04e1d7a4b16699a928ecfc161bb9e2b2d1fe5c5b83001cbe6735d`.

One 160-byte `.ui-script-124040-218-0.tmp` remained in the retained run directory after ordinary exit. Published files were intact, but complete temporary-file cleanup is not claimed. This group does not establish general responsiveness or resolution of the historical P1 (cause/fix unproven). See the [source review](../runtime/gui-script-io-source-review.json), [validation entry](validation.md#ui-script-live-io-liveness--october-21-2026), and [native plan](../runtime/gui-script-io-ui-candidate/acceptance-plan.md).
