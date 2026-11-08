//! Bounded, validated profile JSON and atomic local file maintenance.
use anyhow::{Context, Result, ensure};
use mux_core::Profile;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub const MAX_PROFILE_BYTES: usize = 16 * 1024 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

/// Parse a bounded profile, materialize legacy IDs, and validate all settings.
pub fn decode(bytes: &[u8]) -> Result<Profile> {
    decode_with_report(bytes).map(|(profile, _)| profile)
}

pub(crate) fn decode_with_report(bytes: &[u8]) -> Result<(Profile, bool)> {
    ensure!(
        bytes.len() <= MAX_PROFILE_BYTES,
        "profile exceeds the 16 MiB size limit"
    );
    let mut profile: Profile = serde_json::from_slice(bytes).context("parse profile JSON")?;
    let normalized_ids = profile.seats.iter().any(|seat| seat.id.is_empty());
    profile.normalize_ids()?;
    Ok((profile, normalized_ids))
}

/// Serialize a validated profile with normalized IDs and a strict output bound.
pub fn encode(profile: &Profile) -> Result<Vec<u8>> {
    let mut normalized = profile.clone();
    normalized.normalize_ids()?;
    let bytes = serde_json::to_vec_pretty(&normalized).context("serialize profile JSON")?;
    ensure!(
        bytes.len() <= MAX_PROFILE_BYTES,
        "profile exceeds the 16 MiB size limit"
    );
    Ok(bytes)
}

fn read_bytes(path: &Path) -> Result<Vec<u8>> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let length = file
        .metadata()
        .with_context(|| format!("inspect {}", path.display()))?
        .len();
    ensure!(
        length <= MAX_PROFILE_BYTES as u64,
        "{} exceeds the 16 MiB size limit",
        path.display()
    );
    let mut bytes = Vec::with_capacity(length as usize);
    file.take(MAX_PROFILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {}", path.display()))?;
    ensure!(
        bytes.len() <= MAX_PROFILE_BYTES,
        "{} exceeds the 16 MiB size limit",
        path.display()
    );
    Ok(bytes)
}

pub fn read(path: &Path) -> Result<Profile> {
    decode(&read_bytes(path)?)
}

/// Append `.bak` to the full filename (`profile.json` becomes `profile.json.bak`).
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".bak");
    PathBuf::from(name)
}

struct TempPath {
    path: PathBuf,
    committed: bool,
}

impl Drop for TempPath {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .context("profile destination must have a filename")?;
    let mut created = None;
    for _ in 0..32 {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let mut temp_name = name.to_os_string();
        temp_name.push(format!(".tmp.{}.{}", std::process::id(), sequence));
        let temp_path = parent.join(temp_name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => {
                created = Some((
                    TempPath {
                        path: temp_path,
                        committed: false,
                    },
                    file,
                ));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("create temp beside {}", path.display()));
            }
        }
    }
    let (mut temp, mut file) = created.context("could not allocate a unique profile temp file")?;
    file.write_all(bytes)
        .with_context(|| format!("write temp profile for {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("flush temp profile for {}", path.display()))?;
    drop(file);
    atomic_replace(&temp.path, path)?;
    temp.committed = true;
    Ok(())
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let source_wide: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let moved = unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            windows_sys::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING
                | windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
        )
    };
    ensure!(
        moved != 0,
        "atomically replace {}: {}",
        destination.display(),
        std::io::Error::last_os_error()
    );
    Ok(())
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, destination: &Path) -> Result<()> {
    fs::rename(source, destination)
        .with_context(|| format!("atomically replace {}", destination.display()))
}

/// Save a normalized profile, preserving the exact prior valid bytes as `.bak`.
pub fn save(path: &Path, profile: &Profile) -> Result<()> {
    let bytes = encode(profile)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;

    if path.exists() {
        let previous = read_bytes(path)?;
        decode(&previous).with_context(|| {
            format!(
                "refuse to replace invalid existing profile {}",
                path.display()
            )
        })?;
        write_atomic(&backup_path(path), &previous)?;
    }
    write_atomic(path, &bytes)
}

/// Exit saving must not rotate a useful recovery backup when the exact
/// normalized profile is already durable (including immediately after Restore).
pub fn save_if_changed(path: &Path, profile: &Profile) -> Result<bool> {
    let expected = encode(profile)?;
    match read_bytes(path) {
        Ok(previous) if previous == expected => return Ok(false),
        Ok(_) => {}
        Err(error)
            if error.chain().any(|cause| {
                cause
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
            }) => {}
        Err(error) => return Err(error),
    }
    save(path, profile)?;
    Ok(true)
}

