# ProtoInputMultiPlex native identity alignment

The Rust application, SDK, and native compatibility glue use the
ProtoInputMultiPlex namespace. The active aggregate backend patch and its
lifecycle and keyboard-state layers carry matching product-owned names:

- `ProtoInputMultiPlex.QueueFence.V1` for the registered queue-fence message.
- `Local\\ProtoInputMultiPlex.Lifecycle.*` and
  `Local\\ProtoInputMultiPlex.Lease.*` for lifecycle objects.
- `\\\\.\\pipe\\ProtoInputMultiPlex.Lifecycle.*` for lifecycle transport.
- `protoinputmultiplex_cursor_observer32.dll` and
  `protoinputmultiplex_cursor_observer64.dll` for the Rust observer libraries.

The upstream ProtoInput name, revision, interfaces, and third-party notices are
unchanged. These edits change product-owned string identities only; observer
exports, packet layouts, handle ownership, and hook behavior are unchanged.

The isolated transition is preserved in
[`../archive/0006-protoinputmultiplex-branding-transition.patch`](../archive/0006-protoinputmultiplex-branding-transition.patch)
as a historical review artifact. Do not apply it in addition to the active
patches: the canonical strings are already incorporated into those layers.
