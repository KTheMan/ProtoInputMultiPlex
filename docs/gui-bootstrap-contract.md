# Responsive GUI startup

The [independent bounded composed assessment](gui-bootstrap-assessment-2026-10-28.json) accepts this startup workflow.

The GUI opens a small Ouroboros initialization window while a Rust worker acquires the installation/configuration leases, reads application preferences, loads the optional font and UI script, and starts the lifecycle service. Retry is unavailable while initialization is pending. Close stays available. Profile changes, routing Start and agent access become available after Dashboard readiness.

A preferences read or parse error remains visible in a wrapped, vertically scrollable area above Retry and Close. The error includes its path and cause. Repair the indicated file, then use Retry in the same window. A failed read does not replace the file with defaults. A missing data directory needs an existing parent so startup can acquire its sibling configuration lock; an unavailable parent is reported as a recoverable startup error.

Closing during initialization cancels adoption of the result and does not wait for the worker on the UI thread. Cancellation is cooperative between native phases. File-backed ownership guards remain alive for any worker still using the installation or data root. This does not promise a deadline for a Windows call already in progress.

Validation is scoped to a virtual-only development layout. Required formatting, strict workspace Clippy, 580 tests (zero failures, one ignored), and x64/i686 app/observer builds passed on the corrected source. A fresh 360px error screen showed the complete path and parse cause, followed by one actual Retry, same-window Dashboard readiness, positive Quit handler receipt (0 to 1) and normal exit. Independent acceptance composes that corrected run with the unchanged pending-Close case from the original cohort. The original cohort remains independently NOT_ACCEPTED for its clipped error text; no single corrected two-case cohort is claimed.

The opt-in --bootstrap-delay-ms test flag requires a UI script and is bounded to 15 seconds; ordinary startup has no artificial delay. Public script readiness stays false in the initialization window. No MCP permission or input authority is granted by initialization.

This work removes specific pre-window blocking paths. The historical intermittent mid-session GUI stall and earlier cleanup Quit timeout remain open. Hardware, arbitrary applications, installed upgrade behavior, general GUI reliability and full MouseMux Ultra parity are not established by this gate.
