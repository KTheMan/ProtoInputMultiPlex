//! Explicit, offline maintenance of the complete user-data directory.
//!
//! Document versions remain independently validated. Schema 2 adds a managed
//! directory marker and materializes legacy profile IDs without changing grants
//! or login preferences. Run only while every instance using this root is closed.
use crate::{app_preferences, profile_store};
use anyhow::{Context, Result, bail, ensure};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const SCHEMA: u32 = 2;
const MARKER: &str = "data-schema.json";
const MAX_FILE: usize = 32 * 1024 * 1024;
const MAX_TOTAL: usize = 256 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
static NEXT: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Inspect schema and validate known documents without changing any files.
    Inspect {
        #[arg(long)]
        data_dir: PathBuf,
    },
    /// Back up and migrate legacy data; all application instances must be closed.
    Migrate {
        #[arg(long)]
        data_dir: PathBuf,
        /// Fresh destination for a byte-preserving snapshot of previous data.
        #[arg(long)]
        backup_dir: PathBuf,
        /// Optional old portable bundle: import only data, profiles and macros.
        #[arg(long)]
        legacy_root: Option<PathBuf>,
    },
    /// Create a bounded, checksummed backup, including unknown files and corrupt data.
    Backup {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        backup_dir: PathBuf,
    },
    /// Validate an entire backup before replacing data; preserve previous bytes.
    Restore {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        backup: PathBuf,
        /// Fresh recovery snapshot of the current, possibly corrupt data.
        #[arg(long)]
        backup_dir: PathBuf,
    },
    /// Roll an interrupted migration/restore back to its original directory.
    Recover {
        #[arg(long)]
        data_dir: PathBuf,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    schema_version: u32,
}

#[derive(Default)]
struct Tree {
    directories: Vec<String>,
    files: Vec<(String, Vec<u8>)>,
    excluded: Vec<String>,
    bytes: usize,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    backup_version: u32,
    directories: Vec<String>,
    files: Vec<BackupFile>,
    excluded_private_descriptors: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BackupFile {
    path: String,
    bytes: usize,
    sha256: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    original_existed: bool,
    stage_name: String,
    original_name: String,
}

pub fn run(command: Command) -> Result<()> {
    let report = match command {
        Command::Inspect { data_dir } => {
            let root = resolve(&data_dir)?;
            reject_program_root(&root)?;
            let tree = collect(&root, true)?;
            let validation = validate(&tree);
            serde_json::json!({
                "data_dir": root, "schema_version": schema(&tree)?,
                "files": tree.files.len(), "bytes": tree.bytes,
                "valid": validation.is_ok(),
                "validation_error": validation.err().map(|error| format!("{error:#}")),
                "pending_recovery": journal_path(&root)?.exists(),
                "excluded_private_descriptors": tree.excluded,
            })
        }
        Command::Backup {
            data_dir,
            backup_dir,
        } => {
            let root = resolve(&data_dir)?;
            reject_program_root(&root)?;
            let destination = resolve(&backup_dir)?;
            disjoint(&root, &destination)?;
            let _lock = Lock::acquire(&root)?;
            ensure!(
                !journal_path(&root)?.exists(),
                "pending transaction: run config recover first"
            );
            ensure!(root.is_dir(), "data directory does not exist");
            let tree = collect(&root, true)?;
            backup_tree(&tree, &destination)?;
            serde_json::json!({"backup": destination, "files": tree.files.len(), "excluded_private_descriptors": tree.excluded})
        }
        Command::Migrate {
            data_dir,
            backup_dir,
            legacy_root,
        } => {
            let root = resolve(&data_dir)?;
            reject_program_root(&root)?;
            let destination = resolve(&backup_dir)?;
            disjoint(&root, &destination)?;
            let _lock = Lock::acquire(&root)?;
            ensure!(
                !journal_path(&root)?.exists(),
                "pending transaction: run config recover first"
            );
            let previous = collect(&root, true)?;
            let mut candidate = if let Some(legacy) = legacy_root {
                let legacy = resolve(&legacy)?;
                disjoint(&root, &legacy)?;
                disjoint(&destination, &legacy)?;
                ensure!(
                    previous.files.is_empty() && previous.directories.is_empty(),
                    "legacy import requires an empty destination data directory"
                );
                import_legacy(&legacy)?
            } else {
                collect(&root, true)?
            };
            validate(&candidate)?;
            normalize(&mut candidate)?;
            backup_tree(&previous, &destination)?;
            let original = transact(&root, &candidate)?;
            serde_json::json!({"data_dir": root, "schema_version": SCHEMA, "backup": destination, "retained_original": original, "excluded_private_descriptors": candidate.excluded})
        }
        Command::Restore {
            data_dir,
            backup,
            backup_dir,
        } => {
            let root = resolve(&data_dir)?;
            reject_program_root(&root)?;
            let source = resolve(&backup)?;
            let destination = resolve(&backup_dir)?;
            disjoint(&root, &source)?;
            disjoint(&root, &destination)?;
            disjoint(&source, &destination)?;
            let _lock = Lock::acquire(&root)?;
            // Restore intentionally accepts corrupt/future existing documents, but
            // never proceeds over an interrupted transaction.
            ensure!(
                !journal_path(&root)?.exists(),
                "pending transaction: run config recover first"
            );
            let mut candidate = read_backup(&source)?;
            reject_program_tree(&candidate)?;
            validate(&candidate)?;
            normalize(&mut candidate)?;
            let previous = collect(&root, true)?;
            backup_tree(&previous, &destination)?;
            let original = transact(&root, &candidate)?;
            serde_json::json!({"data_dir": root, "restored_from": source, "backup": destination, "retained_original": original, "schema_version": SCHEMA})
        }
        Command::Recover { data_dir } => {
            let root = resolve(&data_dir)?;
            let _lock = Lock::acquire(&root)?;
            recover(&root)?
        }
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

/// Read-only preflight. An absent marker is legacy data and remains usable.
pub fn check_ready(root: &Path) -> Result<()> {
    let root = resolve(root)?;
    ensure!(
        !journal_path(&root)?.exists(),
        "pending data transaction; close the app and run config recover --data-dir {}",
        root.display()
    );
    let marker = root.join(MARKER);
    let marker_present = match fs::symlink_metadata(&marker) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error).context("inspect data schema marker"),
    };
    if marker_present {
        let marker: Marker = serde_json::from_slice(&read_bounded(&marker, 4096)?)
            .context("invalid data schema marker; restore a known-good backup")?;
        ensure!(
            marker.schema_version != 1,
            "data schema 1 requires explicit migration; close all instances and run config migrate --data-dir {} --backup-dir <fresh-backup-directory>",
            root.display()
        );
        ensure!(
            marker.schema_version == SCHEMA,
            "unsupported data schema {}; expected {SCHEMA}; use a compatible application or explicitly restore a backup",
            marker.schema_version
        );
    }
    Ok(())
}

/// Keep the returned handle alive for the full GUI/MCP lifetime. Shared runtime
/// leases prevent offline maintenance from swapping a directory under live
/// writers. This creates only a sibling lock file, never preferences or roots.
/// Missing parent directories are an error rather than an unprotected lease.
pub fn runtime_lease(root: &Path) -> Result<Option<RuntimeLease>> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
    let root = resolve(root)?;
    let installation = crate::installation::data_root_lease(&root)?;
    let path = sibling(&root, "lock")?;
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        ensure_plain(&metadata, &path)?;
        ensure!(
            metadata.is_file(),
            "maintenance lock must be a regular file"
        );
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(&path)
        .context(
            "data directory is undergoing offline maintenance; close maintenance before launching",
        )?;
    check_ready(&root)?;
    Ok(Some(RuntimeLease {
        _config: file,
        _installation: installation,
    }))
}

