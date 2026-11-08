# Input forwarding contract

**Evidence status: bounded virtual acceptance passed for one owned development workflow.** This is not physical-device, desktop-wide, arbitrary-application, same-process-isolation, RustDesk, GUI-stall-fix, or full Ultra parity evidence. Historical GUI-stall P1 remains OPEN.

## Scope and graph

Forwarding supports active virtual users in automatic desktop routing. The version-1 graph uses stable user IDs and allows at most one outgoing edge per source. It rejects invalid/missing/inactive endpoints, self-links, duplicate outgoing edges, cycles, and keyboard-enabled edges to users without a virtual keyboard. Multiple chains may converge. Pointer actions route once to the terminal user. Optional keyboard/text traverses only enabled hops and stops locally at the first disabled hop. Physical users and other routing modes are unavailable. A nonempty replication graph and forwarding cannot be active together.

## GUI and persistence

The Ouroboros editor keeps draft and applied definitions separate and displays dirty, pending, error, and unavailable states. Apply is explicit. A live graph becomes applied only after a matching worker revision and resolved-edge acknowledgement. A stopped Apply updates the in-memory profile; the existing stopped-only Save profile action persists it. Runtime hiding of forwarding-source overlays does not change saved visibility settings. GUI Clear restores each source's configured visibility; normal Stop tears down overlays while retaining preferences.

## Input and release behavior

Relative pointer motion passes through the terminal user's tuning and bounds. Absolute MoveTo uses a source-coordinate anchor retained across graph revisions, not the hidden/stationary source overlay position. Connected actions are authorized before recording, anchor changes, claims, or emission for the original source, each traversed user, actual terminal sink and selected PID. Claims retain the source/path and terminal output recipient so converged holds can be independently released. UP uses its saved recipient after settings or graph changes; failed UP cleanup remains pending and does not issue a new DOWN.

## MCP authorization

The existing 23-tool MCP surface extends `routing_graph` with `inspect_forwarding`, `apply_forwarding`, and `clear_forwarding`; it adds no tools. Inspect is read-only. Connected graph Apply/Clear require the expected forwarding revision from Inspect, session-control permission for every active user, and the full explicit nonempty automatic-routing PID allowlist including selected recipients. Client EOF does not revert a GUI-applied graph.

## Accepted evidence and limits

The [October 21 assessment](input-forwarding-assessment-2026-10-21.json) records 51 real GUI actions, 13 app-local virtual `routing_input` calls including one expected denial, and one connected identical-graph MCP Apply across three owned ordinary EDIT probes and two normal worker cycles. It accepted partial keyboard routing, tuned motion and retained anchor across a graph revision, convergence hold cleanup, stopped Save and same-GUI worker restart, fresh Agent Access grant before MCP mutation, GUI Clear visibility restoration, and normal Stop/EOF/WM_CLOSE cleanup. The restart reused the same GUI and saved profile; it did not verify fresh disk reload. This is one bounded app-local virtual development workflow.

The producer passed formatting, strict Clippy, 452 workspace tests (0 failed, 3 ignored), and x64/i686 app and observer builds. The [independent review](../runtime/input-forwarding-ui-attempt004-review.json) is SHA-256 `99c6562f10b0ec87cfe42fcdb4d01de030b3e53e96743320411c59f9551e5451`; the [producer report](../runtime/producer-input-forwarding-20261021-attempt003/verification.json) is SHA-256 `ef67855286aaea3818b08ccbe540bd206ed2bf92cadcc9302e14a12f91b2c150`. Native attempts [001](../runtime/input-forwarding-ui-attempt001/evidence.json), [002](../runtime/input-forwarding-ui-attempt002/evidence.json), and [003](../runtime/input-forwarding-ui-attempt003/evidence.json) remain FAIL with original evidence and reviews preserved. This acceptance does not validate real hardware, arbitrary applications, desktop-wide routing, same-process multi-user isolation, RustDesk, or full Ultra parity; historical GUI-stall P1 remains OPEN.
