//! Focused, explicitly ignored acceptance of exact premultiplied GPU cursor pixels.
//! Reads owned CPU/GPU resources only; it never captures the desktop or sends input.
use super::*;
use crate::cursor_presenter::RasterBackend;
use std::{fs, path::PathBuf, sync::Arc};

fn evidence_dir() -> PathBuf {
    let directory = std::env::var_os("PROTOINPUTMULTIPLEX_GPU_EVIDENCE_DIR")
        .map(PathBuf::from)
        .expect("explicit isolated evidence destination");
    fs::create_dir_all(&directory).unwrap();
    directory
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
        assert_ne!(
            GetForegroundWindow(),
            overlay.hwnd,
            "overlay must not activate"
        );
    }
}

fn application_cursor(visible: bool) -> AppCursorObservation {
    // This is a source glyph asset, not a CPU-composed final cursor canvas.
    // GPU scaling/composition must preserve premultiplied transparent artwork.
    let mut pixels = [32, 64, 96, 128].repeat(8);
    pixels[..4].copy_from_slice(&[0, 0, 0, 255]);
    AppCursorObservation {
        state: "Native acceptance fixture".into(),
        capability: true,
        sequence: 1,
        kind: 2,
        visible: Some(visible),
        image: visible.then(|| AppCursorImage {
            width: 4,
            height: 2,
            hotspot: (1, 1),
            pixels: Arc::from(pixels),
            identity: 0x0acc_0001,
        }),
        ..Default::default()
    }
}

fn read_gpu(overlay: &Overlay) -> Vec<u32> {
    let (pixels, presents, time) = overlay
        .presenter
        .readback_for_generation(overlay.status.paint_generation)
        .unwrap();
    assert!(presents > 0 && time > 0);
    assert_eq!(
        pixels.len(),
        (overlay.status.canvas_size.0 * overlay.status.canvas_size.1) as usize
    );
    assert!(pixels.iter().any(|pixel| pixel >> 24 > 0));
    assert!(pixels.iter().any(|pixel| pixel >> 24 == 0));
    assert!(pixels.iter().all(|pixel| {
        let [b, g, r, a] = pixel.to_le_bytes();
        b <= a && g <= a && r <= a
    }));
    pixels
}