/// Restore the validated backup, retaining the current valid profile as the new backup.
pub fn restore(path: &Path) -> Result<Profile> {
    let backup = backup_path(path);
    let bytes = read_bytes(&backup)?;
    let profile =
        decode(&bytes).with_context(|| format!("validate backup {}", backup.display()))?;
    save(path, &profile)?;
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mux_core::Seat;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "protoinputmultiplex-profile-store-{}-{}",
                std::process::id(),
                NEXT_DIR.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn profile(name: &str) -> Profile {
        Profile {
            version: 1,
            concurrent: false,
            desktop: None,
            replication: Default::default(),
            forwarding: Default::default(),
            mapper: Default::default(),
            seats: vec![Seat {
                id: format!("{name}-id"),
                name: name.into(),
                mouse: format!("device-{name}"),
                keyboard: None,
                target_pid: 42,
                desktop_start: None,
                cursor: Default::default(),
                annotation: Default::default(),
                magnifier: Default::default(),
                output: Default::default(),
            }],
        }
    }

    #[test]
    fn decode_materializes_legacy_identity_and_encode_persists_it() {
        let mut old = profile("old");
        old.seats[0].id.clear();
        let bytes = serde_json::to_vec(&old).unwrap();
        let (normalized, changed) = decode_with_report(&bytes).unwrap();
        assert!(changed);
        assert!(!normalized.seats[0].id.is_empty());
        let encoded = encode(&old).unwrap();
        let reread = decode(&encoded).unwrap();
        assert_eq!(reread.seats[0].id, normalized.seats[0].id);
    }

    #[test]
    fn decode_rejects_oversized_input_before_json_parsing() {
        let oversized = vec![b' '; MAX_PROFILE_BYTES + 1];
        assert!(decode(&oversized).is_err());
    }

    #[test]
    fn unchanged_exit_save_preserves_the_restore_recovery_backup() {
        let temp = TempDir::new();
        let path = temp.0.join("profile.json");
        save(&path, &profile("B")).unwrap();
        save(&path, &profile("A")).unwrap();
        let restored = restore(&path).unwrap();
        assert_eq!(restored.seats[0].name, "B");
        let prior_main = fs::read(&path).unwrap();
        let recovery = fs::read(backup_path(&path)).unwrap();
        assert_eq!(decode(&recovery).unwrap().seats[0].name, "A");
        assert!(!save_if_changed(&path, &restored).unwrap());
        assert_eq!(fs::read(&path).unwrap(), prior_main);
        assert_eq!(fs::read(backup_path(&path)).unwrap(), recovery);
    }

    #[test]
    fn exit_save_creates_missing_file_but_refuses_invalid_existing_data() {
        let temp = TempDir::new();
        let path = temp.0.join("profile.json");
        assert!(save_if_changed(&path, &profile("A")).unwrap());
        assert!(!save_if_changed(&path, &profile("A")).unwrap());
        fs::write(&path, b"invalid profile").unwrap();
        assert!(save_if_changed(&path, &profile("B")).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"invalid profile");
    }

    #[test]
    fn save_and_restore_keep_exact_previous_valid_bytes_as_backup() {
        let temp = TempDir::new();
        let path = temp.0.join("profile.json");
        let mut legacy = profile("legacy");
        legacy.seats[0].id.clear();
        let original = serde_json::to_vec(&legacy).unwrap();
        fs::write(&path, &original).unwrap();

        save(&path, &profile("replacement")).unwrap();
        assert_eq!(fs::read(backup_path(&path)).unwrap(), original);
        let replaced = read(&path).unwrap();
        assert_eq!(replaced.seats[0].name, "replacement");

        let restored = restore(&path).unwrap();
        assert_eq!(restored.seats[0].name, "legacy");
        assert!(!restored.seats[0].id.is_empty());
        assert_eq!(read(&path).unwrap().seats[0].id, restored.seats[0].id);
        assert_eq!(
            read(&backup_path(&path)).unwrap().seats[0].name,
            "replacement"
        );
    }

    #[test]
    fn invalid_existing_profile_preserves_main_and_backup() {
        let temp = TempDir::new();
        let path = temp.0.join("profile.json");
        let backup = backup_path(&path);
        let invalid = b"{not json";
        let sentinel = b"do not overwrite stale backup";
        fs::write(&path, invalid).unwrap();
        fs::write(&backup, sentinel).unwrap();
        assert!(save(&path, &profile("new")).is_err());
        assert_eq!(fs::read(&path).unwrap(), invalid);
        assert_eq!(fs::read(&backup).unwrap(), sentinel);
    }

    #[test]
    fn failed_temp_write_does_not_leave_a_partial_destination() {
        let temp = TempDir::new();
        let blocker = temp.0.join("parent-is-a-file");
        fs::write(&blocker, b"file").unwrap();
        let path = blocker.join("profile.json");
        assert!(save(&path, &profile("new")).is_err());
        assert!(blocker.is_file());
    }

    #[cfg(windows)]
    #[test]
    fn failed_replace_keeps_open_main_and_exact_backup_and_cleans_temp() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

        let temp = TempDir::new();
        let path = temp.0.join("profile.json");
        let original = encode(&profile("old")).unwrap();
        fs::write(&path, &original).unwrap();
        let _held = OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&path)
            .unwrap();

        assert!(save(&path, &profile("replacement")).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read(backup_path(&path)).unwrap(), original);
        assert!(fs::read_dir(&temp.0).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("profile.json.tmp.")
        }));
    }
}
