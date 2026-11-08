#[path = "src/abi_contract.rs"]
mod abi_contract;
#[path = "../../scripts/native/windows-version-resource.rs"]
mod windows_version;

fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=src/abi_contract.rs");
    if std::env::var_os("CARGO_FEATURE_NATIVE_FFI").is_none() {
        return Ok(());
    }
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    std::fs::write(
        output.join("protoinputmultiplex_application_sdk.h"),
        abi_contract::render_header(),
    )
    .expect("write generated application SDK header");
    windows_version::embed(
        "mux_application_sdk.dll",
        "ProtoInputMultiPlex application SDK",
        true,
    )
}