#[test]
#[ignore = "Requires coordinated actual hardware GPU readback; owned overlays only, no desktop input/capture"]
fn owned_gpu_rasterization_cache_fallback_and_window_contract() {
    let directory = evidence_dir();
    let guard = DesktopGuard::cursor_thread().unwrap();
    let mut a = Overlay::new("GPU raster user A").unwrap();
    let mut b = Overlay::new("GPU raster user B").unwrap();
    a.presenter.enable_readback();
    b.presenter.enable_readback();
    exclude_owned_window(&a);
    exclude_owned_window(&b);
    let mut settings = CursorSettings {
        label: true,
        badge: Some("A1".into()),
        ..Default::default()
    };
    a.draw(64, 80, "Unicode λ user A", &settings, false)
        .unwrap();
    let a_pixels = read_gpu(&a);
    let a_record = a.status.clone();
    assert_eq!(
        a.status.presentation.raster_backend,
        RasterBackend::Direct2D
    );
    assert_eq!(
        a.status.presentation.gpu_uploads, 0,
        "no final CPU-canvas upload"
    );
    assert!(
        a.print_surface.borrow().canvas.is_none(),
        "normal GPU draw has no CPU final raster"
    );
    assert_eq!(
        a.status.pixel_telemetry_source.as_deref(),
        Some("unavailable_gpu")
    );
    assert_eq!((a.status.painted_pixels, a.status.pixel_hash), (0, 0));
    let moved = a.status.presentation.clone();
    let generation = a.status.paint_generation;
    a.draw(70, 90, "Unicode λ user A", &settings, false)
        .unwrap();
    assert_eq!(a.status.paint_generation, generation);
    assert_eq!(
        (
            a.status.presentation.gpu_draws,
            a.status.presentation.gpu_presents,
            a.status.presentation.composition_commits
        ),
        (
            moved.gpu_draws,
            moved.gpu_presents,
            moved.composition_commits
        )
    );
    assert!(a.print_surface.borrow().canvas.is_none());
    assert_window_contract(&a, (70, 90));

    let before_pause = a.status.presentation.gpu_draws;
    a.draw(70, 90, "Unicode λ user A", &settings, true).unwrap();
    let paused_pixels = read_gpu(&a);
    let paused_record = a.status.clone();
    assert_eq!(
        a.status.presentation.raster_backend,
        RasterBackend::Direct2D
    );
    assert_eq!(a.status.presentation.gpu_draws, before_pause + 1);
    assert!(a.print_surface.borrow().canvas.is_none());
    assert_ne!(
        a_pixels, paused_pixels,
        "paused tint changes actual GPU pixels"
    );
    a.draw(70, 90, "Unicode λ user A", &settings, false)
        .unwrap();
    assert_eq!(
        read_gpu(&a),
        a_pixels,
        "unpausing restores original artwork"
    );

    // Independent original shape/text/badge rendering; both overlays are GPU.
    let other = CursorSettings {
        shape: CursorShape::Ring,
        color: [255, 80, 30],
        badge: Some("B2".into()),
        badge_shape: BadgeShape::Circle,
        ..Default::default()
    };
    b.draw(320, 120, "Independent B", &other, false).unwrap();
    let b_pixels = read_gpu(&b);
    let b_record = b.status.clone();
    assert_eq!(
        b.status.presentation.raster_backend,
        RasterBackend::Direct2D
    );
    assert!(b.print_surface.borrow().canvas.is_none());
    assert_ne!(a_pixels, b_pixels);
    assert_window_contract(&b, (320, 120));

    // Exercise the remaining original GPU vector branches on the same owner.
    let mut vector_captures = Vec::new();
    for (name, shape) in [
        ("gpu-crosshair", CursorShape::Crosshair),
        ("gpu-ibeam", CursorShape::IBeam),
    ] {
        b.draw(
            320,
            120,
            "vector",
            &CursorSettings {
                shape,
                label: false,
                ..Default::default()
            },
            false,
        )
        .unwrap();
        assert_eq!(
            b.status.presentation.raster_backend,
            RasterBackend::Direct2D
        );
        assert!(b.print_surface.borrow().canvas.is_none());
        vector_captures.push((name, read_gpu(&b), b.status.clone()));
    }

    // Ordinary stock following with a non-System preference has no observer
    // image. Resolve a glyph asset while preserving the existing stock layout.
    let stock_settings = CursorSettings {
        shape: CursorShape::Crosshair,
        label: false,
        follow_application: true,
        ..Default::default()
    };
    let stock_app = AppCursorObservation {
        kind: 32515,
        visible: Some(true),
        image: None,
        ..Default::default()
    };
    b.draw_application(
        (320, 120),
        "stock",
        &stock_settings,
        false,
        Some(&stock_app),
    )
    .unwrap();
    let stock_pixels = read_gpu(&b);
    let stock_record = b.status.clone();
    assert_eq!(
        b.status.presentation.raster_backend,
        RasterBackend::Direct2D
    );
    assert!(b.print_surface.borrow().canvas.is_none());
    assert_window_contract(&b, (320, 120));
    let before_hidden = b.status.presentation.gpu_draws;
    let hidden = application_cursor(false);
    b.draw_application((320, 120), "stock", &stock_settings, false, Some(&hidden))
        .unwrap();
    let (hidden_pixels, presents, time) = b
        .presenter
        .readback_for_generation(b.status.paint_generation)
        .unwrap();
    assert!(presents > 0 && time > 0);
    assert!(
        hidden_pixels.iter().all(|pixel| *pixel == 0),
        "hidden glyph without label/badge is fully transparent"
    );
    assert_eq!(b.status.presentation.gpu_draws, before_hidden + 1);
    assert_eq!(
        b.status.presentation.raster_backend,
        RasterBackend::Direct2D
    );
    assert!(b.print_surface.borrow().canvas.is_none());

    // Native/custom glyph source bytes may be CPU assets. Scaling, halo and final
    // composition must remain GPU, and color-only edits reuse the asset texture.
    settings = CursorSettings {
        shape: CursorShape::System,
        size: 32.,
        label: false,
        follow_application: true,
        ..Default::default()
    };
    let app = application_cursor(true);
    a.draw_application((70, 90), "glyph", &settings, false, Some(&app))
        .unwrap();
    let pixels = read_gpu(&a);
    assert!(pixels.iter().any(|pixel| {
        let [_, _, _, alpha] = pixel.to_le_bytes();
        alpha > 0 && alpha < 255
    }));
    let uploads = a.status.presentation.asset_uploads;
    settings.color = [30, 220, 100];
    a.draw_application((70, 90), "glyph", &settings, false, Some(&app))
        .unwrap();
    assert_eq!(
        a.status.presentation.asset_uploads, uploads,
        "glyph texture reused while GPU halo changes"
    );
    assert!(a.print_surface.borrow().canvas.is_none());
    let halo = read_gpu(&a);
    let halo_record = a.status.clone();
    assert_ne!(pixels, halo);

    // Exact-size A/B/A reuse keeps at most two chains, with no backbuffer target
    // reference retained by D2D between draws or eviction.
    for size in [20., 48., 20., 48., 80., 48.] {
        let setting = CursorSettings {
            size,
            label: false,
            ..Default::default()
        };
        b.draw(320, 120, "cache", &setting, false).unwrap();
        let (resident, _, _) = b.presenter.gpu().unwrap().resource_cache_evidence();
        assert!(resident <= 2);
        read_gpu(&b);
    }
    assert!(b.presenter.gpu().unwrap().resource_cache_evidence().2 > 0);

    // The print-only CPU compatibility replay is lazy and cannot change the
    // current GPU generation, pixels or draw/present counters.
    let before = a.status.presentation.clone();
    let generation = a.status.paint_generation;
    let print = DibCanvas::new(a.status.canvas_size.0, a.status.canvas_size.1).unwrap();
    assert_ne!(
        unsafe {
            SendMessageW(
                a.hwnd,
                WM_PRINTCLIENT,
                print.dc as WPARAM,
                PRF_CLIENT as LPARAM,
            )
        },
        0
    );
    assert!(a.print_surface.borrow().canvas.is_some());
    assert_eq!(a.status.paint_generation, generation);
    assert_eq!(a.status.presentation.gpu_draws, before.gpu_draws);
    assert_eq!(a.status.presentation.gpu_presents, before.gpu_presents);
    assert_eq!(read_gpu(&a), halo);

    // Cached device loss replays the SAME recipe/generation lazily, without a
    // host device reset. The other user's resources remain healthy/independent.
    let generation = b.status.paint_generation;
    assert!(b.print_surface.borrow().canvas.is_none());
    b.presenter.fail_next_present = true;
    b.draw(
        321,
        121,
        "cache",
        &CursorSettings {
            size: 48.,
            label: false,
            ..Default::default()
        },
        false,
    )
    .unwrap();
    assert_eq!(b.status.paint_generation, generation);
    assert_eq!(b.status.presentation.raster_backend, RasterBackend::Gdi);
    assert!(b.status.presentation.fallback_reason.is_some());
    assert!(b.print_surface.borrow().canvas.is_some());
    assert!(b.status.painted_pixels > 0 && b.status.pixel_hash != 0);
    assert_window_contract(&b, (321, 121));
    assert_eq!(
        a.status.presentation.raster_backend,
        RasterBackend::Direct2D
    );

    // Explicit policy changes reinitialize/detach only on the owner thread and
    // distinguish user-selected GDI from an initialization failure.
    settings.renderer = mux_core::desktop::CursorRenderer::Gdi;
    a.draw_application((70, 90), "glyph", &settings, false, Some(&app))
        .unwrap();
    assert_eq!(a.status.presentation.raster_backend, RasterBackend::Gdi);
    assert!(a.status.presentation.fallback_reason.is_none());
    settings.renderer = mux_core::desktop::CursorRenderer::Auto;
    a.draw_application((70, 90), "glyph", &settings, false, Some(&app))
        .unwrap();
    assert_eq!(
        a.status.presentation.raster_backend,
        RasterBackend::Direct2D
    );
    assert!(a.print_surface.borrow().canvas.is_none());
    assert_window_contract(&a, (70, 90));
    assert!(!guard.pump());
    for (name, pixels) in [
        ("gpu-original-a", a_pixels),
        ("gpu-original-b", b_pixels),
        ("gpu-custom-halo", halo),
        ("gpu-paused-a", paused_pixels),
        ("gpu-stock-follow", stock_pixels),
    ] {
        let packed = pixels
            .iter()
            .flat_map(|p| p.to_le_bytes())
            .collect::<Vec<_>>();
        fs::write(directory.join(format!("{name}.bgra")), packed).unwrap();
    }
    let mut receipt = serde_json::json!({"schema":2,"source":"actual_gpu_rasterization_readback","a":a.status,"b":b.status,"captures":[{"file":"gpu-original-a.bgra","overlay":a_record},{"file":"gpu-original-b.bgra","overlay":b_record},{"file":"gpu-custom-halo.bgra","overlay":halo_record},{"file":"gpu-paused-a.bgra","overlay":paused_record},{"file":"gpu-stock-follow.bgra","overlay":stock_record}],"two_owner_overlays":true,"no_cpu_final_canvas_upload":true,"lazy_cached_failure":true,"print_only_cpu_replay":true,"hidden_glyph_transparent":true,"ambient_foreground_not_asserted":true});
    for (name, pixels, status) in vector_captures {
        let file = format!("{name}.bgra");
        let packed = pixels
            .iter()
            .flat_map(|pixel| pixel.to_le_bytes())
            .collect::<Vec<_>>();
        fs::write(directory.join(&file), packed).unwrap();
        receipt["captures"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"file":file,"overlay":status}));
    }
    let (ah, bh) = (a.hwnd, b.hwnd);
    drop(a);
    drop(b);
    let cleanup = unsafe { (IsWindow(ah) == 0, IsWindow(bh) == 0) };
    receipt["cleanup"] = serde_json::json!({"owner_thread":unsafe {windows_sys::Win32::System::Threading::GetCurrentThreadId()},"a_hwnd_absent":cleanup.0,"b_hwnd_absent":cleanup.1});
    fs::write(
        directory.join("gpu-rasterization.json"),
        serde_json::to_vec_pretty(&receipt).unwrap(),
    )
    .unwrap();
    assert!(cleanup.0 && cleanup.1);
    assert!(!guard.pump());
}
