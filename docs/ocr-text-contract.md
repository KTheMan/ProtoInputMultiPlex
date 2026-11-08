# OCR text and typed Mapper observations

Status: bounded independent acceptance for local Latin OCR on two owned
ordinary Windows targets and the explicit OCR-to-KeyPulse workflow.
OCR remains an early preview. [Assessment](ocr-text-assessment-2026-11-07.json)
Earlier Screen Tools and Mapper screen assessments retain their original
pixel/image-match scope; this does not establish broad OCR quality.

## Selected-window text extraction

In **Screen tools**, choose an active virtual user with an acknowledged selected
allowed application window, then **Read text**. **Text extraction options** takes
an optional client-coordinate region; all four fields blank means the whole
client. **Check OCR models** retries the background bundle check. Missing or
invalid models make the local action unavailable with a reason. Recognition
uses a fresh snapshot, not the previously displayed image.

The existing MCP `routing_capture` tool accepts the additive query:

```json
{"user":0,"analysis":{"type":"ocr","region":{"x":12,"y":8,"width":640,"height":120}},"return_image":false}
```

Omit `region` or use null for the whole client. Omitted `return_image` still
returns the PNG; false returns capture metadata and analysis without the image.
No arbitrary PID, HWND, desktop source, model path or external provider is
accepted in the query. The tool count and existing pixel/image-match queries
are unchanged.

OCR analysis contains `type:"ocr"`, `provider:"ocrs"`, `language:"Latin"`,
`text`, `lines`, `client_region`, `desktop_region`, and `snapshot_only:true`.
Each line contains its text, `client_box`, `desktop_box`, and `words`; each word
has the same text/box fields. Boxes are integer `[x,y,width,height]`, clipped to
the requested region and translated into client and desktop coordinates.
Desktop coordinates may be negative. A successful empty read returns empty
text and no lines. There is no fabricated confidence score or inference that
an application control can be acted upon.

The local GUI uses existing human capture authority. Connected MCP separately
requires Agent Access capture permission, current client/user/PID scope and
the exact selected target. Routing or PID permission alone is insufficient.
Standalone capture requires its explicit process scope. Existing completion
checks revalidate authorization and exact target after capture and analysis.
Changing the Screen Tools session, user or target invalidates its private
snapshot. OCR text/boxes are absent from structured JSON Script state. An
explicitly requested GUI screenshot PNG may contain visible recognized text.

## OCR to a Mapper action

`ocr {capture_user,region}` is a triggered Observer with an explicit stable read
user. Its input is `trigger:Pulse`; outputs are `text:Text`, `has_text:Hold`,
and `done:Pulse`. Recognition updates Text and has_text together, then emits
done through the ordinary typed wave. An empty read writes `""`, sets has_text
false and still completes successfully. Inference/capture errors are failures,
not empty recognition.

A complete action path is:

```text
virtual Wheel → ImpulseToPulse → OCR trigger
OCR text → TextContains → Gate open
OCR done → Gate value → KeyPulse sink
```

`text_contains {needle,case_sensitive}` and
`text_compare {expected,operation,case_sensitive}` take `value:Text` and emit
`value:Hold`. Compare operations are `equals` and `not_equals`. Contains requires
a nonempty needle; Compare permits an empty expected value. Case-insensitive
comparison folds ASCII A–Z only, not general Unicode case or normalization.
There is no regex or scalar coercion. Empty OCR participates in comparisons
normally: for example, NotEquals against `"READY"` is true for empty text.
Combine has_text with the predicate using Hold logic when recognition must
contain actual text. Use the done Pulse gate to request an action per successful
read rather than relying on a rising edge of an unchanged predicate.

Text may be observed by latest Monitor and authored as a static Text constant.
Delay, Throttle, Gate(Text), Switch(Text), Scope, numeric operators and numeric
visualizers do not accept Text. The original OCR acceptance did not include a Mapper TypeText sink; [explicit triggered typing](mapper-type-text-contract.md) has a [separate bounded assessment](mapper-type-text-assessment-2026-11-07.json). Existing
KeyPulse/KeyHold outputs retain their normal permission, effect-budget and
saved-release rules. No OCR result is implicitly typed, clicked or dispatched.

Validation checks portable structure, types and OCR parameters only. Apply
prepares configuration and commits disarmed; Arm is explicit and revision
checked. None captures, recognizes or initializes an OCR engine. GUI bundle
availability checks are separate worker jobs. Static validation is not proof
that models, capture permission or a live target are available.

## Bounds, lifecycle and privacy

