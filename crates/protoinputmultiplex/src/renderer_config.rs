// Pure renderer policy shared by the GUI and the opt-in renderer-only example.
#[cfg(feature = "software-renderer")]
use anyhow::Context;
use anyhow::Result;
#[cfg(feature = "software-renderer")]
use std::sync::Arc;

pub(crate) fn renderer_options(software_dx12: bool) -> Result<eframe::NativeOptions> {
    if !software_dx12 {
        return Ok(eframe::NativeOptions {
            renderer: eframe::Renderer::Glow,
            ..Default::default()
        });
    }
    #[cfg(feature = "software-renderer")]
    {
        Ok(software_renderer_options())
    }
    #[cfg(not(feature = "software-renderer"))]
    {
        anyhow::bail!(
            "Software DX12 renderer unavailable: build with the software-renderer feature; hardware fallback disabled"
        )
    }
}

#[cfg(feature = "software-renderer")]
const SOFTWARE_UNAVAILABLE: &str =
    "Software DX12 renderer unavailable: no compatible CPU adapter; hardware fallback disabled";

#[cfg(feature = "software-renderer")]
fn software_adapter_index(
    adapters: impl IntoIterator<Item = (eframe::wgpu::AdapterInfo, bool)>,
) -> Option<usize> {
    adapters
        .into_iter()
        .position(|(info, surface_supported)| software_adapter_info(&info) && surface_supported)
}

#[cfg(feature = "software-renderer")]
fn software_adapter_info(info: &eframe::wgpu::AdapterInfo) -> bool {
    use eframe::wgpu::{Backend, DeviceType};
    info.backend == Backend::Dx12 && info.device_type == DeviceType::Cpu
}

#[cfg(feature = "software-renderer")]
fn software_renderer_options() -> eframe::NativeOptions {
    use eframe::{egui_wgpu, wgpu};
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    // Replace all environment-derived instance configuration in strict software mode.
    // Adapter enumeration can still probe hardware drivers before this selector runs.
    setup.instance_descriptor = wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12,
        flags: wgpu::InstanceFlags::empty(),
        backend_options: Default::default(),
        memory_budget_thresholds: Default::default(),
        display: None,
    };
    setup.power_preference = wgpu::PowerPreference::None;
    setup.native_adapter_selector = Some(Arc::new(|adapters, surface| {
        let surface = surface.ok_or_else(|| SOFTWARE_UNAVAILABLE.to_owned())?;
        let index = software_adapter_index(adapters.iter().map(|adapter| {
            let info = adapter.get_info();
            let supported = software_adapter_info(&info) && adapter.is_surface_supported(surface);
            (info, supported)
        }))
        .ok_or_else(|| SOFTWARE_UNAVAILABLE.to_owned())?;
        Ok(adapters[index].clone())
    }));
    eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: egui_wgpu::WgpuConfiguration {
            wgpu_setup: setup.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub(crate) fn verify_renderer(software_dx12: bool, cc: &eframe::CreationContext<'_>) -> Result<()> {
    if !software_dx12 {
        return Ok(());
    }
    #[cfg(feature = "software-renderer")]
    {
        let state = cc
            .wgpu_render_state
            .as_ref()
            .context(SOFTWARE_UNAVAILABLE)?;
        let info = state.adapter.get_info();
        anyhow::ensure!(
            software_adapter_info(&info),
            "{SOFTWARE_UNAVAILABLE}: actual backend={:?}, device_type={:?}",
            info.backend,
            info.device_type
        );
        eprintln!(
            "GUI software adapter selected: backend={:?}, device_type={:?}, name={:?}, vendor={:#x}, device={:#x}, driver={:?}, driver_info={:?}; rendering and presentation not yet verified",
            info.backend,
            info.device_type,
            info.name,
            info.vendor,
            info.device,
            info.driver,
            info.driver_info
        );
        Ok(())
    }
    #[cfg(not(feature = "software-renderer"))]
    {
        let _ = cc;
        anyhow::bail!(
            "Software DX12 renderer unavailable: build with the software-renderer feature; hardware fallback disabled"
        )
    }
}

#[cfg(test)]
mod renderer_tests {
    use super::*;

    #[test]
    fn default_mode_remains_glow() {
        assert_eq!(
            renderer_options(false).unwrap().renderer,
            eframe::Renderer::Glow
        );
    }

    #[cfg(not(feature = "software-renderer"))]
    #[test]
    fn software_request_without_feature_is_rejected() {
        let error = renderer_options(true)
            .err()
            .expect("missing feature must fail");
        assert!(error.to_string().contains("hardware fallback disabled"));
    }

    #[cfg(feature = "software-renderer")]
    fn info(
        backend: eframe::wgpu::Backend,
        device_type: eframe::wgpu::DeviceType,
    ) -> eframe::wgpu::AdapterInfo {
        eframe::wgpu::AdapterInfo {
            // Names must not override the backend/type policy.
            name: "Microsoft Basic Render Driver".to_owned(),
            vendor: 0,
            device: 0,
            device_type,
            device_pci_bus_id: String::new(),
            driver: String::new(),
            driver_info: String::new(),
            backend,
            subgroup_min_size: 4,
            subgroup_max_size: 4,
            transient_saves_memory: false,
        }
    }

    #[cfg(feature = "software-renderer")]
    #[test]
    fn rejects_non_cpu_non_dx12_and_incompatible_candidates() {
        use eframe::wgpu::{Backend, DeviceType};
        for kind in [
            DeviceType::DiscreteGpu,
            DeviceType::IntegratedGpu,
            DeviceType::VirtualGpu,
            DeviceType::Other,
        ] {
            assert_eq!(
                software_adapter_index([(info(Backend::Dx12, kind), true)]),
                None
            );
        }
        for backend in [Backend::Vulkan, Backend::Gl, Backend::Metal] {
            assert_eq!(
                software_adapter_index([(info(backend, DeviceType::Cpu), true)]),
                None
            );
        }
        assert_eq!(
            software_adapter_index([(info(Backend::Dx12, DeviceType::Cpu), false)]),
            None
        );
        assert_eq!(software_adapter_index([]), None);
    }

    #[cfg(feature = "software-renderer")]
    #[test]
    fn skips_hardware_and_selects_only_compatible_dx12_cpu() {
        use eframe::wgpu::{Backend, DeviceType};
        assert_eq!(
            software_adapter_index([
                (info(Backend::Dx12, DeviceType::DiscreteGpu), true),
                (info(Backend::Dx12, DeviceType::Cpu), false),
                (info(Backend::Dx12, DeviceType::Cpu), true),
            ]),
            Some(2)
        );
    }

    #[cfg(feature = "software-renderer")]
    #[test]
    fn software_configuration_is_explicit_and_does_not_create_an_instance() {
        use eframe::{egui_wgpu::WgpuSetup, wgpu};
        let options = renderer_options(true).unwrap();
        assert_eq!(options.renderer, eframe::Renderer::Wgpu);
        let WgpuSetup::CreateNew(setup) = options.wgpu_options.wgpu_setup else {
            panic!("strict software mode must install its selector");
        };
        assert_eq!(setup.instance_descriptor.backends, wgpu::Backends::DX12);
        assert!(setup.instance_descriptor.flags.is_empty());
        assert!(setup.native_adapter_selector.is_some());
        // Neither missing surface nor an empty adapter list may fall back.
        let selector = setup.native_adapter_selector.unwrap();
        assert!(
            selector(&[], None)
                .unwrap_err()
                .contains("hardware fallback disabled")
        );
    }
}