pub struct RuntimeLease {
    _config: File,
    _installation: Option<File>,
}

fn resolve(path: &Path) -> Result<PathBuf> {
    ensure!(!path.as_os_str().is_empty(), "empty paths are not allowed");
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    for component in absolute.components() {
        ensure!(
            !matches!(component, Component::ParentDir),
            "parent traversal is not allowed"
        );
    }
    // Check every existing ancestor, including junctions and dangling symlinks.
    let mut current = PathBuf::new();
    for component in absolute.components() {
        current.push(component);
        // A Windows prefix such as `\\?\E:` is not independently queryable.
        // Inspect it only after RootDir completes the filesystem root.
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) => ensure_plain(&metadata, &current)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", current.display()));
            }
        }
    }
    if absolute.exists() {
        ensure!(
            absolute.is_dir(),
            "expected directory: {}",
            absolute.display()
        );
        return fs::canonicalize(absolute).context("resolve data directory");
    }
    let parent = absolute.parent().context("directory needs a parent")?;
    ensure!(
        parent.is_dir(),
        "parent directory must already exist: {}",
        parent.display()
    );
    Ok(fs::canonicalize(parent)?.join(absolute.file_name().context("directory needs a name")?))
}

fn ensure_plain(metadata: &fs::Metadata, path: &Path) -> Result<()> {
    use std::os::windows::fs::MetadataExt;
    ensure!(
        !metadata.file_type().is_symlink() && metadata.file_attributes() & 0x400 == 0,
        "symlinks/reparse points are not allowed: {}",
        path.display()
    );
    Ok(())
}

fn disjoint(left: &Path, right: &Path) -> Result<()> {
    let left = left.to_string_lossy().to_lowercase();
    let right = right.to_string_lossy().to_lowercase();
    ensure!(
        left != right
            && !left.starts_with(&(right.clone() + "\\"))
            && !right.starts_with(&(left.clone() + "\\")),
        "data, backup and legacy directories must not overlap"
    );
    Ok(())
}

fn has_program_signature<'a>(names: impl Iterator<Item = &'a str>) -> bool {
    let mut executable = false;
    let mut manifest = false;
    for name in names {
        if name.eq_ignore_ascii_case(".protoinputmultiplex-install.json") {
            return true;
        }
        executable |= name.eq_ignore_ascii_case("protoinputmultiplex.exe");
        manifest |= name.eq_ignore_ascii_case("bundle-manifest.json");
    }
    executable && manifest
}

fn reject_program_root(root: &Path) -> Result<()> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("inspect data root identity"),
    };
    let mut executable = false;
    let mut manifest = false;
    for entry in entries {
        let name = entry?.file_name();
        let name = name.to_string_lossy();
        executable |= name.eq_ignore_ascii_case("protoinputmultiplex.exe");
        manifest |= name.eq_ignore_ascii_case("bundle-manifest.json");
        ensure!(
            !(name.eq_ignore_ascii_case(".protoinputmultiplex-install.json")
                || executable && manifest),
            "selected data directory is a recognized program/installation root; choose its data subdirectory or use installation commands"
        );
    }
    Ok(())
}

fn reject_program_tree(tree: &Tree) -> Result<()> {
    ensure!(
        !has_program_signature(
            tree.directories
                .iter()
                .map(String::as_str)
                .chain(tree.files.iter().map(|file| file.0.as_str()))
        ),
        "backup payload is a recognized program/installation root, not a user-data directory"
    );
    Ok(())
}

