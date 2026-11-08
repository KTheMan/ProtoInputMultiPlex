//! RETIRED historical CPU-upload acceptance source; no longer compiled.
//! Superseded by `owned_gpu_rasterization_cache_fallback_and_window_contract`
//! in `overlay_native_acceptance.rs`. The original oracle below is preserved.
//!
//! Exact-size surface cache acceptance using owned DIB/GPU resources only.
//! Explicitly ignored: it creates owned windows, but captures no desktop pixels and sends no input.
use super::*;
use crate::cursor_presenter::Presenter;
use std::{fmt::Write as _, fs, path::PathBuf};

fn raw_pixels(overlay: &Overlay) -> Vec<u32> {
    let surface = overlay.print_surface.borrow();
    let canvas = surface.canvas.as_ref().unwrap();
    let count = usize::try_from(surface.size.0)
        .unwrap()
        .checked_mul(usize::try_from(surface.size.1).unwrap())
        .unwrap();
    unsafe { std::slice::from_raw_parts(canvas.bits.cast::<u32>(), count).to_vec() }
}

fn exclude_owned_window(overlay: &Overlay) {
    assert_ne!(
        unsafe { SetWindowDisplayAffinity(overlay.hwnd, WDA_EXCLUDEFROMCAPTURE) },
        0
    );
}

fn assert_window_contract(overlay: &Overlay, pointer: (i32, i32)) {
    unsafe {
        let style = GetWindowLongPtrW(overlay.hwnd, GWL_EXSTYLE) as u32;
        for flag in [
            WS_EX_LAYERED,
            WS_EX_TRANSPARENT,
            WS_EX_NOACTIVATE,
            WS_EX_TOOLWINDOW,
            WS_EX_TOPMOST,
        ] {
            assert_ne!(style & flag, 0, "missing extended style {flag:x}");
        }
        assert_eq!(
            SendMessageW(overlay.hwnd, WM_NCHITTEST, 0, 0),
            HTTRANSPARENT as LRESULT
        );
        assert_eq!(
            SendMessageW(overlay.hwnd, WM_MOUSEACTIVATE, 0, 0),
            MA_NOACTIVATE as LRESULT
        );
        let mut affinity = 0;
        assert_ne!(GetWindowDisplayAffinity(overlay.hwnd, &mut affinity), 0);
        assert_eq!(affinity, WDA_EXCLUDEFROMCAPTURE);
        let mut rect: RECT = std::mem::zeroed();
        assert_ne!(GetWindowRect(overlay.hwnd, &mut rect), 0);
        assert_eq!(
            (
                rect.left + overlay.status.hotspot.0,
                rect.top + overlay.status.hotspot.1
            ),
            pointer
        );
        assert_eq!((rect.left, rect.top), overlay.status.render_origin);
        assert_ne!(IsWindowVisible(overlay.hwnd), 0);
    }
}

