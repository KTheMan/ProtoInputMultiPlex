# Mapper Type Text

Type Text is an output sink in the existing Rust Mapper graph. It types a Text
value into an explicitly selected virtual user's keyboard recipient. It uses
Ouroboros authoring controls and the existing MCP graph API.

A node is `{"type":"sink","user":"stable-seat-id","channel":{"type":"type_text"}}`.
It has two required inputs: `text` (Text) and `trigger` (Pulse), and no `value`
input. A changed true trigger emits the current cached text once. Text updates,
false pulses, validation, Apply and Arm do not type. A later explicit trigger
intentionally types the cached value again. The cache belongs to the current
actor/run; another connection does not inherit it. A missing text value rejects
the wave. Empty text emits nothing. Other effects and the original source's
configured disposition still apply normally.

For OCR, connect OCR `text` to the sink's `text`, and OCR `done` through an
optional predicate/Pulse Gate to its `trigger`. The completion's text and done
are evaluated in the same wave. An authored constant is also a valid text
source. Literal/config text is deliberately shared; live OCR/Monitor values
stay actor-private and are excluded from ordinary shared state and structured Script JSON. Opt-in GUI screenshot PNGs can contain visible recognized text. Text sent
to an authorized application becomes visible in that application.

## Bounds

The wave has the existing 128 weighted sink-output-effect budget. The original source action is not charged to this bound. Type Text costs
one instantaneous WM_CHAR packet per UTF-16 unit. A supplementary character
costs two units. A 128-unit text fits alone; 127 units plus a two-effect KeyPulse
is rejected, and 126 units plus that pulse fits. Compiler structural cost is a
minimum; actual text cost is checked dynamically before forwarding the original
source input, reserving claims, or delivering any sink. Over-budget waves are
rejected whole, never truncated. All text packets are validated before delivery.

Generic private Text remains bounded by 4096 UTF-8 bytes and authored literals
by 256 UTF-8 bytes. Output follows existing text policy: LF/tab are allowed; CR,
NUL and other control characters are rejected. How an application interprets
allowed control characters is application-specific. This addition does not
make Gate/Switch/Delay/Throttle/Scope accept Text.

## Authority and delivery

The output user is explicit and independent of the OCR capture user. Existing
source/ancestry/client/user/target permissions apply, with a paired virtual
keyboard, enabled keyboard output, and a current selected recipient. OCR still
needs its separate capture grant and selected capture target. Configuration
never silently grants capture, a new PID, or an output recipient.

Delivery retains one logical keyboard recipient before its first unit or lazy
attachment and revalidates it through the sequence. Scheduled/OCR output also
checks its saved expected recipient inside that retained scope. A new focus
recipient cannot silently take the rest of a sequence. The scope clears on
success, error and unwinding. These instantaneous text packets create no held
Unicode key claims.

Preflight is atomic; native delivery is not rollback-capable. A failure may leave
an irreversible typed prefix. It surfaces through existing Mapper retirement
and saved-release cleanup for held key/button effects. Already typed text is
never automatically replayed or removed. Inspect target contents and cleanup
state before deciding whether a new trigger is appropriate. Consumed source
DOWN markers still need their matching UP under the existing source rules.

## GUI and agent use

Discover `type_text` in the current Mapper catalogue, choose the output user,
connect both inputs, validate and Apply. Apply remains disarmed; Arm is explicit
and revision checked. The existing MCP tools expose the same graph channel and
ports; no new tool or implicit input action is added. Discovery and stopped
configuration do not require loading backend DLLs. Live typing requires the
normal explicitly scoped routing backend and recipient checks.

The repository agent skill documents these semantics. Acceptance evidence is
tracked in the companion assessment. It is bounded to virtual devices and
owned applications; it does not establish arbitrary-app/hardware behavior,
same-process isolation, historical GUI-stall causation, full Ultra/UI parity or
release readiness.


## Historical target stdio failure and follow-up

A failed owned-fixture attempt received two empty EDIT replies before attachment.
Its first post-attachment query was processed, but the stdout reply pipe closed;
the application text result was UNVERIFIED. At that time the backend allocated a
console and reopened stdout/stderr in ProtoInputHooks/dllmain.cpp:35–38. This
is source correlation, not proof of the exact failure mechanism or a P1 cause.

This Type Text group used a dedicated authenticated out-of-band pipe for
actual EDIT readback; its original failed stdout attempt remains UNVERIFIED.
The separate [target stdio group](target-stdio-contract.md) now reproduces and
fixes PIPE→CHAR mutation in its bounded Release/UCRT virtual matrix. Broader
ordinary-application compatibility remains open; the historical Type Text
artifacts and aggregate outcomes are unchanged.


## Bounded acceptance (November 7, 2026)

The separate [assessment](mapper-type-text-assessment-2026-11-07.json) records qualified source, actual owned EDIT contents, GUI authoring captures and independent review. Budget and delivery-fault coverage is chiefly deterministic unit evidence, not a broad native fault matrix. Original OCR acceptance remains separately scoped.
