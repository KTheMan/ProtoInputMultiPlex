#[path = "../../scripts/native/windows-version-resource.rs"]
mod windows_version;

fn main() -> std::io::Result<()> {
    let filename = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86") => "protoinputmultiplex_cursor_observer32.dll",
        Ok("x86_64") => "protoinputmultiplex_cursor_observer64.dll",
        _ => "cursor_observer.dll",
    };
    windows_version::embed(filename, "ProtoInputMultiPlex cursor observer", true)
}
