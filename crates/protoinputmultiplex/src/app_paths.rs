//! Installation paths are anchored to the executable; user files to their chosen data root.
#[cfg(windows)]
use anyhow::{Context, Result};
use std::path::PathBuf;

pub fn runtime_dir() -> PathBuf {
    let executable_directory = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(std::path::Path::to_path_buf));
    if let Some(directory) = executable_directory
        && (directory.join("bundle-manifest.json").is_file()
            || directory
                .parent()
                .is_some_and(|root| root.join(".protoinputmultiplex-install.json").is_file()))
    {
        return directory.join("runtime");
    }
    // Preserve the development checkout workflow when this is not a distribution.
    PathBuf::from("runtime")
}

#[cfg(windows)]
pub fn default_data_dir() -> Result<PathBuf> {
    Ok(PathBuf::from(
        std::env::var_os("LOCALAPPDATA")
            .context("LOCALAPPDATA is unavailable; pass --data-dir or --preferences explicitly")?,
    )
    .join("ProtoInputMultiPlex"))
}

#[cfg(windows)]
pub fn preferences_path(data: Option<PathBuf>, preferences: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = preferences {
        return std::path::absolute(path).context("resolve preferences path");
    }
    let root = match data {
        Some(root) => std::path::absolute(root).context("resolve data directory")?,
        None => default_data_dir()?,
    };
    Ok(root.join("app-preferences.json"))
}
