# Third-party code

- **ProtoInput** — https://github.com/Ilyaki/ProtoInput, pinned at `dd29bf8d22349605d0e1950c1c5dc4893d72670e`. MIT license, copyright (c) 2021 Ilyaki. The complete license is retained at `vendor/ProtoInput/LICENSE`.
- ProtoInput includes other libraries under `vendor/ProtoInput/lib`, including EasyHook, BlackBone, Dear ImGui, OpenXinput, GLEW, and cereal. Preserve their individual license/notice files when redistributing their source or compiled components; the top-level ProtoInput MIT license does not replace those licenses.
- Rust dependencies are recorded in `Cargo.lock`; retain their applicable notices for packaged distributions.
- **rmcp** — https://github.com/modelcontextprotocol/rust-sdk, Apache-2.0. The official Rust MCP SDK supplies protocol schemas, negotiation, dispatch and stdio transport.
- **Ouroboros UI** — https://github.com/Type-zero-labs/ouroboros-ui, MIT, pinned at `c390d7deffa7955e28b2e3bcb9c22ac0899a261b`, the same snapshot used by OuroForge. Supplies the egui component library, design tokens, Iosevka typography, and Phosphor icons. Preserve its LICENSE, CREDITS.md, and bundled font notices when packaging.

MouseMux is referenced only as the functional product direction. No MouseMux source, binaries, branding, or assets are included.

## Offline OCR code and model assets

- **ocrs 0.12.2 and RTen 0.24.0** — https://github.com/robertknight/ocrs and https://github.com/robertknight/rten. Code is MIT OR Apache-2.0; exact Rust dependency resolution is retained in `Cargo.lock`.
- **Ocrs pretrained Latin model weights** — Robert Knight (`robertknight`), https://huggingface.co/robertknight/ocrs, pinned revision `df0edd170279ab971b53e094c627255a87e1a503`. The two unmodified `.rten` weights are distributed separately under **CC-BY-SA-4.0**, as declared by the included publisher model card. This is distinct from the OCR code licenses and the application's source license.
- Preserve the complete six-file `assets/ocr` bundle: both models, `MODEL_CARD.md`, `provenance.json`, `LICENSE-CC-BY-SA-4.0.txt`, and `ATTRIBUTION.md`. Provenance records fixed URLs, lengths and SHA-256 hashes. The application verifies model bytes before parsing and never downloads models at runtime. See [the OCR text contract](docs/ocr-text-contract.md) for the fixed model identities and bounded qualification status.
The OCR word-component adapter is adapted from Ocrs `detection.rs` at commit
`2dbc1f840e47d45630ef6060499138bf597a9f65`, copyright (c) 2023 Ocrs project
contributors, under MIT. Its complete terms are retained in
`crates/protoinputmultiplex/notices/ocrs-MIT.txt`; portable notice staging supplies
`licenses/rust/ocrs-0.12.2/LICENSE-MIT.txt` when the published crate omits them.
The model attribution also credits Google Research's HierText dataset under the
[publisher's model-specific declaration](https://github.com/robertknight/ocrs-models/blob/a63bee16bf0c888e4ce7fe09ad124c83287444ee/LICENSE.md).
