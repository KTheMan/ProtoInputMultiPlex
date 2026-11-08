# Opt-in GUI capture liveness contract

**Evidence status: bounded GUI capture acceptance passed for one owned development flow.** This is not general GUI responsiveness evidence. Historical native-controls attempt004 remains OPEN/P1; its cause and fix are unproven. The later [UI-script live-I/O group](gui-script-io-assessment-2026-10-21.json) separately moved ongoing opt-in script snapshots, critical event/phase/ack writes, and sample polling to a worker. Other non-script telemetry and renderer behavior remain outside scope.

## Capture lifecycle

This change applies only to opt-in `--ui-script` PNG capture. Screenshot responses carry script, viewport, generation, and request-time tickets; stale or nonmatching responses are ignored. The GUI hook clones the renderer-owned pixel Arc into a single replaceable pending slot and does no PNG encoding, pixel flattening, or file publication. Frames are validated and bounded to 16 Mi pixels. While one frame is being written, pending requests coalesce to the latest generation.

One worker performs RGBA conversion and PNG encoding outside the queue/status locks, writes a unique temporary file beside the destination, stamps its modification time with the screenshot request time, then atomically replaces the PNG. A failed publication retains the prior PNG and reports failure in opt-in progress state. Request-time freshness prevents a late publication of a pre-event screenshot from appearing newer than the UI event.

On drop, the owner clears pending work and signals shutdown without joining the worker. A blocked filesystem call cannot be cancelled; process exit ends a remaining detached worker, and close does not guarantee in-process writer completion.

## Accepted evidence and limits

The [October 21 assessment](gui-capture-liveness-assessment-2026-10-21.json) covers six actual GUI clicks and six sample acknowledgments in one owned configuration flow with no ProtoInput DLL staged. The decoded 540×520 RGBA PNG request mtime followed the Native controls event by 2.8298164 seconds; latest published generation was 2; no failed or rejected generation was reported, and ordinary close exited 0 with the original HWND and staged-image process absent. No ProtoInput DLL was staged; no Start, routing, physical, or global input was used.

The producer passed formatting, strict Clippy, 456 workspace tests (0 failed, 3 ignored), and x64/i686 builds. Focused unit tests cover latest-frame coalescing, nonjoining Drop, failure recovery, old-file retention, and request-time freshness. The [independent review](../runtime/gui-capture-liveness-ui-attempt001-review.json) is SHA-256 `43430292448eae5f608046d6963e9b9122bc7a1ecb3276dfc751aafedbd66df7`; the [producer report](../runtime/producer-gui-capture-liveness-20261021-attempt001/verification.json) is SHA-256 `ac8f8456c205de8572faf6bd157f119e2a601f8dce4f43d83f39f643bc07ea02`.

This single flow does not establish general GUI responsiveness or resolution of the historical P1; its cause and fix remain unproven. There is no physical-input, RustDesk, desktop-wide, same-process, backend-injection, or full Ultra-parity claim.
