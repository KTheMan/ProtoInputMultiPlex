//! Owner-thread GPU rasterization/presentation and lazy GDI compatibility.
//! No GUI graphics context is shared with routing.
use crate::{cursor_rasterizer::Rasterizer, overlay::RenderRecipe};
use anyhow::{Context, Result, ensure};
use mux_core::desktop::CursorRenderer;
use serde::{Deserialize, Serialize};
use std::{marker::PhantomData, rc::Rc};
use windows::{
    Win32::{
        Foundation::{HMODULE, HWND as ComHwnd},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_HARDWARE,
            Direct3D11::*,
            DirectComposition::*,
            Dxgi::{Common::*, *},
        },
    },
    core::Interface,
};
use windows_sys::Win32::{Foundation::*, Graphics::Gdi::*, UI::WindowsAndMessaging::*};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PresentationStatus {
    /// `pending`, `direct_composition_d3d11`, or `gdi`.
    pub backend: String,
    pub adapter: Option<String>,
    pub fallback_reason: Option<String>,
    /// Legacy final-canvas upload count; GPU rasterization never increments it.
    pub gpu_uploads: u64,
    pub gpu_presents: u64,
    pub composition_commits: u64,
    #[serde(default)]
    pub raster_backend: RasterBackend,
    #[serde(default)]
    pub gpu_draws: u64,
    #[serde(default)]
    pub asset_uploads: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RasterBackend {
    #[default]
    Unknown,
    #[serde(rename = "direct2d")]
    Direct2D,
    Gdi,
}

impl Default for PresentationStatus {
    fn default() -> Self {
        Self {
            backend: "pending".into(),
            adapter: None,
            fallback_reason: None,
            gpu_uploads: 0,
            gpu_presents: 0,
            composition_commits: 0,
            raster_backend: RasterBackend::Unknown,
            gpu_draws: 0,
            asset_uploads: 0,
        }
    }
}

pub(crate) struct DibCanvas {
    pub(crate) dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    pub(crate) bits: *mut core::ffi::c_void,
    pub(crate) size: (i32, i32),
}

impl DibCanvas {
    pub(crate) fn new(width: i32, height: i32) -> Result<Self> {
        ensure!(
            (1..=1024).contains(&width) && (1..=1024).contains(&height),
            "invalid cursor canvas"
        );
        unsafe {
            let dc = CreateCompatibleDC(std::ptr::null_mut());
            ensure!(!dc.is_null(), "create cursor drawing context");
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..std::mem::zeroed()
                },
                ..std::mem::zeroed()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(
                dc,
                &info,
                DIB_RGB_COLORS,
                &mut bits,
                std::ptr::null_mut(),
                0,
            );
            if bitmap.is_null() || bits.is_null() {
                if !bitmap.is_null() {
                    DeleteObject(bitmap);
                }
                DeleteDC(dc);
                anyhow::bail!("create cursor bitmap");
            }
            let previous = SelectObject(dc, bitmap);
            if previous.is_null() || previous == (-1isize) as HGDIOBJ {
                DeleteObject(bitmap);
                DeleteDC(dc);
                anyhow::bail!("select cursor bitmap");
            }
            Ok(Self {
                dc,
                bitmap,
                previous,
                bits,
                size: (width, height),
            })
        }
    }
}

impl Drop for DibCanvas {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

/// Owns its independent D3D device/context and composition tree on the HWND owner.
/// Never Send/Sync even though some COM interfaces permit cross-thread calls.
pub(crate) struct Presenter {
    gpu: Option<Gpu>,
    status: PresentationStatus,
    generation: Option<u64>,
    preference: CursorRenderer,
    _thread: PhantomData<Rc<()>>,
    #[cfg(test)]
    pub(crate) fail_next_present: bool,
}

impl Presenter {
    pub(crate) fn new(hwnd: HWND) -> Self {
        Self::initialize(hwnd, Gpu::new)
    }

