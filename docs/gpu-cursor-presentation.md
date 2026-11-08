# GPU cursor presentation

The isolated current-application integration starts at snapshot `04658a2623f840cb2460ca6035008df743d08e37`. That snapshot preserves the main session's 832 protected inputs from its accepted annotation-export producer, including current application-cursor following and unrelated lifecycle, routing, GUI and permission work. Separate baseline commit `ee241610f433541e7de0c4963265d14b844f0c2b` then preserves only the main session's reviewed 5,000 ms native Open budget; established 1,500 ms replies, 4,000 ms prepare and the vendor remain unchanged. The main session's post-change required checks/builds passed; its native acceptance is still pending. This document is an implementation contract; native and performance qualification is pending until the recorded acceptance evidence is attached.

## Renderer and presenter boundary

`overlay.rs` retains cursor layout, GDI rasterization, application-image blending, render keys, metadata refresh, geometry, visibility, labels, badges and its owned `WM_PRINT` surface. The private `cursor_presenter.rs` owns the DIB resource and presents the final premultiplied BGRA pixels unchanged. The image upload is GPU presentation of a CPU raster. No GPU rasterization is implemented.

Each overlay owns an independent hardware D3D11 device, immediate context and DirectComposition tree on its existing owner thread. The presenter cannot be sent or shared across threads. It never borrows the egui/eframe graphics context. This follows the [D3D11 threading contract](https://learn.microsoft.com/en-us/windows/win32/direct3d11/overviews-direct3d-11-render-multi-thread-intro): the immediate context and DXGI presentation are used on the same thread. Shutdown releases the composition tree and GPU resources before destroying the owned HWND.

The composition chain uses BGRA8, premultiplied alpha, two buffers, flip-sequential presentation and stretch scaling with exact cursor extents. It retains only the active extent and one spare extent. A size change swaps a matching spare or evicts it before allocating another resource pair. Resources never scale or pad the final cursor raster. Cached movement reuses the raster and checks device health, without another upload, Present or Commit.

These choices follow Microsoft's [composition swap-chain contract](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgifactory2-createswapchainforcomposition). The owned HWND backing stays transparent in the GPU path. Its retained CPU `WM_PRINT` image is useful for compatibility but is not evidence of GPU presentation.

## Fallback and observability

GPU initialization, device-health, upload, Present or Commit failure permanently selects GDI for that overlay. The same call presents the current CPU raster through `UpdateLayeredWindow` with `ULW_ALPHA`; subsequent cached movement remains usable. The upload preserves current custom cursor alpha, including opaque black and partial alpha. It does not apply the historical color-key conversion. Microsoft's [layered-window API](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-updatelayeredwindow) documents this alpha mode, and [BLENDFUNCTION](https://learn.microsoft.com/en-us/windows/win32/api/wingdi/ns-wingdi-blendfunction) requires premultiplied RGB for per-pixel alpha.

Existing `OverlayStatus` telemetry gains a defaulted `presentation` object:

- `backend`: `pending`, `direct_composition_d3d11`, or `gdi`.
- `adapter` and `fallback_reason`: optional hardware identity and permanent fallback explanation.
- `gpu_uploads`, `gpu_presents`, `composition_commits`: successful changed-raster submissions.

Legacy serialized overlay states deserialize with pending presentation and zero counters. Existing MCP routing state exposes this additive data. The Ouroboros Theme panel shows read-only cursor-display status and fallback explanation for an active owned overlay. No capture tool, setting, input grant or permission default changes.

## Bounded diagnostic evidence

The off-default `cursor-presentation-evidence` Cargo feature enables acceptance instrumentation only. It also requires `PROTOINPUTMULTIPLEX_GPU_CURSOR_EVIDENCE_DIR` to name an absolute, pre-created disposable directory. Normal builds contain no staging copies, evidence writes, commit waits or diagnostic environment hooks. The feature-enabled build with no directory creates no recorder.

The recorder reserves bounded per-overlay and process capacity before staging selected changed generations. It writes exact CPU pixels, a copy of the same pre-Present GPU buffer, immutable completed receipts and owner-thread cleanup ledgers. Metadata-only cache refreshes share the original raster generation and do not take another copy. Diagnostic failures are reported in evidence ledgers and never select product fallback. An optional named-overlay force-GDI hook exists only in the diagnostic/test build.

Generation-specific GPU readback waits for the associated composition commit and records an owned-chain present count. This establishes transfer parity and acknowledged presentation activity. Microsoft defines [WaitForCommitCompletion](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositiondevice-waitforcommitcompletion) as completion of processing the previous commit; it does not establish physical scanout, movement-composition latency or an independent layout oracle. Raw composition timestamps are retained without converting them into scanout latency.

The evidence caps are 32 records/128 MiB per overlay and 96 records/256 MiB per process. Reservations include raw files and control metadata; failed partial attempts remain charged. A completed receipt is published without overwriting a previous name. Missing receipts, errors, budget exhaustion or cleanup failures invalidate acceptance without changing production behavior.

## Qualification scope

Required checks are formatting, strict workspace Clippy and workspace tests, plus full x64 and i686 application and observer builds. The coordinated native plan covers current-route owned overlays, supported stock/custom/hidden application cursors, Unicode labels/badges, geometry, click-through/no-activate/capture exclusion, cached movement, fallback and cleanup. Release comparison uses separate frozen GDI baseline and GPU candidate binaries with evidence disabled.

The earlier native-tail investigation remains a bounded partial result: thread-switch association was measured, but scheduler-versus-compositor attribution was unavailable after the ETW capability probe returned access denied. This integration does not reopen that investigation or claim that its approximately 24–25 microsecond interaction is explained. Real devices, arbitrary applications, mixed-DPI/reconnect behavior and physical scanout remain outside the bounded acceptance scope.
