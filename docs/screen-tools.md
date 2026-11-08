# Selected-window Screen Tools

Screen Tools captures one explicitly selected allowed client window for an
active virtual user. Local GUI admission uses that user's existing capture
path and explicit allowed selected PID; it does not require the Agent Access
checkbox. Connected MCP calls require the separate capture grant. Input or PID
scope by itself is not capture permission. Standalone MCP requires explicit
process scope.

The GUI page can capture a snapshot, inspect one pixel, or find a template
cropped from the current snapshot. Pixel coordinates and search regions are
relative to the captured client area. Pixel results include RGBA, client
coordinates and desktop coordinates derived from that same snapshot. Image
matches include top-left, size, center, score and desktop positions.

Use MCP `routing_capture` with `user`, optional `analysis`, and optional
`return_image` (true by default). `analysis` is tagged as either
`{"type":"pixel","x":0,"y":0}` or
`{"type":"image_match","template_png_base64":"…"}`. Image-match PNGs are
limited to 256 KiB encoded and 128 × 128 pixels decoded. `region` is optional
and fully bounded by the captured image. `min_score` defaults to 0.95 and must
be finite in [0, 1]. The result is the highest-scoring row-major candidate that
meets the threshold, or `match: null`. Matching exhaustively compares RGB
mean-absolute similarity, ignores alpha, and rejects searches above 64,000,000
candidate-pixel operations before scanning. A budget error is not a no-match
result. `return_image: false` returns metadata and analysis without the PNG.

Capture and analysis run on the existing worker with its five-second deadline.
Every path binds the result to the same active user and exact selected target.
The local GUI drops results if its UI epoch or target binding changes.
Connected MCP additionally rechecks the current Agent Access capture grant,
client generation and allowed PID after analysis and before returning results.
Standalone MCP requires explicit PID scope and rechecks the selected target.
Screen Tools stores snapshots
and templates in memory only and clears them when the session, user, target or
target revision changes. It does not write files, call a vision/LLM provider,
capture the desktop or OS pointer, or send input. Results are observations of
one snapshot; they do not click, move, or launch automation.

This standalone selected-window inspection workflow remains bounded. Mapper now
also has Pulse-triggered Pixel Color and Image Match graph nodes for selected
client windows; see the [Mapper screen contract](mapper-screen.md). OCR, LLM
vision, full-desktop capture and remaining graph operators remain open.
[Mapper screen assessment](mapper-screen-assessment-2026-11-07.json)