    fn initialize(hwnd: HWND, create: impl FnOnce(HWND) -> Result<Gpu>) -> Self {
        let mut status = PresentationStatus {
            backend: "pending".into(),
            ..Default::default()
        };
        let gpu = match create(hwnd) {
            Ok(gpu) => {
                status.adapter = Some(gpu.adapter.clone());
                Some(gpu)
            }
            Err(error) => {
                status.backend = "gdi".into();
                status.fallback_reason = Some(format!("GPU initialization: {error:#}"));
                None
            }
        };
        Self {
            gpu,
            status,
            generation: None,
            preference: CursorRenderer::Auto,
            _thread: PhantomData,
            #[cfg(test)]
            fail_next_present: false,
        }
    }

    #[cfg(any(test, feature = "cursor-presentation-evidence"))]
    pub(crate) fn forced_gdi(hwnd: HWND) -> Self {
        Self::initialize(hwnd, |_| anyhow::bail!("injected initialization failure"))
    }

    pub(crate) fn status(&self) -> PresentationStatus {
        self.status.clone()
    }

    pub(crate) fn set_renderer(&mut self, hwnd: HWND, preference: CursorRenderer) -> bool {
        if self.preference == preference {
            return false;
        }
        self.gpu.take(); // Detach before another backend uses the same HWND.
        self.preference = preference;
        self.generation = None;
        self.status.raster_backend = RasterBackend::Unknown;
        self.status.fallback_reason = None;
        if preference == CursorRenderer::Gdi {
            self.status.backend = "gdi".into();
        } else {
            match Gpu::new(hwnd) {
                Ok(gpu) => {
                    self.status.adapter = Some(gpu.adapter.clone());
                    self.gpu = Some(gpu);
                    self.status.backend = "pending".into();
                }
                Err(error) => self.fallback(format!("GPU initialization: {error:#}")),
            }
        }
        true
    }

    fn fallback(&mut self, reason: String) {
        self.gpu.take();
        self.generation = None;
        self.status.backend = "gdi".into();
        self.status.fallback_reason = Some(reason);
    }

    /// Position-only reuse still notices device loss, but never draws/presents.
    pub(crate) fn reuse_gpu(&mut self, generation: u64) -> bool {
        #[cfg(test)]
        if self.gpu.is_some() && std::mem::take(&mut self.fail_next_present) {
            self.fallback("injected cached device health failure".into());
            return false;
        }
        if let Some(gpu) = self.gpu.as_ref() {
            if let Err(error) = gpu.check_health() {
                self.fallback(format!("GPU device health: {error:#}"));
                return false;
            }
            return self.generation == Some(generation);
        }
        false
    }

    pub(crate) fn rasterize(
        &mut self,
        hwnd: HWND,
        recipe: &RenderRecipe,
        origin: (i32, i32),
        generation: u64,
    ) -> bool {
        let Some(gpu) = self.gpu.as_mut() else {
            return false;
        };
        let result = (|| -> Result<()> {
            #[cfg(test)]
            if std::mem::take(&mut self.fail_next_present) {
                anyhow::bail!("injected presentation failure");
            }
            gpu.check_health()?;
            gpu.draw(hwnd, recipe, origin)?;
            #[cfg(any(test, feature = "cursor-presentation-evidence"))]
            if gpu.capture_submissions && gpu.submitted_buffer.is_some() {
                gpu.submitted_generation = Some(generation);
            }
            Ok(())
        })();
        // Asset creation is real even if a later draw/present fails.
        self.status.asset_uploads += gpu.rasterizer.asset_uploads;
        gpu.rasterizer.asset_uploads = 0;
        if let Err(error) = result {
            self.fallback(format!("GPU rasterization: {error:#}"));
            return false;
        }
        self.status.gpu_draws += 1;
        self.status.gpu_presents += 1;
        self.status.composition_commits += 1;
        self.status.backend = "direct_composition_d3d11".into();
        self.status.raster_backend = RasterBackend::Direct2D;
        self.generation = Some(generation);
        true
    }

