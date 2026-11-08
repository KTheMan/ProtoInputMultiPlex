# GUI render-capture observation

Optional metadata in the existing GUI activity response passed bounded
independent acceptance for one stopped GUI and setup MCP client.
[Assessment](capture-observation-assessment-2026-11-07.json)

## What the optional field reports

`Snapshot.render_capture` may be present when the current GUI process has a
script capture instance. It contains metadata only:

- A process-local script identity and whether its screenshot worker was
  enabled.
- The latest requested and received screenshot generations, monotonic age of
  the latest request, and age of an outstanding request when one exists.
- Dimensions for the latest matching screenshot
  event, plus whether the existing capture worker accepted the image Arc.
- The existing PNG worker's active, pending, published, failed and rejected
  generations, and whether the script/capture owner has retired.

It contains no pixels, PNG bytes, paths, image hashes, target content or
automation inputs. A script identity is an in-process correlation value, not a
credential or durable identity.

## Meaning and limits

The request-to-receipt duration runs from the GUI's screenshot request to
acceptance of the matching renderer `Screenshot` event. It includes frame
scheduling and presentation as well as readback; it is not the duration of
Glow's `read_pixels` call. Receipt means only that the event returned an image
to the GUI callback. Worker acceptance means the existing worker accepted the
image Arc for its bounded PNG publication queue. `published` reports file
publication, not rendering or UI health. Request/receipt metadata and worker
publication metadata are sampled independently. They do not form one atomic
pipeline transaction, so a transient worker generation ahead of a separately
sampled receipt is not a health or ordering failure.

Counts describe unique increasing generations, not normal frame rate.
`request_age_ms` and `receipt_age_ms` age the latest request and received
generation respectively. `pending_age_ms` measures the continuous interval
with an outstanding response; newer requests do not reset it. It clears when
the latest requested generation is received or the reporter retires.

An outstanding age does not prove that the renderer is blocked. `retired` means the script/capture
owner retired, not that a pending renderer response or detached writer
finished. An absent field, missing subfield or unknown worker-lock sample is
unknown, not success or failure. If a one-shot worker-state `try_lock` cannot
acquire its mutex, omit that worker-state portion while retaining otherwise
valid callback activity.

This channel describes explicit UI-script screenshot work only. It says
nothing about normal repaint or presentation when no screenshot was requested.
The observer reads metadata without filesystem access or blocking waits. It
uses the existing authenticated activity channels and grants no capture,
routing, input, restart or recovery authority. It adds no MCP tool.

The November 6 observation recorded a GUI action and an owned-window
probe response alongside 13 pending script snapshots and screenshot-writer
generation activity. That run ended before its next scripted page transition
and did not identify a renderer, filesystem or historical stall cause. The
existing screenshot-enabled/disabled comparison also did not measure the
renderer readback call. Any future result must keep those boundaries explicit.

For direct `read_pixels` timing, a separate focused diagnostic would need to
instrument the Glow readback boundary in a private build and correlate that
duration with the existing callback and window probes. Request-to-receipt age
alone cannot substitute for that measurement. Neither observation establishes
that the historical stall is fixed.

## Private connection files in long temporary directories

The owned setup exercise exposed failure creating a private bridge descriptor
when its random temporary suffix crossed the ordinary Windows path limit.
The Rust bridge resolves the existing parent directory to an extended Windows
path and uses that form consistently for private creation, rename and cleanup.
It retains exclusive creation, the protected current-logon ACL and atomic
publication. The native path remains long; it was not shortened to bypass the
case. This is a separate connection-file reliability fix, not a cause or fix
for the historical GUI stall.