fn valid_relative(name: &str) -> Result<PathBuf> {
    ensure!(
        !name.is_empty() && name.len() <= 2048,
        "invalid backup path length"
    );
    // Use portable forward slash paths and reject Windows aliases/ADS/devices.
    for part in name.split('/') {
        ensure!(
            !part.is_empty() && part != "." && part != ".." && !part.ends_with(['.', ' ']),
            "invalid backup path component"
        );
        ensure!(
            !part
                .chars()
                .any(|value| value.is_control() || "<>:\\|?*\"".contains(value)),
            "invalid character in backup path"
        );
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        ensure!(
            !matches!(
                stem.as_str(),
                "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
            ) && !(stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit())
                && !["COM¹", "COM²", "COM³", "LPT¹", "LPT²", "LPT³"].contains(&stem.as_str()),
            "reserved Windows path component"
        );
    }
    let path = PathBuf::from(name);
    ensure!(
        path.components()
            .all(|component| matches!(component, Component::Normal(_))),
        "backup paths must be relative"
    );
    Ok(path)
}

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    ensure_plain(&metadata, path)?;
    ensure!(
        metadata.is_file() && metadata.len() <= limit as u64,
        "file is not regular or exceeds size limit: {}",
        path.display()
    );
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= limit,
        "file grew beyond size limit: {}",
        path.display()
    );
    Ok(bytes)
}

fn private_descriptor(name: &str, bytes: &[u8]) -> bool {
    if name
        .rsplit('/')
        .next()
        .is_some_and(|name| name.starts_with("protoinputmultiplex-agent-"))
    {
        return true;
    }
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .is_some_and(|value| {
            value
                .get("pipe")
                .and_then(|value| value.as_str())
                .is_some_and(|pipe| pipe.starts_with(r"\\.\pipe\ProtoInputMultiPlex."))
                && value.get("token").is_some()
                && value.get("controller_pid").is_some()
        })
}

fn collect(root: &Path, exclude_private: bool) -> Result<Tree> {
    let mut tree = Tree::default();
    match fs::symlink_metadata(root) {
        Ok(metadata) => {
            ensure_plain(&metadata, root)?;
            ensure!(metadata.is_dir(), "data root must be a directory");
            collect_directory(root, "", &mut tree, exclude_private, 0)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspect data directory"),
    }
    tree.directories.sort();
    tree.files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut names = BTreeSet::new();
    for name in tree
        .directories
        .iter()
        .chain(tree.files.iter().map(|file| &file.0))
    {
        ensure!(
            names.insert(name.to_lowercase()),
            "case-insensitive duplicate data path"
        );
    }
    Ok(tree)
}

fn collect_directory(
    root: &Path,
    prefix: &str,
    tree: &mut Tree,
    exclude_private: bool,
    depth: usize,
) -> Result<()> {
    ensure!(depth <= 32, "data directory nesting exceeds 32 levels");
    ensure_plain(&fs::symlink_metadata(root)?, root)?;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let leaf = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("non-Unicode filenames cannot be backed up"))?;
        let name = if prefix.is_empty() {
            leaf
        } else {
            format!("{prefix}/{leaf}")
        };
        valid_relative(&name)?;
        let metadata = fs::symlink_metadata(entry.path())?;
        ensure_plain(&metadata, &entry.path())?;
        ensure!(
            tree.directories.len() + tree.files.len() + tree.excluded.len() < MAX_ENTRIES,
            "too many data entries"
        );
        if metadata.is_dir() {
            tree.directories.push(name.clone());
            collect_directory(&entry.path(), &name, tree, exclude_private, depth + 1)?;
        } else {
            let bytes = read_bounded(&entry.path(), MAX_FILE)?;
            if private_descriptor(&name, &bytes) {
                ensure!(
                    exclude_private,
                    "backup contains a private GUI connection descriptor"
                );
                tree.excluded.push(name);
                continue;
            }
            tree.bytes = tree
                .bytes
                .checked_add(bytes.len())
                .context("data size overflow")?;
            ensure!(
                tree.bytes <= MAX_TOTAL,
                "data exceeds the 256 MiB maintenance limit"
            );
            tree.files.push((name, bytes));
        }
    }
    Ok(())
}

fn schema(tree: &Tree) -> Result<u32> {
    tree.files
        .iter()
        .find(|file| file.0.eq_ignore_ascii_case(MARKER))
        .map_or(Ok(1), |file| {
            let marker: Marker =
                serde_json::from_slice(&file.1).context("invalid data schema marker")?;
            Ok(marker.schema_version)
        })
}

fn validate(tree: &Tree) -> Result<()> {
    let mut names = BTreeSet::new();
    for name in tree
        .directories
        .iter()
        .chain(tree.files.iter().map(|file| &file.0))
    {
        valid_relative(name)?;
        ensure!(
            names.insert(name.to_lowercase()),
            "case-insensitive duplicate data path"
        );
    }
    let version = schema(tree)?;
    ensure!(
        matches!(version, 1 | SCHEMA),
        "unsupported data schema {version}"
    );
    for (name, bytes) in &tree.files {
        let normalized = name.to_lowercase();
        if normalized == "app-preferences.json" {
            app_preferences::decode(bytes).with_context(|| format!("validate {name}"))?;
        } else if normalized.starts_with("profiles/") && normalized.ends_with(".json") {
            profile_store::decode(bytes).with_context(|| format!("validate {name}"))?;
        } else if normalized.starts_with("macros/") && normalized.ends_with(".json") {
            mux_core::automation::Clip::from_json(std::str::from_utf8(bytes)?)
                .with_context(|| format!("validate {name}"))?;
        }
    }
    Ok(())
}

