//! Stage the separate, offline OCR model bundle beside Cargo's executable outputs.
use std::{env, fs, io, path::Path};

#[path = "../../scripts/native/windows-version-resource.rs"]
mod windows_version;

const OCR_ASSETS: [&str; 6] = [
    "text-detection-ssfbcj81.rten",
    "text-rec-checkpoint-s52qdbqt.rten",
    "MODEL_CARD.md",
    "provenance.json",
    "LICENSE-CC-BY-SA-4.0.txt",
    "ATTRIBUTION.md",
];

fn main() -> io::Result<()> {
    windows_version::embed("protoinputmultiplex.exe", "ProtoInputMultiPlex", false)?;
    let manifest = env::var_os("CARGO_MANIFEST_DIR")
        .ok_or_else(|| io::Error::other("Cargo did not provide CARGO_MANIFEST_DIR"))?;
    let output =
        env::var_os("OUT_DIR").ok_or_else(|| io::Error::other("Cargo did not provide OUT_DIR"))?;
    // Cargo uses <profile>/build/<package-hash>/out, including target triples.
    let output = Path::new(&output);
    let build = output
        .parent()
        .and_then(Path::parent)
        .filter(|path| path.file_name().is_some_and(|name| name == "build"))
        .ok_or_else(|| io::Error::other("Unexpected Cargo OUT_DIR layout"))?;
    let profile = build
        .parent()
        .ok_or_else(|| io::Error::other("Cargo OUT_DIR has no profile directory"))?;
    let source = Path::new(&manifest).join("assets/ocr");
    let destination = profile.join("assets/ocr");
    fs::create_dir_all(&destination)?;
    for name in OCR_ASSETS {
        let input = source.join(name);
        println!("cargo:rerun-if-changed={}", input.display());
        fs::copy(input, destination.join(name))?;
    }
    Ok(())
}