| Bound | Limit |
|---|---:|
| Captured frame | Existing 16 Mi-pixel bound |
| OCR region | 1,048,576 pixels; at most 4096 per side |
| Detected/returned words | 256 |
| Detected/returned lines | 64 |
| Runtime Text | 4096 UTF-8 bytes; empty permitted |
| Predicate literal | 256 UTF-8 bytes |
| Observation nodes / pending per actor / pending per session | 8 / 8 / 32 |
| Outstanding reads per actor/node | 1 |

Text rejects control characters other than tab, CR and LF. The app rejects
invalid/oversized regions and excessive recognition-line aspect before
recognition. The verified detector input is 600 × 800; the recognizer uses
height 64 and maximum width 2400, so recognition-line aspect is capped at 37.5.
The worker passes the original ROI to the pinned library preprocessing, which
pads small inputs and internally resizes detector tensors before mapping results
back to the input. Recognition normalizes line height internally. The app uses
a fixed, strict pixel-probability boundary of 0.5 for word components, deliberately
differing from the library's empirical 0.2 default. The neutral boundary corrected
false detections on the retained blank/owned sample without filtering recognized
tokens. It is not caller-tunable and is not a word-recognition confidence score.
The small MIT-attributed contour adapter retains the upstream component geometry.
Mask dimensions, finite probabilities and bounds are checked before recognition;
word/line and UTF-8 limits fail explicitly instead of truncating output. No white
canvas or app-side ROI resampling remains. Accuracy still depends on content,
contrast and layout; broad dark-theme or arbitrary-application quality is open.

OCR reuses the existing one physical Mapper capture task and its tombstone;
GUI/MCP capture has its separate existing slot. Work starts only after the
trigger wave and original/native effects are admitted. Repeated triggers for
an outstanding node, or a full session queue, drop only the new read and
increment `dropped_observations`; otherwise-valid input still commits. The
five-second deadline starts at dispatch, not healthy queue admission. Cancel
does not free a still-running physical task or spawn a replacement.

Receipts retain the actual actor/origin, run/ticket, source ancestry, explicit
capture user, selected window/process lifetime and downstream sink scopes.
Completion rechecks these and evaluates the actor's **current** State, then
commits only after downstream effects succeed. EOF retires its actor; Disarm,
Stop, revoked authority, changed targets, timeouts and faults cancel work and
release only saved Mapper claims. Uncertain release remains visible as cleanup;
do not blindly retry input. Rejected input while pending/held observation work
could be stranded follows existing safe retirement.

Live pixels and recognized Text remain in the requesting actor's state/latest
Monitor or its explicit capture reply. `inspect_mapper_monitors` is actor-private;
ordinary state exposes counts, not text/frames. No recognized text enters shared
profiles, flows, diagnostics, error messages, structured JSON Script state or
trace history.
Authored constants, comparison literals and OCR configuration are deliberately
shared graph configuration. No automatic result-to-configuration copy occurs.

## Offline assets

OCR uses `ocrs = 0.12.2` and `rten = 0.24.0`; code licenses are MIT OR Apache-2.0.
The separate unmodified pretrained weights are published by Robert Knight at
[robertknight/ocrs](https://huggingface.co/robertknight/ocrs), revision
`df0edd170279ab971b53e094c627255a87e1a503`, under CC-BY-SA-4.0 as recorded in
the included publisher card and provenance.

| Model | Bytes | SHA-256 |
|---|---:|---|
| `text-detection-ssfbcj81.rten` | 2,523,564 | `614aafabf27c94d386f7aa036c967c2e47e4b9938fa11531ca8f5698c1ca4c36` |
| `text-rec-checkpoint-s52qdbqt.rten` | 9,716,444 | `606d9a0414c6b73c99df75b707c11c70d1c8b12e1d4f900922e185fc37bfca65` |

The six-file executable-relative `assets/ocr` bundle also contains
`MODEL_CARD.md`, `provenance.json`, `LICENSE-CC-BY-SA-4.0.txt`, and
`ATTRIBUTION.md`. Bounded reads verify length/hash before parsing the same
buffer. Only successfully initialized immutable model weights are cached;
failed initialization may be retried after the bundle is corrected. No frame
or text is cached globally. There is no runtime download, source-tree fallback,
clipboard, global input or external vision/LLM service. See
[portable packaging](portable-packaging.md) and
[third-party notices](../THIRD_PARTY_NOTICES.md) for distribution requirements.

OCR can misread Latin text. Broader languages, arbitrary applications, physical
devices, desktop-wide or same-process isolation, and full Ultra parity remain
outside this bounded group.
