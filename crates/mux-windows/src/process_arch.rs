//! Process architecture for the two supported Intel target widths.
//!
//! IsWow64Process2 is resolved at run time so Windows versions before 10 1709
//! can still use the existing IsWow64Process/GetNativeSystemInfo path.
use anyhow::{Result, bail, ensure};
use windows_sys::Win32::{
    Foundation::HANDLE,
    System::{
        LibraryLoader::{GetModuleHandleW, GetProcAddress},
        SystemInformation::{
            GetNativeSystemInfo, IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_I386,
            IMAGE_FILE_MACHINE_UNKNOWN, OSVERSIONINFOW, PROCESSOR_ARCHITECTURE_AMD64,
            PROCESSOR_ARCHITECTURE_INTEL, SYSTEM_INFO,
        },
        Threading::IsWow64Process,
    },
};

type IsWow64Process2 = unsafe extern "system" fn(HANDLE, *mut u16, *mut u16) -> i32;
type RtlGetVersion = unsafe extern "system" fn(*mut OSVERSIONINFOW) -> i32;
const _: () = assert!(std::mem::size_of::<OSVERSIONINFOW>() == 276);

pub(crate) fn process_bits(process: HANDLE) -> Result<u32> {
    let kernel32: Vec<u16> = "kernel32.dll\0".encode_utf16().collect();
    unsafe {
        let module = GetModuleHandleW(kernel32.as_ptr());
        ensure!(
            !module.is_null(),
            "locate kernel32 for process architecture"
        );
        if let Some(symbol) = GetProcAddress(module, c"IsWow64Process2".as_ptr().cast()) {
            // FARPROC is an untyped function pointer; its Windows system ABI is
            // the same as the resolved export's documented signature.
            let query: IsWow64Process2 = std::mem::transmute(symbol);
            let mut process_machine = 0;
            let mut native_machine = 0;
            ensure!(
                query(process, &mut process_machine, &mut native_machine) != 0,
                "query process architecture: {}",
                std::io::Error::last_os_error()
            );
            return classify_machines(process_machine, native_machine);
        }

        // GetNativeSystemInfo may report an emulated Intel architecture on
        // ARM64. Use this older pair only on a verified pre-1709 release, when
        // x86 emulation on ARM64 was not a supported Windows host capability.
        ensure_legacy_release()?;
        let mut wow64 = 0;
        ensure!(
            IsWow64Process(process, &mut wow64) != 0,
            "query WOW64 process: {}",
            std::io::Error::last_os_error()
        );
        let mut system = SYSTEM_INFO::default();
        GetNativeSystemInfo(&mut system);
        classify_legacy(
            wow64 != 0,
            system.Anonymous.Anonymous.wProcessorArchitecture,
        )
    }
}

fn ensure_legacy_release() -> Result<()> {
    let ntdll: Vec<u16> = "ntdll.dll\0".encode_utf16().collect();
    unsafe {
        let module = GetModuleHandleW(ntdll.as_ptr());
        ensure!(!module.is_null(), "cannot verify legacy Windows release");
        let symbol = GetProcAddress(module, c"RtlGetVersion".as_ptr().cast())
            .ok_or_else(|| anyhow::anyhow!("cannot verify legacy Windows release"))?;
        // This user-mode ntdll signature/layout is also used by the .NET runtime.
        let query: RtlGetVersion = std::mem::transmute(symbol);
        let mut version = OSVERSIONINFOW {
            dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
            ..Default::default()
        };
        ensure!(
            query(&mut version) == 0
                && legacy_release_allowed(
                    version.dwMajorVersion,
                    version.dwMinorVersion,
                    version.dwBuildNumber
                ),
            "process architecture is unavailable on this Windows release"
        );
    }
    Ok(())
}

fn legacy_release_allowed(major: u32, minor: u32, build: u32) -> bool {
    match (major, minor) {
        (6, 0..=3) => true,
        (10, 0) => matches!(build, 10240 | 10586 | 14393 | 15063),
        _ => false,
    }
}

fn classify_machines(process_machine: u16, native_machine: u16) -> Result<u32> {
    match (process_machine, native_machine) {
        (IMAGE_FILE_MACHINE_UNKNOWN, IMAGE_FILE_MACHINE_I386) => Ok(32),
        (IMAGE_FILE_MACHINE_UNKNOWN, IMAGE_FILE_MACHINE_AMD64) => Ok(64),
        (IMAGE_FILE_MACHINE_I386, IMAGE_FILE_MACHINE_AMD64) => Ok(32),
        _ => bail!(
            "unsupported process/native machine pair {process_machine:#06x}/{native_machine:#06x}"
        ),
    }
}