    /// CPU compatibility replay only, after GPU ownership has been detached.
    pub(crate) fn present(
        &mut self,
        hwnd: HWND,
        canvas: &DibCanvas,
        size: (i32, i32),
        origin: (i32, i32),
        generation: u64,
    ) -> Result<()> {
        ensure!(
            size == canvas.size,
            "cursor presenter canvas dimensions mismatch"
        );
        ensure!(
            self.gpu.is_none(),
            "GDI replay requires a detached GPU presenter"
        );
        if self.generation != Some(generation) {
            layered(hwnd, canvas.dc, size, origin)?;
            self.generation = Some(generation);
        }
        self.status.backend = "gdi".into();
        self.status.raster_backend = RasterBackend::Gdi;
        Ok(())
    }

    /// Explicit release is called before DestroyWindow, on the owner thread.
    pub(crate) fn shutdown(&mut self) {
        self.gpu.take();
    }

    #[cfg(test)]
    pub(crate) fn gpu(&self) -> Option<&Gpu> {
        self.gpu.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn enable_readback(&mut self) {
        self.set_readback_enabled(true);
    }

    /// Enables staging only after the caller reserves evidence capacity; callers
    /// should disable it after reading the single GPU draw selected for that reservation.
    #[cfg(any(test, feature = "cursor-presentation-evidence"))]
    pub(crate) fn set_readback_enabled(&mut self, enabled: bool) {
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.capture_submissions = enabled;
            gpu.submitted_buffer = None;
            gpu.submitted_generation = None;
            gpu.pending_readback_error = None;
        }
    }

    /// Reads the exact currently submitted GPU generation after commit acknowledgement.
    #[cfg(any(test, feature = "cursor-presentation-evidence"))]
    pub(crate) fn readback_for_generation(&self, generation: u64) -> Result<(Vec<u32>, u32, i64)> {
        ensure!(
            self.generation == Some(generation)
                && self.status.backend == "direct_composition_d3d11",
            "requested cursor generation is not the current GPU presentation"
        );
        self.gpu
            .as_ref()
            .context("GPU cursor presenter is unavailable")?
            .readback_for_generation(generation)
    }
}

fn layered(hwnd: HWND, dc: HDC, size: (i32, i32), origin: (i32, i32)) -> Result<()> {
    let pos = POINT {
        x: origin.0,
        y: origin.1,
    };
    let size = SIZE {
        cx: size.0,
        cy: size.1,
    };
    let source = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    ensure!(
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                std::ptr::null_mut(),
                &pos,
                &size,
                dc,
                &source,
                0,
                &blend,
                ULW_ALPHA,
            )
        } != 0,
        "compose cursor backing: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

/// Exact-size resources: reuse never scales or pads the cursor image. The GPU
/// keeps at most the active surface and one inactive, most recently used size.
struct CompositionSurface {
    size: (i32, i32),
    swapchain: IDXGISwapChain1,
    backing: DibCanvas,
}

pub(crate) struct Gpu {
    // Drop uses these while the owned HWND is still alive.
    composition: IDCompositionDevice,
    target: IDCompositionTarget,
    visual: IDCompositionVisual,
    surface: Option<CompositionSurface>,
    spare_surface: Option<CompositionSurface>,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    factory: IDXGIFactory2,
    adapter: String,
    rasterizer: Rasterizer,
    #[cfg(test)]
    resource_creations: u64,
    #[cfg(test)]
    resource_reuses: u64,
    #[cfg(any(test, feature = "cursor-presentation-evidence"))]
    submitted_buffer: Option<ID3D11Texture2D>,
    #[cfg(any(test, feature = "cursor-presentation-evidence"))]
    capture_submissions: bool,
    #[cfg(any(test, feature = "cursor-presentation-evidence"))]
    submitted_generation: Option<u64>,
    #[cfg(any(test, feature = "cursor-presentation-evidence"))]
    pending_readback_error: Option<String>,
}

impl Gpu {
    fn new(hwnd: HWND) -> Result<Self> {
        unsafe {
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .context("create hardware D3D11 device")?;
            let device = device.context("missing D3D11 device")?;
            let context = context.context("missing D3D11 immediate context")?;
            let dxgi: IDXGIDevice = device.cast()?;
            let rasterizer = Rasterizer::new(&dxgi)?;
            let adapter = dxgi.GetAdapter()?;
            let desc = adapter.cast::<IDXGIAdapter1>()?.GetDesc1()?;
            ensure!(
                desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0,
                "software adapter is not GPU presentation"
            );
            let adapter_name = String::from_utf16_lossy(
                &desc.Description[..desc
                    .Description
                    .iter()
                    .position(|v| *v == 0)
                    .unwrap_or(desc.Description.len())],
            );
            let factory: IDXGIFactory2 = adapter.GetParent()?;
            let composition: IDCompositionDevice = DCompositionCreateDevice(&dxgi)?;
            let target = composition.CreateTargetForHwnd(ComHwnd(hwnd), true)?;
            let visual = composition.CreateVisual()?;
            target.SetRoot(&visual)?;
            Ok(Self {
                composition,
                target,
                visual,
                surface: None,
                spare_surface: None,
                device,
                context,
                factory,
                adapter: adapter_name,
                rasterizer,
                #[cfg(test)]
                resource_creations: 0,
                #[cfg(test)]
                resource_reuses: 0,
                #[cfg(any(test, feature = "cursor-presentation-evidence"))]
                submitted_buffer: None,
                #[cfg(any(test, feature = "cursor-presentation-evidence"))]
                capture_submissions: false,
                #[cfg(any(test, feature = "cursor-presentation-evidence"))]
                submitted_generation: None,
                #[cfg(any(test, feature = "cursor-presentation-evidence"))]
                pending_readback_error: None,
            })
        }
    }

