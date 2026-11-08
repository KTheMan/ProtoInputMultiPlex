//! Windows version metadata for this project's own binaries, not the native backend.
use std::{env, fs, io, path::PathBuf, process::Command};

pub fn embed(filename: &str, description: &str, library: bool) -> io::Result<()> {
    println!("cargo:rerun-if-changed=../../scripts/native/windows-version-resource.rs");
    println!("cargo:rerun-if-env-changed=PROTOINPUTMULTIPLEX_RC");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return Err(io::Error::other(
            "Windows version resources require the MSVC toolchain",
        ));
    }
    let version = env::var("CARGO_PKG_VERSION").map_err(io::Error::other)?;
    if !version
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b".-+".contains(&byte))
    {
        return Err(io::Error::other(
            "Invalid package version for Windows metadata",
        ));
    }
    let mut parts = Vec::with_capacity(4);
    for key in [
        "CARGO_PKG_VERSION_MAJOR",
        "CARGO_PKG_VERSION_MINOR",
        "CARGO_PKG_VERSION_PATCH",
    ] {
        let value = env::var(key).map_err(io::Error::other)?;
        parts.push(value.parse::<u16>().map_err(io::Error::other)?.to_string());
    }
    parts.push("0".into());
    let numeric_version = parts.join(",");
    let flags = u8::from(env::var("PROFILE").as_deref() == Ok("debug"))
        | (u8::from(
            !env::var("CARGO_PKG_VERSION_PRE")
                .unwrap_or_default()
                .is_empty(),
        ) << 1);
    let file_type = if library { 2 } else { 1 };
    let internal_name = std::path::Path::new(filename)
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("Invalid original filename for Windows metadata"))?;
    // Numeric constants avoid an SDK header dependency; RC still comes from the Windows SDK.
    let source = format!(
        r#"1 VERSIONINFO
FILEVERSION {numeric_version}
PRODUCTVERSION {numeric_version}
FILEFLAGSMASK 0x3f
FILEFLAGS {flags}
FILEOS 0x40004
FILETYPE {file_type}
FILESUBTYPE 0
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904B0"
        BEGIN
            VALUE "CompanyName", "ProtoInputMultiPlex contributors\0"
            VALUE "FileDescription", "{description}\0"
            VALUE "FileVersion", "{version}\0"
            VALUE "InternalName", "{internal_name}\0"
            VALUE "OriginalFilename", "{filename}\0"
            VALUE "ProductName", "ProtoInputMultiPlex\0"
            VALUE "ProductVersion", "{version}\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0409, 1200
    END
END
"#
    );
    let output = PathBuf::from(
        env::var_os("OUT_DIR").ok_or_else(|| io::Error::other("Missing Cargo OUT_DIR"))?,
    );
    let script = output.join("protoinputmultiplex-version.rc");
    let resource = output.join("protoinputmultiplex-version.res");
    fs::write(&script, source)?;
    let compiler = env::var_os("PROTOINPUTMULTIPLEX_RC").unwrap_or_else(|| "rc.exe".into());
    let status = Command::new(compiler)
        .arg("/nologo")
        .arg("/fo")
        .arg(&resource)
        .arg(&script)
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "Windows resource compiler failed: {status}"
        )));
    }
    if library {
        println!("cargo:rustc-link-arg-cdylib={}", resource.display());
    } else {
        println!(
            "cargo:rustc-link-arg-bin=protoinputmultiplex={}",
            resource.display()
        );
    }
    Ok(())
}