fn classify_legacy(wow64: bool, native_arch: u16) -> Result<u32> {
    match (wow64, native_arch) {
        (false, PROCESSOR_ARCHITECTURE_INTEL) => Ok(32),
        (false, PROCESSOR_ARCHITECTURE_AMD64) => Ok(64),
        (true, PROCESSOR_ARCHITECTURE_AMD64) => Ok(32),
        _ => bail!("unsupported WOW64/native processor pair {wow64}/{native_arch}"),
    }
}

pub(crate) fn ensure_host_target(bits: u32) -> Result<()> {
    host_target_compatible(std::env::consts::ARCH, bits)
}

fn host_target_compatible(host: &str, bits: u32) -> Result<()> {
    match (host, bits) {
        ("x86", 32) | ("x86_64", 32 | 64) => Ok(()),
        ("x86", 64) => bail!("an x86 host cannot attach to a 64-bit target"),
        _ => bail!("unsupported host/target architecture"),
    }
}

pub(crate) fn loader_name() -> Result<&'static str> {
    loader_name_for(std::env::consts::ARCH)
}

fn loader_name_for(host: &str) -> Result<&'static str> {
    match host {
        "x86" => Ok("ProtoInputLoader32.dll"),
        "x86_64" => Ok("ProtoInputLoader64.dll"),
        _ => bail!("ProtoInput backend requires an x86 or x64 host"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_and_native_machines_are_both_required() {
        assert_eq!(
            classify_machines(IMAGE_FILE_MACHINE_UNKNOWN, IMAGE_FILE_MACHINE_I386).unwrap(),
            32
        );
        assert_eq!(
            classify_machines(IMAGE_FILE_MACHINE_I386, IMAGE_FILE_MACHINE_AMD64).unwrap(),
            32
        );
        assert_eq!(
            classify_machines(IMAGE_FILE_MACHINE_UNKNOWN, IMAGE_FILE_MACHINE_AMD64).unwrap(),
            64
        );
        for (process, native) in [
            (0, 0),
            (IMAGE_FILE_MACHINE_I386, IMAGE_FILE_MACHINE_I386),
            (IMAGE_FILE_MACHINE_UNKNOWN, 0xaa64),
            (0xaa64, IMAGE_FILE_MACHINE_AMD64),
        ] {
            assert!(classify_machines(process, native).is_err());
        }
    }

    #[test]
    fn legacy_classification_is_unambiguous_on_intel() {
        assert_eq!(
            classify_legacy(false, PROCESSOR_ARCHITECTURE_INTEL).unwrap(),
            32
        );
        assert_eq!(
            classify_legacy(true, PROCESSOR_ARCHITECTURE_AMD64).unwrap(),
            32
        );
        assert_eq!(
            classify_legacy(false, PROCESSOR_ARCHITECTURE_AMD64).unwrap(),
            64
        );
        assert!(classify_legacy(true, PROCESSOR_ARCHITECTURE_INTEL).is_err());
        assert!(classify_legacy(false, 12).is_err());
        assert!(classify_legacy(true, 12).is_err());
    }

    #[test]
    fn legacy_fallback_requires_a_known_pre_1709_release() {
        for (major, minor, build) in [
            (6, 0, 6002),
            (6, 1, 7601),
            (6, 2, 9200),
            (6, 3, 9600),
            (10, 0, 10240),
            (10, 0, 10586),
            (10, 0, 14393),
            (10, 0, 15063),
        ] {
            assert!(legacy_release_allowed(major, minor, build));
        }
        for (major, minor, build) in [
            (0, 0, 0),
            (10, 0, 16299),
            (10, 0, 19045),
            (10, 0, 26100),
            (10, 0, 15062),
            (11, 0, 100),
            (6, 4, 10000),
        ] {
            assert!(!legacy_release_allowed(major, minor, build));
        }
    }

    #[test]
    fn host_compatibility_rejects_cross_width_x86_host() {
        assert!(host_target_compatible("x86", 32).is_ok());
        assert!(host_target_compatible("x86", 64).is_err());
        assert!(host_target_compatible("x86_64", 32).is_ok());
        assert!(host_target_compatible("x86_64", 64).is_ok());
        assert!(host_target_compatible("arm", 32).is_err());
        assert!(host_target_compatible("x86_64", 0).is_err());
        assert_eq!(loader_name_for("x86").unwrap(), "ProtoInputLoader32.dll");
        assert_eq!(loader_name_for("x86_64").unwrap(), "ProtoInputLoader64.dll");
        assert!(loader_name_for("arm").is_err());
    }
}