#[test]
#[ignore = "Requires coordinated owned-window GPU/cache native acceptance"]
fn owned_gpu_resize_cache_pixels_lifecycle_and_fallback() {
    let directory = std::env::var_os("PROTOINPUTMULTIPLEX_GPU_EVIDENCE_DIR")
        .map(PathBuf::from)
        .expect("explicit isolated evidence destination");
    fs::create_dir_all(&directory).unwrap();
    let guard = DesktopGuard::cursor_thread().unwrap();
    let foreground = unsafe { GetForegroundWindow() };
    let mut overlay = Overlay::new("Owned resize/cache acceptance").unwrap();
    overlay.presenter.enable_readback();
    let mut golden = Overlay::new("Owned GDI cache golden").unwrap();
    golden.presenter.shutdown();
    golden.presenter = Presenter::forced_gdi(golden.hwnd);
    exclude_owned_window(&overlay);
    exclude_owned_window(&golden);

    // A/B/A/B/C/B/A: A and B alternate between the two resident size pairs;
    // C evicts the least-recently-used A; B hits; the final A allocates again.
    let sequence = [0usize, 1, 0, 1, 2, 1, 0];
    let expected_creates = [1u64, 2, 2, 2, 3, 3, 4];
    let expected_reuses = [0u64, 0, 1, 2, 2, 3, 3];
    let mut extents = Vec::new();
    let mut report = String::from(
        "Owned GPU cache acceptance: raw premultiplied BGRA comparison; no desktop capture/input.\n",
    );

    for (step, variant) in sequence.into_iter().enumerate() {
        let settings = CursorSettings {
            size: [28.0, 56.0, 84.0][variant],
            shape: [
                CursorShape::Arrow,
                CursorShape::Crosshair,
                CursorShape::Ring,
            ][variant],
            label_position: LabelPosition::Left,
            badge: Some("P2".into()),
            badge_shape: BadgeShape::Box,
            badge_position: BadgePosition::TopRight,
            color: [45 + step as u8 * 9, 212 - step as u8 * 13, 191],
            ..Default::default()
        };
        let paused = variant == 2;
        let pointer = (64 + step as i32, 80 + step as i32);
        let generation = overlay.status.paint_generation + 1;
        let uploads = overlay.status.presentation.gpu_uploads;
        overlay
            .draw(pointer.0, pointer.1, "Resize acceptance", &settings, paused)
            .unwrap();
        golden
            .draw(320, 80, "Resize acceptance", &settings, paused)
            .unwrap();
        assert_eq!(overlay.status.paint_generation, generation);
        assert_eq!(overlay.status.presentation.gpu_uploads, uploads + 1);
        assert_eq!(
            overlay.status.presentation.backend,
            "direct_composition_d3d11"
        );
        assert_eq!(overlay.status.canvas_size, golden.status.canvas_size);
        assert_eq!(overlay.status.hotspot, golden.status.hotspot);
        let expected = raw_pixels(&golden);
        assert_eq!(
            raw_pixels(&overlay),
            expected,
            "CPU DIB mismatch at step {step}"
        );
        let (gpu, presents, frame_time) = overlay
            .presenter
            .readback_for_generation(generation)
            .unwrap();
        assert_eq!(
            gpu, expected,
            "premultiplied BGRA GPU bytes at cache step {step}"
        );
        assert!(presents > 0);
        let cache = overlay.presenter.gpu().unwrap().resource_cache_evidence();
        assert_eq!(
            cache,
            (
                (step + 1).min(2),
                expected_creates[step],
                expected_reuses[step]
            )
        );
        if step < 3 {
            extents.push(overlay.status.canvas_size);
        }
        assert_window_contract(&overlay, pointer);
        assert_eq!(unsafe { GetForegroundWindow() }, foreground);
        writeln!(report, "step={step} variant={variant} extent={:?} generation={generation} pixels={} dxgi_presents={presents} raw_frame_time={frame_time} cache={cache:?}", overlay.status.canvas_size, gpu.len()).unwrap();
        assert!(!guard.pump());
    }
    assert_ne!(extents[0], extents[1]);
    assert_eq!(extents[0], extents[2]);

    // Cached movement, hide and reveal reuse the last frame without repainting or upload.
    let settings = CursorSettings {
        size: 28.0,
        shape: CursorShape::Arrow,
        label_position: LabelPosition::Left,
        badge: Some("P2".into()),
        badge_shape: BadgeShape::Box,
        badge_position: BadgePosition::TopRight,
        color: [99, 134, 191],
        ..Default::default()
    };
    // Re-enter the same A key retained by the last sequence element.
    let cached_generation = overlay.status.paint_generation;
    let cached_uploads = overlay.status.presentation.gpu_uploads;
    let cache = overlay.presenter.gpu().unwrap().resource_cache_evidence();
    for step in 0..100 {
        overlay
            .draw(64 + step, 80, "Resize acceptance", &settings, false)
            .unwrap();
    }
    let hidden = CursorSettings {
        visible: false,
        ..settings.clone()
    };
    overlay
        .draw(170, 190, "Resize acceptance", &hidden, false)
        .unwrap();
    assert_eq!(unsafe { IsWindowVisible(overlay.hwnd) }, 0);
    overlay
        .draw(170, 190, "Resize acceptance", &settings, false)
        .unwrap();
    assert_eq!(overlay.status.paint_generation, cached_generation);
    assert_eq!(overlay.status.presentation.gpu_uploads, cached_uploads);
    assert_eq!(
        overlay.presenter.gpu().unwrap().resource_cache_evidence(),
        cache
    );
    assert_window_contract(&overlay, (170, 190));

    // An injected presentation failure on a cached frame permanently selects GDI in the same call.
    overlay.presenter.fail_next_present = true;
    overlay
        .draw(171, 191, "Resize acceptance", &settings, false)
        .unwrap();
    assert_eq!(overlay.status.presentation.backend, "gdi");
    assert!(
        overlay
            .status
            .presentation
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("injected presentation failure")
    );
    assert!(overlay.presenter.gpu().is_none());
    assert_eq!(overlay.status.paint_generation, cached_generation);
    assert_window_contract(&overlay, (171, 191));
    assert_eq!(unsafe { GetForegroundWindow() }, foreground);
    writeln!(report, "cached_moves=100 hidden_reveal=true cache={cache:?} same_call_permanent_gdi_fallback=true presentation={:?}", overlay.status.presentation).unwrap();

    assert!(
        golden
            .status
            .presentation
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("injected initialization failure")
    );
    let hwnd = overlay.hwnd;
    let golden_hwnd = golden.hwnd;
    drop(overlay);
    drop(golden);
    assert_eq!(unsafe { IsWindow(hwnd) }, 0);
    assert_eq!(unsafe { IsWindow(golden_hwnd) }, 0);
    assert!(!guard.pump());
    report.push_str("owned_windows_destroyed=true foreground_unchanged=true PASS\n");
    fs::write(
        directory.join("optimization-native-acceptance.txt"),
        &report,
    )
    .unwrap();
    println!("{report}");
}
