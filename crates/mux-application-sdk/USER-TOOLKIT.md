# Local user toolkit

The Rust, Web and native consumer APIs share one implementation for current authorized user lookup, application-owned JSON data and badge colors. This is consumer-local state: no network operation, grant expansion, physical device discovery, Host cursor recoloring, callback or default persistence occurs.

Rust: `ClientState::local_toolkit(ToolkitRequest)` or `local_toolkit_json(&str)`. Windows Rust consumers can also use `NativeClient::local_toolkit(&str)`. Web: `WebClient.local_toolkit(requestJson)` returns JSON text synchronously. Native C: additive `pim_app_native_local_toolkit(client, request_json, length)` retains JSON for the existing response_size/read/discard API. The existing ABI3 function signatures/layouts remain unchanged; callers discover the added export normally. Toolkit JSON has its own required `version: 1`.

```json
{"version":1,"operation":{"kind":"source","source":9007199254740993}}
```

The operations are `list`, `user` with exact `{epoch,local}`, `source` with opaque SourceId, `set_data` with user and any bounded JSON value, `clear_data` with user, `palette`, and `set_palette` with `colors: [[r,g,b,a], ...]`. SourceId never identifies a physical interface path, Raw Input handle, grouped keyboard/mouse pair or hardware ID. Exact absent/stale/out-of-scope lookup returns `{"version":1,"result":{"kind":"user","user":null}}`; assigning data to such a user fails.

A returned user context contains the current consent-redacted `user` view, `app_data` (null when absent) and `badge_rgba`. Badge colors use a fixed integer hash of the exact UserId and a shared original default palette; slots may repeat. Applications can replace the local badge palette explicitly. This never changes the authoritative cursor's observed RGBA or issues CursorControl. Each consumer's app data and selected palette are independent; they are not synchronized between native and Web clients.

JSON identity integers may exceed JavaScript's exact Number range. Pass request and response text without rounding identities through JSON.parse/Number. Use a lossless application parser or preserve exact identity JSON fragments. The Web binding parses the request in Rust, preserving u64 values; no JavaScript numeric conversion is involved in the method itself. Native JSON readers must likewise use unsigned 64-bit integer handling.

Limits: 8192 UTF-8 bytes/request; 4096 encoded JSON bytes/data value; 32768 encoded JSON data bytes across the complete cache; at most the current 64 authorized users; nesting at most 32 levels and 4096 nodes/value; 1..32 palette entries with nonzero alpha. These are JSON byte/count bounds, not an allocator-size claim. Invalid or over-limit operations leave prior data/palette intact. Version and unknown request/operation fields fail explicitly. No local lookup is allowed outside the current Live snapshot.

A same-epoch refresh preserves annotations only for still-visible exact UserId/source bindings. Disposal or loss of visibility removes their data; source replacement cannot inherit it. Gap, terminal/revocation/disconnect and a new epoch clear all annotations and reset the badge palette. Refresh after a gap starts empty. Toolkit data is not added to state_json, network snapshots, event frames, native model queries, or logs: it appears only in an explicit local toolkit result.

The native operation runs only on the existing owner thread. An unread retained response yields BUSY without executing the operation; read/discard it first. Short reads keep the exact original result and never re-execute a mutation. It allocates no remote request ID and does not consume pending network requests. Stop still clears ordinary retained output under the existing contract. The Web method invokes no callback while borrowing state; reentrant callers either complete synchronously or receive a fixed busy error rather than a RefCell panic.

Authored acceptance extends the existing GUI native/WASM pair at its two-user baseline, real overflow gap and existing reentrant cleanup, preserving that fixture's no-native-disposal/no-native-gap overlap contract. Shared model tests cover disposal/revocation/epoch/visibility rebinding and total-cache limits. Actual physical device lookup, hardware metadata, global DPI discovery and offline users remain separate work. Source presence is not runtime qualification.
