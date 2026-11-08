# Stopped GUI profile setup through MCP

**Status:** Bounded composed acceptance passed the [independent assessment](stopped-setup-assessment-2026-10-27.json).

## Use the stopped Setup connection

In the GUI, choose the active profile file and optional transfer file under **Settings → Users & devices → Profiles & backend**. While routing is stopped, open **Settings → Agent access → Agent Access** and select **Enable setup access**. The GUI creates a private, authenticated connection with descriptor kind `stopped_setup`. It starts disabled on each GUI launch and is separate from live Agent Access.

The connected MCP server exposes only `profile_setup`. Use `inspect` first. It returns the complete GUI-owned profile, an opaque `revision`, the GUI-selected active and transfer paths, file status, setup authority and broker status, recovery status, and discovery status. `refresh_discovery` requests a new GUI scan; inspect its requested and completed generations and `devices_current` before treating native device path/kind rows as fresh. Keep stable seat IDs. Virtual seats use canonical virtual paths; newly assigned physical seats use fresh discovered interface paths of the right kind. Previously saved disconnected physical paths may remain for offline editing, with Start still subject to current checks. Configured virtual paths are reported separately. Neither result contains Raw Input handles or proves attachment.

To replace the profile, call `configure` with the current `expected_revision` and a **complete version-1 profile JSON string** in `document`. This is a whole-profile operation. A stale revision, unsaved local GUI draft, pending file job, or unavailable stopped authority rejects it. Re-inspect after a rejection and resolve any local draft in the GUI before retrying. An accepted Configure changes the GUI's committed in-memory profile; use Save to persist it.

`save` and `load` use the active path selected in the GUI. `export` and `import` use the GUI-selected transfer path. Each request carries `expected_revision` only; MCP cannot supply a filesystem path. A successful file call returns a structured worker ticket, kind, exact selected path, revision, and terminal outcome such as `saved`, `exported`, `installed`, or `finished_not_installed`. A failed call returns MCP error text without a guaranteed structured ticket or outcome. Inspect reports the **current** file ticket and status, which a later operation may have superseded. A timeout or EOF remains **unconfirmed** unless later evidence reliably identifies that same accepted operation. Reconnect while Setup authority remains enabled, or re-enable it in the GUI after revocation; never blindly replay an uncertain write.

**Disable setup access**, an accepted GUI Start, and GUI Quit revoke Setup authority and retire its descriptor and broker. While the GUI remains running, re-enabling after Disable or Stop requires a fresh generation and token. After Quit, relaunch the GUI before enabling Setup again. Setup cannot start routing, attach targets, send input, capture, change unrelated application preferences, or grant live Agent Access. Its narrow persistence-policy operation is described below. The GUI owns Start, Stop, and Quit.

## Profile persistence policy

`configure_persistence` changes only `load_profile_at_startup` and
`save_profile_on_exit`, through the serial application-preferences owner. It
requires the current `expected_revision` and a complete `persistence` object.
Inspect includes `profile_persistence`, `preferences_operation` and `exit_save`.
This is separate from whole-profile Configure and does not load or save a
profile immediately. Claimed commits remain applied even if the requester
disconnects; unclaimed requests are cancelled on authority loss, stale revision
or abandonment. See the [complete policy contract](profile-persistence-contract.md)
and its [bounded assessment](profile-persistence-assessment-2026-11-07.json).

## Limits and acceptance boundary

The named-pipe limit is **16 MiB of encoded JSON including the envelope**. A profile within the separate 16 MiB profile-file limit can exceed the pipe limit after JSON escaping. Oversized Configure is rejected before GUI dispatch; oversized Inspect returns an explicit error. There is no chunked transfer.

The focused current-build workflow observed all 38 GUI control steps, profile configuration and four file operations, revision and local-draft rejection, authority revocation/reconnect, broker retirement before Start, default-off live Agent Access, normal Stop, Setup re-enable, and connected-client cleanup through a verified owned native Quit command. Its positive GUI Quit receipt and normal process exits were observed with no forced cleanup. Required checks passed: formatting, strict Clippy, 574 tests with zero failures and one ignored, and paired x64/i686 Windows builds.

Acceptance must be composed only as declared in the focused contract: prior hide/actual tray Open and hidden authenticated Inspect are separate supported observations. Earlier failed aggregates retain their NOT_ACCEPTED status. This does not establish a new popup/accessibility pass or one-cohort end-to-end UI pass. The earlier raw005 cleanup Quit timeout remains unresolved. Physical-device behavior, routed input delivery, historical GUI-stall P1 closure, general reliability and full Ultra functional/UI parity remain outside this group.
