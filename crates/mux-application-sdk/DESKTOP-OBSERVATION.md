# Version 1 desktop observations

This is a trusted, explicitly approved virtual observation bridge. It does not
discover devices, export physical identities, inject OS input, or prove target
delivery. The private desktop worker and GUI controller own admission and loss
detection; none of their device, seat, routing, binding, window, or target tokens
are public SDK data.

## Trusted host boundary

`Host` and `Engine` expose `create_desktop_observed(owner, name, baseline)`,
`publish_desktop_observation(owner, user, record)`, and
`retire_desktop_observed(owner, user)`. `HostBridge` forwards these typed calls.
They are absent from `ClientCommand`, `HostCommand`, and owner publication frames.
The provider owner must be a separate private recipient, not a receiver or the
cooperative GUI owner. `screens()` returns the exact session mapping. The baseline
must match it, including revision and ordered bounds; topology replacement requires
retiring bindings and establishing a deliberate new mapping/session.

The worker supplies an atomic, validated baseline of actual logical ownership.
Held buttons and native keys enter the initial snapshot, without synthetic DOWN
events or a claim that their historical DOWN reached an OS target. Provider records
start at `next_sequence` and must be exactly contiguous. An invalid transition,
identity, revision, position, or sequence retires the mapping. Consumers accept
strictly increasing provider sequences because grant filtering can omit intervening
pointer, key, or text records.

Native keys retain Windows VK, scan byte, and None/E0/E1 prefix as distinct version
1 data. They never enter cooperative USB HID `held_keys`. Generic Web event hooks observe provider events.
The provider event wrapper stamps the exact serial pointer position and redacts it
without pointer consent. Selector-based virtual-click callbacks share the existing
button pairing and hit testing using that position, never final-batch pointer state;
the callback payload remains the original provider event, with no DOM/OS dispatch. Text is one UTF-16 unit,
including an individual surrogate, rather than a HID event or a normalized string.
Configured cursor state retains the desktop cursor shape (including System), exact
finite f32 size, RGB color, configured visibility, and follow-application preference.
It does not report effective cursor pixels or application visibility.

## Consent and negotiation

Desktop consent defaults to absent; text consent defaults to false in the GUI.
The host approves `Some(DesktopConsentV1 { version: 1, text_units: false })` for a
fresh receiver. Normal export consent must also select exact `Only(user IDs)` and
the intended pointer/key families. `AllOwned` never exports observed users.
Changing consent retires subscriptions; the GUI bridge retires an existing permit
before any approval change, so reconnect requires a freshly registered receiver.

Decoder support is separate from consent. Legacy `EventHello` is unchanged and
still rejects unknown fields. Only `DesktopEventHello` includes the explicit
`desktop_observation_version: 1`. Authentication records this before the owning GUI
thread negotiates the model and emits Ready. Desktop descriptors advertise the
optional version only after explicit host consent; legacy descriptors omit it.
Legacy Ready and cooperative UserView JSON omit the new optional `desktop` field,
and legacy clients receive no observed users, desktop capability, or desktop event.

Native Rust callers use `NativeClient::connect_desktop(config, hwnd, 1)` only after
reading a version 1 descriptor; existing `connect` remains legacy. The native C ABI
keeps its signature/layout: its bounded JSON configuration accepts an optional
`desktop_observation_version: 1`, omitted for a legacy descriptor. Web callers use
`connect_desktop(endpoint, credential, 1)`; `connect` remains legacy. A transport
neutral `ClientState` calls `enable_desktop_observation(1)` before installing the
authenticated snapshot. Unsupported versions and provider state without opt-in
fail closed. JSON carries identifiers as existing SDK JSON text, not JS-rounded
numeric wrapper fields.

Pointer consent exposes position, button holds and configured cursor, but no native
key holds or text. Key consent exposes native key holds but no pointer/cursor.
Text additionally requires explicit text consent and key consent. Observed users
always report `owned_by_recipient: false`. Core provenance denies every normal
rename/dispose/cursor/input/click mutation even for the private owner; SDK delegated
and AllOwned controls do not bypass it. No receiver can change the private source
binding. The consumer-local toolkit can annotate an approved observed UserView;
it cannot mutate the provider or send input.

## Retirement boundary and validation

Provider retirement scrubs that user's queued Engine creation/update/observation
payloads before late Poll can read them, then emits disposal only where the user
was already visible. Other users and recipients remain intact. Client disposal,
gap, terminal state and disconnect clear provider holds and local annotations.
Already serialized or written transport bytes cannot be retracted; loss detected
concurrently with a GUI turn is not an instantaneous global visibility guarantee.
The GUI checks worker/controller liveness before folding each delivery and before
draining the bridge, and rechecks after trusted creation/publication.

Source-authored Core, SDK and transport tests cover exact native held baselines,
UTF-16 units, family redaction, negotiation before Ready, legacy wire omission,
owner/delegation denial, sequence fencing, queue scrubbing and consumer held-state
clearing. This private overlay has not been parsed, formatted, compiled or run
locally. Hosted formatting, clippy, workspace tests and independent source review
remain required, followed by actual bounded GUI/native/Web lifecycle acceptance.
No desktop-wide isolation, real-device, target-delivery or release qualification is
established by this source change.
