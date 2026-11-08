#[path = "src/abi_contract.rs"]
mod abi_contract;
#[path = "../../scripts/native/windows-version-resource.rs"]
mod windows_version;

fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=src/abi_contract.rs");
    if std::env::var_os("CARGO_FEATURE_NATIVE_FFI").is_none() { return Ok(()); }
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    let native = std::env::var_os("CARGO_FEATURE_NATIVE_CLIENT").is_some()
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let header = if native { abi_contract::render_header_for(true) } else { abi_contract::render_header() };
    std::fs::write(output.join("protoinputmultiplex_application_sdk.h"), header)?;
    windows_version::embed("mux_application_sdk.dll", "ProtoInputMultiPlex application SDK", true)
}