    fn create_surface(&self, size: (i32, i32)) -> Result<CompositionSurface> {
        unsafe {
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: size.0 as u32,
                Height: size.1 as u32,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                ..Default::default()
            };
            let swapchain =
                self.factory
                    .CreateSwapChainForComposition(&self.device, &desc, None)?;
            let backing = DibCanvas::new(size.0, size.1)?;
            std::ptr::write_bytes(backing.bits, 0, (size.0 * size.1 * 4) as usize);
            Ok(CompositionSurface {
                size,
                swapchain,
                backing,
            })
        }
    }

    fn activate_surface(&mut self, hwnd: HWND, size: (i32, i32), origin: (i32, i32)) -> Result<()> {
        if self
            .spare_surface
            .as_ref()
            .is_some_and(|surface| surface.size == size)
        {
            std::mem::swap(&mut self.surface, &mut self.spare_surface);
            #[cfg(test)]
            {
                self.resource_reuses += 1;
            }
        } else {
            // Evict before allocating so a miss does not temporarily retain a
            // third application-owned chain/backing pair. A creation error
            // still selects the existing GDI fallback on this same call.
            self.spare_surface = None;
            let surface = self.create_surface(size)?;
            self.spare_surface = self.surface.replace(surface);
            #[cfg(test)]
            {
                self.resource_creations += 1;
            }
        }
        let surface = self
            .surface
            .as_ref()
            .context("missing active cursor surface")?;
        unsafe {
            self.visual.SetContent(&surface.swapchain)?;
        }
        // Restore a full-size transparent HWND backing on every size switch.
        // It is never a CPU mirror, and cached backings remain entirely zero.
        layered(hwnd, surface.backing.dc, size, origin)
    }

    fn check_health(&self) -> Result<()> {
        unsafe {
            self.device
                .GetDeviceRemovedReason()
                .context("D3D11 device lost")?;
            ensure!(
                self.composition.CheckDeviceState()?.as_bool(),
                "DirectComposition device lost"
            );
        }
        Ok(())
    }