fn normalize(tree: &mut Tree) -> Result<()> {
    for (name, bytes) in &mut tree.files {
        let normalized = name.to_lowercase();
        if normalized == "app-preferences.json" {
            *bytes = app_preferences::encode(&app_preferences::decode(bytes)?)?;
        } else if normalized.starts_with("profiles/") && normalized.ends_with(".json") {
            *bytes = profile_store::encode(&profile_store::decode(bytes)?)?;
        }
    }
    tree.files
        .retain(|file| !file.0.eq_ignore_ascii_case(MARKER));
    tree.files.push((
        MARKER.into(),
        serde_json::to_vec_pretty(&Marker {
            schema_version: SCHEMA,
        })?,
    ));
    tree.bytes = tree.files.iter().map(|file| file.1.len()).sum();
    ensure!(
        tree.bytes <= MAX_TOTAL,
        "normalized data exceeds size limit"
    );
    ensure!(
        tree.files.len() + tree.directories.len() <= MAX_ENTRIES,
        "normalized data exceeds entry limit"
    );
    Ok(())
}

fn import_legacy(root: &Path) -> Result<Tree> {
    ensure!(root.is_dir(), "legacy bundle directory does not exist");
    let mut tree = collect(&root.join("data"), true)?;
    for directory in ["profiles", "macros"] {
        let imported = collect(&root.join(directory), true)?;
        ensure!(
            !tree
                .directories
                .iter()
                .any(|name| name.eq_ignore_ascii_case(directory))
                && !tree
                    .files
                    .iter()
                    .any(|file| file.0.eq_ignore_ascii_case(directory)),
            "legacy data contains conflicting {directory}"
        );
        tree.directories.push(directory.into());
        tree.directories.extend(
            imported
                .directories
                .into_iter()
                .map(|name| format!("{directory}/{name}")),
        );
        tree.files.extend(
            imported
                .files
                .into_iter()
                .map(|(name, bytes)| (format!("{directory}/{name}"), bytes)),
        );
        tree.excluded.extend(
            imported
                .excluded
                .into_iter()
                .map(|name| format!("{directory}/{name}")),
        );
        tree.bytes += imported.bytes;
    }
    ensure!(
        tree.bytes <= MAX_TOTAL && tree.files.len() + tree.directories.len() <= MAX_ENTRIES,
        "legacy import exceeds maintenance bounds"
    );
    Ok(tree)
}

