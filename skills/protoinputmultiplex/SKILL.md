---
name: protoinputmultiplex
description: Operate ProtoInputMultiPlex MCP for explicitly scoped virtual Windows input, stopped GUI setup, and read-only application-event SDK status.
---

# ProtoInputMultiPlex

Use the connected server's advertised tools and the operator's existing grants.
Routing connections expose 25 tools in current builds. A stopped GUI Setup
connection exposes `profile_setup` and `application_event_status` in builds
containing the SDK-status addition. Check discovery before using a newer tool.

## Choose the existing connection

- Standalone `protoinputmultiplex.exe mcp --runtime <absolute-runtime-directory>`
  owns a separate virtual routing session.
- `protoinputmultiplex.exe mcp --gui-connection <descriptor.json>` uses the private
  descriptor supplied by the GUI operator. A live routing descriptor and a
  stopped Setup descriptor grant different authority; use the supplied mode.
- For stopped SDK inspection, the operator explicitly enables Setup access.
  Routing and target PIDs are unnecessary. Do not start routing, enable access,
  switch authority, or create an event session merely to obtain status.

Keep descriptors and credentials private. Connection failures and unavailable
tools are not permission to broaden scope or reconnect under another authority.

## Inspect application-event SDK status

Call `application_event_status` with no arguments or `{}`. Both supported modes
reject extra fields. On a stopped Setup connection,
`profile_setup {"operation":"application_event_status"}` provides the same
read-only operation. It accepts no profile document, mutation, consent, user,
input, capture, or credential fields.

The result is a safe summary, not an event stream. It contains no user/source/
receiver identities, names, input values, or credentials. Unavailable or stale
transport counters are omitted; absence does not mean zero or completed cleanup.
Standalone routing does not own the GUI's application-event session.

Event connection issuance and event/control consent remain local Ouroboros
workflows. **Select all visible users** changes only a receiver draft's exact
current IDs. Apply and observation/control consent remain separate; later users
are not implicitly shared. Application-event IDs are not `routing_input` user
indices. MCP status grants neither event production nor routing/capture access.

## Routing and other operations

Read the relevant section of [operations](references/operations.md) before using
standalone/live routing, profile or preference mutations, capture, Mapper,
replication, automation, drawing, magnification, or recovery. It preserves the
existing mode-specific workflows and historical acceptance boundaries.
Repository contract links in that reference resolve from a project checkout.

Inspect current ownership, user/PID scope and delivery state before input. Use
virtual devices and disposable applications for qualification. Do not move the
operator's hardware or RustDesk pointer. Acknowledgement is not proof of target
delivery; after uncertain input or cleanup, inspect rather than replay blindly.
GUI-owned routing starts/stops through its GUI lifecycle; follow the documented
client-disconnect semantics instead of treating them as global Stop.

Source support, build checks, rendered pixels and functional/runtime acceptance
are separate evidence. Full Windows/Web SDK, hardware/isolation and Ultra/UX
parity are not established by this skill or a successful status call.