    fn draw(&mut self, hwnd: HWND, recipe: &RenderRecipe, origin: (i32, i32)) -> Result<()> {
        #[cfg(any(test, feature = "cursor-presentation-evidence"))]
        {
            self.submitted_buffer = None;
            self.submitted_generation = None;
            self.pending_readback_error = None;
        }
        let size = recipe.layout.size;
        ensure!(
            (1..=1024).contains(&size.0) && (1..=1024).contains(&size.1),
            "invalid GPU cursor extent"
        );
        if self.surface.as_ref().is_none_or(|s| s.size != size) {
            self.activate_surface(hwnd, size, origin)?;
        }
        unsafe {
            let swapchain = &self
                .surface
                .as_ref()
                .context("missing cursor surface")?
                .swapchain;
            let buffer: ID3D11Texture2D = swapchain.GetBuffer(0)?;
            let dxgi_surface: IDXGISurface = buffer.cast()?;
            self.rasterizer.draw(&dxgi_surface, recipe)?;
            drop(dxgi_surface); // D2D target is already unbound before copy/flip/cache changes.
            self.check_health()?;
            #[cfg(any(test, feature = "cursor-presentation-evidence"))]
            if self.capture_submissions {
                match self.snapshot_buffer(&buffer) {
                    Ok(snapshot) => self.submitted_buffer = Some(snapshot),
                    Err(error) => self.pending_readback_error = Some(format!("{error:#}")),
                }
            }
            let result = swapchain.Present(0, DXGI_PRESENT(0));
            ensure!(
                result == windows::core::HRESULT(0),
                "DXGI cursor Present did not present: {result:?}"
            );
            self.composition
                .Commit()
                .context("commit GPU cursor artwork")?;
            self.check_health()?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn resource_cache_evidence(&self) -> (usize, u64, u64) {
        (
            usize::from(self.surface.is_some()) + usize::from(self.spare_surface.is_some()),
            self.resource_creations,
            self.resource_reuses,
        )
    }

    #[cfg(any(test, feature = "cursor-presentation-evidence"))]
    fn snapshot_buffer(&self, buffer: &ID3D11Texture2D) -> Result<ID3D11Texture2D> {
        unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            buffer.GetDesc(&mut desc);
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.MiscFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            let mut staging = None;
            self.device
                .CreateTexture2D(&desc, None, Some(&mut staging))?;
            let staging = staging.context("missing GPU readback resource")?;
            self.context.CopyResource(&staging, buffer);
            Ok(staging)
        }
    }

    #[cfg(any(test, feature = "cursor-presentation-evidence"))]
    pub(crate) fn readback(&self) -> Result<Vec<u32>> {
        if let Some(error) = &self.pending_readback_error {
            anyhow::bail!("cursor GPU evidence snapshot failed: {error}");
        }
        unsafe {
            let staging = self
                .submitted_buffer
                .as_ref()
                .context("no submitted GPU snapshot")?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            staging.GetDesc(&mut desc);
            ensure!(
                (1..=1024).contains(&desc.Width) && (1..=1024).contains(&desc.Height),
                "invalid cursor GPU readback extent"
            );
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let _mapped = MappedStaging::new(&self.context, staging);
            ensure!(
                !mapped.pData.is_null(),
                "cursor GPU readback returned null data"
            );
            let width = usize::try_from(desc.Width).context("cursor readback width overflow")?;
            let height = usize::try_from(desc.Height).context("cursor readback height overflow")?;
            let row_bytes = width
                .checked_mul(std::mem::size_of::<u32>())
                .context("cursor readback row byte count overflow")?;
            let row_pitch =
                usize::try_from(mapped.RowPitch).context("cursor readback row pitch overflow")?;
            ensure!(
                row_pitch >= row_bytes,
                "cursor GPU readback row pitch is too small"
            );
            let pixel_count = width
                .checked_mul(height)
                .context("cursor readback pixel count overflow")?;
            let output_bytes = pixel_count
                .checked_mul(std::mem::size_of::<u32>())
                .context("cursor readback byte count overflow")?;
            let source_bytes = (height - 1)
                .checked_mul(row_pitch)
                .and_then(|offset| offset.checked_add(row_bytes))
                .context("cursor readback source extent overflow")?;
            ensure!(
                source_bytes >= row_bytes
                    && source_bytes <= isize::MAX as usize
                    && output_bytes <= isize::MAX as usize,
                "invalid cursor GPU readback byte extent"
            );
            let mut pixels = Vec::new();
            pixels
                .try_reserve_exact(pixel_count)
                .context("reserve cursor GPU readback pixels")?;
            pixels.resize(pixel_count, 0);
            let source = mapped.pData.cast::<u8>();
            for row in 0..height {
                let source_offset = row
                    .checked_mul(row_pitch)
                    .context("cursor readback source offset overflow")?;
                let destination_offset = row
                    .checked_mul(width)
                    .context("cursor readback destination offset overflow")?;
                std::ptr::copy_nonoverlapping(
                    source.add(source_offset),
                    pixels.as_mut_ptr().add(destination_offset).cast::<u8>(),
                    row_bytes,
                );
            }
            Ok(pixels)
        }
    }

    #[cfg(any(test, feature = "cursor-presentation-evidence"))]
    pub(crate) fn wait_for_commit_completion(&self) -> Result<()> {
        unsafe {
            self.composition.WaitForCommitCompletion()?;
        }
        Ok(())
    }

    #[cfg(any(test, feature = "cursor-presentation-evidence"))]
    pub(crate) fn presentation_evidence(&self) -> Result<(u32, i64)> {
        self.wait_for_commit_completion()?;
        unsafe {
            let count = self
                .surface
                .as_ref()
                .context("missing cursor surface")?
                .swapchain
                .GetLastPresentCount()?;
            let statistics = self.composition.GetFrameStatistics()?;
            Ok((count, statistics.lastFrameTime))
        }
    }

    #[cfg(any(test, feature = "cursor-presentation-evidence"))]
    fn readback_for_generation(&self, generation: u64) -> Result<(Vec<u32>, u32, i64)> {
        if let Some(error) = &self.pending_readback_error {
            anyhow::bail!("cursor GPU evidence snapshot failed: {error}");
        }
        ensure!(
            self.submitted_generation == Some(generation),
            "no GPU readback is available for the requested cursor generation"
        );
        let (present_count, frame_time) = self.presentation_evidence()?;
        let pixels = self.readback()?;
        Ok((pixels, present_count, frame_time))
    }
}

#[cfg(any(test, feature = "cursor-presentation-evidence"))]
struct MappedStaging<'a> {
    context: &'a ID3D11DeviceContext,
    resource: &'a ID3D11Texture2D,
}

#[cfg(any(test, feature = "cursor-presentation-evidence"))]
impl<'a> MappedStaging<'a> {
    fn new(context: &'a ID3D11DeviceContext, resource: &'a ID3D11Texture2D) -> Self {
        Self { context, resource }
    }
}

#[cfg(any(test, feature = "cursor-presentation-evidence"))]
impl Drop for MappedStaging<'_> {
    fn drop(&mut self) {
        unsafe {
            self.context.Unmap(self.resource, 0);
        }
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        unsafe {
            let _ = self.target.SetRoot(None);
            let _ = self.composition.Commit();
            self.context.ClearState();
            self.context.Flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presenter_rejects_an_extent_outside_its_canvas() {
        assert!(DibCanvas::new(0, 1).is_err());
        assert!(DibCanvas::new(1025, 1).is_err());
        let canvas = DibCanvas::new(2, 2).unwrap();
        let mut presenter = Presenter::forced_gdi(std::ptr::null_mut());
        let error = presenter
            .present(std::ptr::null_mut(), &canvas, (3, 2), (0, 0), 1)
            .unwrap_err();
        assert!(error.to_string().contains("dimensions mismatch"));
    }
}