fn sha256(bytes: &[u8]) -> Result<String> {
    use windows_sys::Win32::Security::Cryptography::{
        BCRYPT_SHA256_ALGORITHM, BCryptCloseAlgorithmProvider, BCryptHash,
        BCryptOpenAlgorithmProvider,
    };
    let mut algorithm = std::ptr::null_mut();
    let opened = unsafe {
        BCryptOpenAlgorithmProvider(&mut algorithm, BCRYPT_SHA256_ALGORITHM, std::ptr::null(), 0)
    };
    ensure!(opened >= 0, "open SHA-256 provider failed: {opened:#x}");
    let mut digest = [0_u8; 32];
    let result = unsafe {
        BCryptHash(
            algorithm,
            std::ptr::null(),
            0,
            bytes.as_ptr(),
            bytes.len() as u32,
            digest.as_mut_ptr(),
            digest.len() as u32,
        )
    };
    unsafe {
        BCryptCloseAlgorithmProvider(algorithm, 0);
    }
    ensure!(result >= 0, "SHA-256 failed: {result:#x}");
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn write_tree(tree: &Tree, root: &Path) -> Result<()> {
    fs::create_dir(root).with_context(|| format!("create fresh directory {}", root.display()))?;
    for name in &tree.directories {
        fs::create_dir_all(root.join(valid_relative(name)?))?;
    }
    for (name, bytes) in &tree.files {
        let target = root.join(valid_relative(name)?);
        fs::create_dir_all(target.parent().context("file needs parent")?)?;
        write_new(&target, bytes)?;
    }
    Ok(())
}

fn backup_tree(tree: &Tree, destination: &Path) -> Result<()> {
    fs::create_dir(destination).context("backup destination must be fresh")?;
    write_tree(tree, &destination.join("payload"))?;
    let files = tree
        .files
        .iter()
        .map(|(path, bytes)| {
            Ok(BackupFile {
                path: path.clone(),
                bytes: bytes.len(),
                sha256: sha256(bytes)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let manifest = Manifest {
        backup_version: 1,
        directories: tree.directories.clone(),
        files,
        excluded_private_descriptors: tree.excluded.clone(),
    };
    // Manifest is the completion record; incomplete backups cannot be restored.
    write_new(
        &destination.join("backup-manifest.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(())
}

fn read_backup(source: &Path) -> Result<Tree> {
    let manifest: Manifest = serde_json::from_slice(&read_bounded(
        &source.join("backup-manifest.json"),
        4 * 1024 * 1024,
    )?)?;
    ensure!(manifest.backup_version == 1, "unsupported backup version");
    ensure!(
        manifest.files.len() + manifest.directories.len() <= MAX_ENTRIES,
        "too many backup entries"
    );
    for file in &manifest.files {
        valid_relative(&file.path)?;
    }
    for name in &manifest.directories {
        valid_relative(name)?;
    }
    ensure!(
        source.join("payload").is_dir(),
        "backup payload directory is missing"
    );
    let tree = collect(&source.join("payload"), false)?;
    let mut expected_dirs = manifest.directories;
    expected_dirs.sort();
    ensure!(
        tree.directories == expected_dirs && tree.files.len() == manifest.files.len(),
        "backup inventory mismatch"
    );
    let mut names = BTreeSet::new();
    for expected in &manifest.files {
        ensure!(
            names.insert(expected.path.to_lowercase()),
            "duplicate backup entry"
        );
        let actual = tree
            .files
            .iter()
            .find(|file| file.0 == expected.path)
            .context("missing backup file")?;
        ensure!(
            actual.1.len() == expected.bytes && sha256(&actual.1)? == expected.sha256,
            "backup checksum mismatch: {}",
            expected.path
        );
    }
    Ok(tree)
}

fn sibling(root: &Path, suffix: &str) -> Result<PathBuf> {
    let name = root
        .file_name()
        .context("data root must have a directory name")?
        .to_str()
        .context("data directory name must be Unicode")?;
    Ok(root
        .parent()
        .context("data root needs a parent")?
        .join(format!(".{name}.config-{suffix}")))
}

fn journal_path(root: &Path) -> Result<PathBuf> {
    sibling(root, "transaction.json")
}

struct Lock {
    path: PathBuf,
    file: Option<File>,
    _installation: Option<File>,
}
impl Lock {
    fn acquire(root: &Path) -> Result<Self> {
        // Every caller follows the same order: installation lease, then data
        // lease. External maintenance clients therefore also block upgrades.
        let installation = crate::installation::data_root_lease(root)?;
        let path = sibling(root, "lock")?;
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            ensure_plain(&metadata, &path)?;
            ensure!(
                metadata.is_file(),
                "maintenance lock must be a regular file"
            );
        }
        // OS sharing prevents concurrent maintenance; a stale filename after a
        // process crash is harmless and may be reopened by the next invocation.
        use std::os::windows::fs::OpenOptionsExt;
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(&path)
            .context("another maintenance process is using this root")?;
        Ok(Self {
            path,
            file: Some(file),
            _installation: installation,
        })
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = fs::remove_file(&self.path);
    }
}

fn transact(root: &Path, candidate: &Tree) -> Result<Option<PathBuf>> {
    transact_inner(root, candidate, || Ok(()))
}

fn transact_inner(
    root: &Path,
    candidate: &Tree,
    after_original: impl FnOnce() -> Result<()>,
) -> Result<Option<PathBuf>> {
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    let nonce = format!("{}-{sequence}", std::process::id());
    let stage = sibling(root, &format!("stage-{nonce}"))?;
    let original = sibling(root, &format!("original-{nonce}"))?;
    ensure!(
        !stage.exists() && !original.exists(),
        "transaction scratch paths already exist"
    );
    write_tree(candidate, &stage)?;
    let journal = Journal {
        version: 1,
        original_existed: root.exists(),
        stage_name: stage.file_name().unwrap().to_str().unwrap().to_owned(),
        original_name: original.file_name().unwrap().to_str().unwrap().to_owned(),
    };
    let journal_file = journal_path(root)?;
    write_new(&journal_file, &serde_json::to_vec_pretty(&journal)?)?;
    let result = (|| -> Result<()> {
        if journal.original_existed {
            fs::rename(root, &original).context("retain original data directory")?;
        }
        after_original()?;
        fs::rename(&stage, root).context("publish migrated/restored data directory")?;
        fs::remove_file(&journal_file).context("complete data transaction")?;
        Ok(())
    })();
    if let Err(error) = result {
        let rollback = recover(root);
        bail!(
            "data transaction failed: {error:#}; rollback: {}; recovery backup and scratch directories retained",
            match rollback {
                Ok(_) => "completed".into(),
                Err(error) => format!("{error:#}; run config recover"),
            }
        );
    }
    Ok(journal.original_existed.then_some(original))
}

fn recover(root: &Path) -> Result<serde_json::Value> {
    let journal_file = journal_path(root)?;
    if !journal_file.exists() {
        return Ok(
            serde_json::json!({"recovered": false, "data_dir": root, "reason": "no pending transaction"}),
        );
    }
    let journal: Journal = serde_json::from_slice(&read_bounded(&journal_file, 4096)?)?;
    ensure!(journal.version == 1, "unsupported recovery journal version");
    let parent = root.parent().context("root needs parent")?;
    let expected_prefix = sibling(root, "")?
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    for (name, kind) in [
        (&journal.stage_name, "stage-"),
        (&journal.original_name, "original-"),
    ] {
        let relative = valid_relative(name)?;
        ensure!(
            relative.components().count() == 1
                && name.starts_with(&format!("{expected_prefix}{kind}")),
            "invalid recovery journal path"
        );
    }
    let stage = resolve(&parent.join(&journal.stage_name))?;
    let original = resolve(&parent.join(&journal.original_name))?;
    if original.exists() {
        if root.exists() {
            ensure!(
                !stage.exists(),
                "both stage and published data exist; preserve them and inspect before recovery"
            );
            fs::rename(root, &stage).context("retain failed candidate")?;
        }
        fs::rename(&original, root).context("restore original data directory")?;
    } else if !journal.original_existed && root.exists() {
        ensure!(
            !stage.exists(),
            "both stage and published data exist; manual inspection required"
        );
        fs::rename(root, &stage).context("retain uncommitted new data")?;
    } else {
        ensure!(
            root.exists() || !journal.original_existed,
            "original data directory is missing; retain journal for manual recovery"
        );
    }
    fs::remove_file(journal_file)?;
    Ok(serde_json::json!({"recovered": true, "data_dir": root, "retained_candidate": stage}))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "protoinputmultiplex-config-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
        fn data(&self) -> PathBuf {
            let root = self.path("data with spaces");
            fs::create_dir(&root).unwrap();
            root
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn valid_data(root: &Path) {
        fs::create_dir(root.join("profiles")).unwrap();
        fs::create_dir(root.join("macros")).unwrap();
        fs::create_dir(root.join("unknown-empty-folder")).unwrap();
        fs::write(
            root.join("app-preferences.json"),
            br#"{"version":1,"launch_at_login":true,"zoom_percent":125}"#,
        )
        .unwrap();
        fs::write(root.join("profiles/legacy.json"), br#"{"version":1,"seats":[{"name":"A","mouse":"interface-path","keyboard":null,"target_pid":42}]}"#).unwrap();
        fs::write(root.join("unknown.bin"), [0, 1, 255, 4]).unwrap();
        fs::write(root.join("macros/sample.json"), br#"{"version":1,"events":[{"at_micros":0,"action":{"type":"move_to","user":0,"x":1,"y":2}}]}"#).unwrap();
    }

    #[test]
    fn migration_normalizes_ids_preserves_login_opt_in_and_unknown_data() {
        let fixture = Fixture::new();
        let root = fixture.data();
        valid_data(&root);
        let before = fs::read(root.join("profiles/legacy.json")).unwrap();
        let backup = fixture.path("before migration");
        run(Command::Migrate {
            data_dir: root.clone(),
            backup_dir: backup.clone(),
            legacy_root: None,
        })
        .unwrap();
        check_ready(&root).unwrap();
        let tree = collect(&root, true).unwrap();
        assert_eq!(schema(&tree).unwrap(), SCHEMA);
        assert!(
            app_preferences::decode(&fs::read(root.join("app-preferences.json")).unwrap())
                .unwrap()
                .launch_at_login
        );
        let profile = profile_store::read(&root.join("profiles/legacy.json")).unwrap();
        assert!(profile.seats[0].id.starts_with("legacy-"));
        assert_eq!(fs::read(root.join("unknown.bin")).unwrap(), [0, 1, 255, 4]);
        assert!(root.join("unknown-empty-folder").is_dir());
        assert_eq!(
            read_backup(&backup)
                .unwrap()
                .files
                .iter()
                .find(|file| file.0 == "profiles/legacy.json")
                .unwrap()
                .1,
            before
        );
    }

    #[test]
    fn restore_retains_corrupt_original_bytes_and_rejects_bad_backup_before_writes() {
        let fixture = Fixture::new();
        let root = fixture.data();
        valid_data(&root);
        let backup = fixture.path("known good");
        run(Command::Backup {
            data_dir: root.clone(),
            backup_dir: backup.clone(),
        })
        .unwrap();
        fs::write(root.join("app-preferences.json"), b"CORRUPT").unwrap();
        let recovery = fixture.path("corrupt recovery");
        run(Command::Restore {
            data_dir: root.clone(),
            backup: backup.clone(),
            backup_dir: recovery.clone(),
        })
        .unwrap();
        assert_eq!(
            read_backup(&recovery)
                .unwrap()
                .files
                .iter()
                .find(|file| file.0 == "app-preferences.json")
                .unwrap()
                .1,
            b"CORRUPT"
        );
        app_preferences::decode(&fs::read(root.join("app-preferences.json")).unwrap()).unwrap();
        fs::write(backup.join("payload/unknown.bin"), b"tampered").unwrap();
        let rejected_recovery = fixture.path("must not be created");
        assert!(
            run(Command::Restore {
                data_dir: root.clone(),
                backup,
                backup_dir: rejected_recovery.clone()
            })
            .is_err()
        );
        assert!(!rejected_recovery.exists());
        app_preferences::decode(&fs::read(root.join("app-preferences.json")).unwrap()).unwrap();
    }

    #[test]
    fn private_descriptors_are_excluded_by_name_and_content() {
        let fixture = Fixture::new();
        let root = fixture.data();
        fs::write(
            root.join("renamed-secret.json"),
            br#"{"pipe":"\\\\.\\pipe\\ProtoInputMultiPlex.test","token":"secret","controller_pid":1}"#,
        )
        .unwrap();
        fs::write(
            root.join("protoinputmultiplex-agent-5.json.tmp"),
            b"partially written secret",
        )
        .unwrap();
        fs::write(root.join("normal.txt"), b"ordinary data").unwrap();
        let tree = collect(&root, true).unwrap();
        assert_eq!(tree.files.len(), 1);
        assert_eq!(tree.excluded.len(), 2);
        let backup = fixture.path("backup");
        backup_tree(&tree, &backup).unwrap();
        assert_eq!(read_backup(&backup).unwrap().files.len(), 1);
    }

    #[test]
    fn legacy_import_selects_only_data_profiles_and_macros() {
        let fixture = Fixture::new();
        let legacy = fixture.path("old portable");
        fs::create_dir(&legacy).unwrap();
        fs::create_dir(legacy.join("data")).unwrap();
        fs::create_dir(legacy.join("profiles")).unwrap();
        fs::write(legacy.join("protoinputmultiplex.exe"), b"not user data").unwrap();
        fs::write(legacy.join("bundle-manifest.json"), b"not user data").unwrap();
        fs::write(
            legacy.join("data/app-preferences.json"),
            br#"{"version":1,"launch_at_login":false}"#,
        )
        .unwrap();
        fs::write(
            legacy.join("profiles/empty.json"),
            br#"{"version":1,"seats":[]}"#,
        )
        .unwrap();
        let root = fixture.path("new data");
        run(Command::Migrate {
            data_dir: root.clone(),
            backup_dir: fixture.path("before import"),
            legacy_root: Some(legacy.clone()),
        })
        .unwrap();
        assert!(root.join("profiles/empty.json").exists());
        assert!(!root.join("protoinputmultiplex.exe").exists());
        assert!(!root.join("bundle-manifest.json").exists());
        assert!(legacy.join("protoinputmultiplex.exe").exists());
    }

    #[test]
    fn injected_publish_failure_rolls_back_and_preserves_staged_candidate() {
        let fixture = Fixture::new();
        let root = fixture.data();
        valid_data(&root);
        let mut tree = collect(&root, true).unwrap();
        normalize(&mut tree).unwrap();
        let result = transact_inner(&root, &tree, || {
            bail!("injected failure after original rename")
        });
        assert!(result.is_err());
        assert!(root.join("profiles/legacy.json").exists());
        assert!(!root.join(MARKER).exists());
        assert!(!journal_path(&root).unwrap().exists());
        assert!(
            fs::read_dir(&fixture.0)
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .contains("config-stage-"))
        );
    }

    #[test]
    fn interrupted_published_transaction_recovers_original_and_is_idempotent() {
        let fixture = Fixture::new();
        let root = fixture.data();
        valid_data(&root);
        let original = sibling(&root, "original-test").unwrap();
        let stage = sibling(&root, "stage-test").unwrap();
        let mut candidate = collect(&root, true).unwrap();
        normalize(&mut candidate).unwrap();
        fs::rename(&root, &original).unwrap();
        write_tree(&candidate, &root).unwrap();
        let journal = Journal {
            version: 1,
            original_existed: true,
            stage_name: stage.file_name().unwrap().to_str().unwrap().into(),
            original_name: original.file_name().unwrap().to_str().unwrap().into(),
        };
        write_new(
            &journal_path(&root).unwrap(),
            &serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
        assert!(check_ready(&root).is_err());
        recover(&root).unwrap();
        assert!(!root.join(MARKER).exists());
        assert!(stage.join(MARKER).exists());
        recover(&root).unwrap();
    }

    #[test]
    fn failed_validation_and_future_schema_do_not_mutate_source() {
        let fixture = Fixture::new();
        let root = fixture.data();
        valid_data(&root);
        fs::write(root.join(MARKER), b"{\"schema_version\":99}").unwrap();
        assert!(check_ready(&root).is_err());
        let backup = fixture.path("not created");
        assert!(
            run(Command::Migrate {
                data_dir: root.clone(),
                backup_dir: backup.clone(),
                legacy_root: None
            })
            .is_err()
        );
        assert!(!backup.exists());
        // Backups remain possible even if the marker itself is corrupt/future.
        run(Command::Backup {
            data_dir: root.clone(),
            backup_dir: backup,
        })
        .unwrap();
        assert_eq!(
            fs::read(root.join(MARKER)).unwrap(),
            b"{\"schema_version\":99}"
        );
    }

    #[test]
    fn schema_one_marker_requires_explicit_migration_and_can_be_migrated() {
        let fixture = Fixture::new();
        let root = fixture.data();
        valid_data(&root);
        fs::write(root.join(MARKER), b"{\"schema_version\":1}").unwrap();
        assert!(format!("{:#}", check_ready(&root).unwrap_err()).contains("config migrate"));
        let backup = fixture.path("schema one backup");
        run(Command::Migrate {
            data_dir: root.clone(),
            backup_dir: backup.clone(),
            legacy_root: None,
        })
        .unwrap();
        check_ready(&root).unwrap();
        assert_eq!(schema(&read_backup(&backup).unwrap()).unwrap(), 1);
        assert_eq!(schema(&collect(&root, true).unwrap()).unwrap(), SCHEMA);
    }

    #[test]
    fn unsafe_paths_overlap_and_stale_outputs_are_rejected() {
        for name in [
            "../escape",
            "C:/escape",
            "/root",
            "a\\..\\b",
            "a:b",
            "NUL.txt",
            "a/COM1",
            "a./b",
            "a//b",
        ] {
            assert!(valid_relative(name).is_err(), "{name}");
        }
        let fixture = Fixture::new();
        let root = resolve(&fixture.data()).unwrap();
        assert!(disjoint(&root, &root.join("nested")).is_err());
        assert!(disjoint(&root, &root).is_err());
        // Windows PathBuf::join normalizes `..` when its base is verbatim.
        // Construct the literal supplied path so resolve sees the traversal.
        let traversal = PathBuf::from(format!("{}\\..\\escape", root.display()));
        assert!(resolve(&traversal).is_err());
        assert!(backup_tree(&Tree::default(), &root).is_err());
    }

    #[test]
    fn resolve_accepts_canonical_verbatim_windows_paths() {
        let fixture = Fixture::new();
        let root = fixture.data();
        let canonical = fs::canonicalize(&root).unwrap();
        assert_eq!(resolve(&canonical).unwrap(), canonical);
        let new_root = fs::canonicalize(&fixture.0).unwrap().join("absent data");
        assert_eq!(resolve(&new_root).unwrap(), new_root);
    }

    #[test]
    fn recognized_program_roots_are_rejected_before_maintenance_writes() {
        for signature in [
            ".ProtoInputMultiPlex-Install.JSON",
            "ProtoInputMultiPlex.EXE",
        ] {
            let fixture = Fixture::new();
            let root = fixture.data();
            valid_data(&root);
            fs::write(root.join(signature), b"program identity").unwrap();
            fs::write(root.join("Bundle-Manifest.JSON"), b"manifest bytes").unwrap();
            let before = collect(&root, true).unwrap().files;
            let backup = fixture.path("must not create backup");
            let source = fixture.path("valid source backup");
            backup_tree(&Tree::default(), &source).unwrap();
            assert!(
                run(Command::Inspect {
                    data_dir: root.clone()
                })
                .is_err()
            );
            assert!(
                run(Command::Backup {
                    data_dir: root.clone(),
                    backup_dir: backup.clone()
                })
                .is_err()
            );
            assert!(
                run(Command::Migrate {
                    data_dir: root.clone(),
                    backup_dir: backup.clone(),
                    legacy_root: None
                })
                .is_err()
            );
            assert!(
                run(Command::Restore {
                    data_dir: root.clone(),
                    backup: source,
                    backup_dir: backup.clone()
                })
                .is_err()
            );
            assert!(!backup.exists());
            assert!(!sibling(&root, "lock").unwrap().exists());
            assert_eq!(collect(&root, true).unwrap().files, before);
        }
    }

    #[test]
    fn program_signature_in_backup_is_rejected_before_recovery_backup() {
        for signature in [
            ".PROTOINPUTMULTIPLEX-INSTALL.JSON",
            "PROTOINPUTMULTIPLEX.EXE",
        ] {
            let fixture = Fixture::new();
            let root = fixture.data();
            valid_data(&root);
            let before = collect(&root, true).unwrap().files;
            let files = vec![
                (signature.into(), b"program identity".to_vec()),
                ("BUNDLE-MANIFEST.JSON".into(), b"manifest bytes".to_vec()),
            ];
            let candidate = Tree {
                files,
                ..Tree::default()
            };
            let source = fixture.path("program backup");
            backup_tree(&candidate, &source).unwrap();
            let recovery = fixture.path("must not create recovery");
            assert!(
                run(Command::Restore {
                    data_dir: root.clone(),
                    backup: source,
                    backup_dir: recovery.clone()
                })
                .is_err()
            );
            assert!(!recovery.exists());
            assert_eq!(collect(&root, true).unwrap().files, before);
        }
    }

    #[test]
    fn standalone_executable_without_program_signature_is_preserved_as_user_data() {
        let fixture = Fixture::new();
        let root = fixture.data();
        valid_data(&root);
        fs::write(
            root.join("protoinputmultiplex.exe"),
            b"standalone user artifact",
        )
        .unwrap();
        run(Command::Migrate {
            data_dir: root.clone(),
            backup_dir: fixture.path("previous data"),
            legacy_root: None,
        })
        .unwrap();
        assert_eq!(
            fs::read(root.join("protoinputmultiplex.exe")).unwrap(),
            b"standalone user artifact"
        );
        check_ready(&root).unwrap();
    }

    #[test]
    fn backup_rejects_manifest_traversal_and_unlisted_files() {
        let fixture = Fixture::new();
        let backup = fixture.path("backup");
        backup_tree(&Tree::default(), &backup).unwrap();
        fs::write(backup.join("payload/extra"), b"unlisted").unwrap();
        assert!(read_backup(&backup).is_err());
        let manifest = Manifest {
            backup_version: 1,
            directories: vec![],
            files: vec![BackupFile {
                path: "../escape".into(),
                bytes: 0,
                sha256: "bad".into(),
            }],
            excluded_private_descriptors: vec![],
        };
        fs::write(
            backup.join("backup-manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        assert!(read_backup(&backup).is_err());
    }

    #[test]
    fn lock_uses_os_handle_and_stale_filename_is_reusable() {
        let fixture = Fixture::new();
        let root = fixture.data();
        let lock = Lock::acquire(&root).unwrap();
        assert!(Lock::acquire(&root).is_err());
        drop(lock);
        fs::write(sibling(&root, "lock").unwrap(), b"stale").unwrap();
        Lock::acquire(&root).unwrap();
    }

    #[test]
    fn runtime_leases_share_with_each_other_and_exclude_maintenance() {
        let fixture = Fixture::new();
        let root = fixture.path("new data");
        let first = runtime_lease(&root).unwrap();
        let second = runtime_lease(&root).unwrap();
        assert!(
            !root.exists(),
            "readonly startup must not create user-data root"
        );
        assert!(Lock::acquire(&root).is_err());
        drop(first);
        assert!(Lock::acquire(&root).is_err());
        drop(second);
        let maintenance = Lock::acquire(&root).unwrap();
        assert!(runtime_lease(&root).is_err());
        drop(maintenance);
        runtime_lease(&root).unwrap();
        assert!(runtime_lease(&fixture.path("missing parent/data")).is_err());
    }

    #[test]
    fn bounded_read_and_sha256_are_checked() {
        let fixture = Fixture::new();
        let path = fixture.path("bounded");
        fs::write(&path, [1, 2, 3, 4]).unwrap();
        assert!(read_bounded(&path, 3).is_err());
        assert_eq!(
            sha256(b"abc").unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn symlink_is_rejected_when_windows_allows_creation() {
        let fixture = Fixture::new();
        let root = fixture.data();
        let target = fixture.path("target");
        fs::create_dir(&target).unwrap();
        if std::os::windows::fs::symlink_dir(&target, root.join("link")).is_ok() {
            assert!(collect(&root, true).is_err());
            assert!(resolve(&root.join("link")).is_err());
        }
    }
}
