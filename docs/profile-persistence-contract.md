# Profile startup and exit persistence

Ouroboros Application preferences provides independent **Load saved profile at startup**
and **Save applied profile on exit** controls. Apply commits them; unapplied drafts do not
change policy. Legacy version-1 preferences default to loading on and exit saving
off. The version remains 1.

Startup selects explicit `--profile` when supplied, otherwise
`<data-root>/profiles/local.json`. With loading enabled, only a missing default
file means a fresh setup; invalid, unreadable, and missing explicit profiles
remain unresolved load errors. With loading disabled, the selected path stays
active but is not read. Manual Load remains available. Changing this setting
does not load immediately; it takes effect on the next launch.

Saving occurs on real Quit, including native close when close-to-tray is off.
Hide, minimize and close-to-tray do not save. Quit waits for SDK retirement,
routing cleanup, preferences and every pending profile operation. It captures
the applied profile and active path once, freezes mutations, and requires that
exact save receipt before closing. Unsubmitted drafts are excluded. Disabling
startup loading while enabling exit saving intentionally saves the fresh applied
setup to its active path. A failed initial load blocks that exit save.

Changed saves retain the exact previous valid bytes in the appended `.bak` file
and atomically replace the main file. An already byte-equivalent normalized
profile is an exit-only no-op, preserving its recovery backup after Restore.
Manual Save retains its established backup behavior. Invalid or unreadable main
files are not silently replaced. Failure or an unconfirmed receipt keeps or
reveals the window and requires an explicit retry. A pending manual Save that
finishes without installing during Quit also reveals the window for review;
issue Quit again after reviewing the current setup. There is no automatic retry.

## Agent operation

Enable stopped Setup access in Agent Access and connect using its private
descriptor. Call `profile_setup` Inspect, then use its current revision:

```json
{"operation":"configure_persistence","expected_revision":7,"persistence":{"load_profile_at_startup":false,"save_profile_on_exit":true}}
```

Both booleans are required; no document or path is accepted. The operation is
revision checked, default off with Setup authority, and changes no login
registration or unrelated preferences. Pending profile file work must finish
first; local application-preference and profile drafts remain available. Inspect reports
`profile_persistence`, `preferences_operation` and `exit_save`. The ordinary live
or standalone `app_preferences` Configure schema rejects these fields, while
Inspect can show them. Revocation, stale revisions or abandoned waits cancel
unclaimed policy transactions. A claimed commit can finish and be installed
after client timeout/EOF. Re-inspect before retrying uncertain work.

## Evidence and limits

The [independent assessment](profile-persistence-assessment-2026-11-07.json)
enumerates the exact native workflow, source/unit boundaries, original failed
attempts and successful qualification. Native tests use disposable profiles and
owned windows without DLLs or routed input. They do not prove hardware or general
application behavior. The historical GUI-stall P1, full Ultra/UI parity and
release readiness remain open. The physical folder rename stays user-deferred;
other chats remain separate.
