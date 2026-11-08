//! Payload-only installation transactions. Configuration and startup registration
//! are deliberately outside this module's ownership boundary.
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use clap::Subcommand;
use serde::{Deserialize, Serialize};

// Installations and retained releases use the canonical product identity.
const INSTALLATION_IDENTITY: &str = "ProtoInputMultiPlex";
const MARKER: &str = ".protoinputmultiplex-install.json";
const JOURNAL: &str = ".protoinputmultiplex-install-journal.json";
const LEASE: &str = ".protoinputmultiplex-runtime.lock";
const STAGE: &str = ".protoinputmultiplex-staging";
const MANIFEST: &str = "bundle-manifest.json";
const USER_DIRS: [&str; 3] = ["data", "profiles", "macros"];

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Install a verified portable bundle, retaining replaced payloads.
    Install {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        root: PathBuf,
    },
    /// Report installation ownership and check the active payload's integrity.
    Inspect {
        #[arg(long)]
        root: PathBuf,
    },
    /// Remove owned program files; preserve configuration and unknown files.
    Uninstall {
        #[arg(long)]
        root: PathBuf,
    },
    /// Roll back an interrupted installation transaction.
    Recover {
        #[arg(long)]
        root: PathBuf,
    },
    /// Explicitly restore the most recently retained successful payload.
    Rollback {
        #[arg(long)]
        root: PathBuf,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Entry {
    path: String,
    sha256: String,
    bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Payload {
    version: String,
    files: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
struct Bundle {
    schema_version: u32,
    application: String,
    version: String,
    platform: String,
    files: Vec<Entry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Retained {
    directory: String,
    payload: Payload,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema_version: u32,
    application: String,
    current: Option<Payload>,
    history: Vec<Retained>,
    uninstalling: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            schema_version: 1,
            application: INSTALLATION_IDENTITY.into(),
            current: None,
            history: Vec::new(),
            uninstalling: false,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema_version: u32,
    before: State,
    incoming: Payload,
    archived: Option<String>,
}

pub fn run(command: Command) -> Result<()> {
    let output = match command {
        Command::Install { bundle, root } => {
            install(&bundle, &root, false)?;
            serde_json::json!({"status":"installed", "root":root, "data_preserved":true})
        }
        Command::Inspect { root } => {
            let root = checked_root(&root, false)?;
            let _lease = acquire_lease(&root, false)?;
            let state = load_state(&root)?;
            if let Some(payload) = &state.current {
                verify_payload(&root.join("app"), payload, false)?;
            }
            serde_json::json!({"root":root, "installation":state,
                "interrupted_transaction":root.join(JOURNAL).exists(),
                "data_preserved":true})
        }
        Command::Uninstall { root } => {
            uninstall(&root)?;
            serde_json::json!({"status":"uninstalled", "root":root,
                "data_preserved":true, "root_preserved":true})
        }
        Command::Recover { root } => {
            let root = checked_root(&root, false)?;
            let _lease = acquire_lease(&root, true)?;
            let recovered = recover(&root)?;
            serde_json::json!({"status":if recovered {"recovered"} else {"no_transaction"}, "root":root})
        }
        Command::Rollback { root } => {
            let root = checked_root(&root, false)?;
            let source = {
                let _lease = acquire_lease(&root, false)?;
                ensure!(
                    !root.join(JOURNAL).exists(),
                    "Run installation recover first"
                );
                let state = load_state(&root)?;
                ensure!(
                    !state.uninstalling,
                    "Finish the interrupted uninstall first"
                );
                let previous = state
                    .history
                    .last()
                    .context("No retained release is available")?;
                root.join(&previous.directory)
            };
            install(&source, &root, true)?;
            serde_json::json!({"status":"rolled_back", "root":root, "data_preserved":true})
        }
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Hold this handle for the lifetime of an installed GUI/MCP/helper command.
/// File locks are released by Windows when a process exits, including crashes.
pub fn runtime_lease() -> Result<Option<File>> {
    let executable = std::env::current_exe()?.canonicalize()?;
    let Some(app) = executable.parent() else {
        return Ok(None);
    };
    if !app
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("app"))
    {
        return Ok(None);
    }
    data_root_lease(app)
}

/// Hold this lease while accessing data beneath an installed `app` directory,
/// even when the caller executable comes from another installation or bundle.
/// Missing data directories are resolved through their existing ancestors.
pub fn data_root_lease(data_root: &Path) -> Result<Option<File>> {
    ensure!(
        !data_root.as_os_str().is_empty(),
        "Data root must not be empty"
    );
    let absolute = std::path::absolute(data_root)?;
    let data_root = prospective_root(&absolute)?;
    for app in data_root.ancestors() {
        if !app
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("app"))
        {
            continue;
        }
        let Some(root) = app.parent() else { continue };
        if root.join(MARKER).exists() {
            return active_installation_lease(root).map(Some);
        }
    }
    Ok(None)
}

fn active_installation_lease(root: &Path) -> Result<File> {
    if !root.join(MARKER).exists() {
        anyhow::bail!("Installation ownership marker is missing");
    }
    no_reparse_ancestors(root)?;
    let lease = acquire_lease(root, false)?;
    let state = load_state(root)?;
    ensure!(
        !state.uninstalling && !root.join(JOURNAL).exists(),
        "Installation needs recovery before launch; run an external installer engine"
    );
    ensure!(
        state.current.is_some(),
        "This installation has been uninstalled"
    );
    Ok(lease)
}

fn acquire_lease(root: &Path, exclusive: bool) -> Result<File> {
    let path = root.join(LEASE);
    no_reparse_if_exists(&path)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    if exclusive {
        file.try_lock().context("Close installed ProtoInputMultiPlex GUI, MCP and helper processes before changing this installation")?;
    } else {
        file.try_lock_shared()
            .context("The installation is currently being changed")?;
    }
    Ok(file)
}

fn install(bundle: &Path, requested_root: &Path, internal: bool) -> Result<()> {
    no_reparse_ancestors(bundle)?;
    let bundle = bundle
        .canonicalize()
        .context("Bundle directory is unavailable")?;
    let payload = read_bundle(&bundle)?;
    verify_payload(&bundle, &payload, false)?;
    let prospective = prospective_root(requested_root)?;
    ensure!(
        !bundle.starts_with(&prospective) || internal,
        "Bundle and installation roots must not overlap"
    );
    ensure!(
        !prospective.starts_with(&bundle),
        "Bundle and installation roots must not overlap"
    );
    let executable = std::env::current_exe()?.canonicalize()?;
    ensure!(
        !executable.starts_with(&prospective),
        "Run the installer engine from outside the installation root"
    );
    let root = checked_root(requested_root, true)?;
    let _lease = acquire_lease(&root, true)?;
    if !root.join(MARKER).exists() && !root.join(JOURNAL).exists() {
        atomic_json(&root.join(MARKER), &State::default())?;
    }
    ensure!(
        !root.join(JOURNAL).exists(),
        "Interrupted transaction found; run installation recover first"
    );
    let before = if root.join(MARKER).exists() {
        load_state(&root)?
    } else {
        State::default()
    };
    ensure!(
        !before.uninstalling,
        "Finish the interrupted uninstall before reinstalling"
    );
    if let Some(current) = &before.current {
        verify_payload(&root.join("app"), current, false)?;
        preflight_writable(&root.join("app"), current, false)?;
    } else {
        ensure!(!root.join("app").exists(), "Unowned app directory exists");
    }
    ensure!(
        !root.join(STAGE).exists(),
        "Unowned staging directory exists; preserve it and choose a fresh root"
    );
    let archived = before
        .current
        .as_ref()
        .map(|_| format!("releases/{}", unique_id()));
    let journal = Journal {
        schema_version: 1,
        before: before.clone(),
        incoming: payload.clone(),
        archived: archived.clone(),
    };
    // Journal is durable before the first payload mutation. Any later failure can
    // restore the old app and move user data back without guessing ownership.
    atomic_json(&root.join(JOURNAL), &journal)?;
    let result = (|| -> Result<()> {
        let stage = root.join(STAGE);
        fs::create_dir(&stage)?;
        for entry in &payload.files {
            let destination = stage.join(&entry.path);
            fs::create_dir_all(destination.parent().context("Missing payload parent")?)?;
            let mut source = File::open(bundle.join(&entry.path))?;
            let mut target = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(destination)?;
            std::io::copy(&mut source, &mut target)?;
            target.sync_all()?;
        }
        verify_payload(&stage, &payload, false)?;
        let mut after = before.clone();
        // Normalize the marker when committing a verified release.
        after.application = INSTALLATION_IDENTITY.into();
        if let (Some(directory), Some(current)) = (&archived, &before.current) {
            let archive = root.join(directory);
            fs::create_dir_all(archive.parent().context("Missing release parent")?)?;
            ensure!(
                !archive.exists(),
                "Retained release directory already exists"
            );
            fs::rename(root.join("app"), &archive)
                .context("Could not retire current app; close all installed processes")?;
            move_user_data(&archive, &stage)?;
            after.history.push(Retained {
                directory: directory.clone(),
                payload: current.clone(),
            });
        }
        fs::rename(&stage, root.join("app"))?;
        after.current = Some(payload);
        atomic_json(&root.join(MARKER), &after)?;
        fs::remove_file(root.join(JOURNAL))?;
        Ok(())
    })();
    if let Err(error) = result {
        return match recover(&root) {
            Ok(_) => Err(error.context("Installation failed; the previous payload and user data were restored")),
            Err(recovery) => Err(error.context(format!("Installation failed; recovery also failed: {recovery:#}. Preserve this root and run installation recover after closing processes"))),
        };
    }
    Ok(())
}

fn recover(root: &Path) -> Result<bool> {
    if !root.join(JOURNAL).exists() {
        return Ok(false);
    }
    let journal: Journal = read_json(&root.join(JOURNAL))?;
    ensure!(
        journal.schema_version == 1,
        "Unsupported installation journal schema"
    );
    validate_state(&journal.before)?;
    validate_payload(&journal.incoming)?;
    if let Some(archive) = &journal.archived {
        validate_archive(archive)?;
    }
    let app = root.join("app");
    let stage = root.join(STAGE);
    let archive = journal.archived.as_ref().map(|path| root.join(path));
    // A pre-existing app is old exactly when the archive rename did not happen.
    let old_app_still_present = journal.before.current.is_some()
        && archive.as_ref().is_some_and(|path| !path.exists())
        && app.exists();
    if old_app_still_present {
        verify_payload(
            &app,
            journal
                .before
                .current
                .as_ref()
                .context("Missing old payload")?,
            false,
        )?;
    }
    if !old_app_still_present && app.exists() {
        verify_payload(&app, &journal.incoming, true)?;
        preflight_writable(&app, &journal.incoming, true)?;
        if let Some(archive) = &archive {
            move_user_data(&app, archive)?;
        }
        remove_payload(&app, &journal.incoming, true)?;
    }
    if stage.exists() {
        // Staging may be only partly copied. Validate every present file before
        // deleting it, while permitting files not reached before the failure.
        let expected = journal
            .incoming
            .files
            .iter()
            .map(|entry| entry.path.to_ascii_lowercase())
            .collect();
        no_reparse_ancestors(&stage)?;
        check_tree(&stage, &stage, &expected)?;
        if let Some(archive) = &archive
            && archive.exists()
        {
            move_user_data(&stage, archive)?;
        }
        // Only this journal-created staging tree permits incomplete hashes:
        // interruption can leave a known destination file partly written.
        remove_known_files(&stage, &journal.incoming)?;
    }
    if let Some(archive) = &archive
        && archive.exists()
    {
        let previous = journal
            .before
            .current
            .as_ref()
            .context("Journal archive has no previous payload")?;
        verify_payload(archive, previous, false)?;
        ensure!(
            !app.exists(),
            "Recovery refuses to replace an existing app directory"
        );
        fs::rename(archive, &app)?;
    }
    atomic_json(&root.join(MARKER), &journal.before)?;
    fs::remove_file(root.join(JOURNAL))?;
    Ok(true)
}

fn uninstall(requested_root: &Path) -> Result<()> {
    let root = checked_root(requested_root, false)?;
    ensure!(
        !std::env::current_exe()?.canonicalize()?.starts_with(&root),
        "Run the uninstaller engine from outside the installation root"
    );
    let _lease = acquire_lease(&root, true)?;
    ensure!(
        !root.join(JOURNAL).exists(),
        "Run installation recover before uninstalling"
    );
    let mut state = load_state(&root)?;
    let payloads: Vec<_> = state
        .current
        .iter()
        .map(|payload| (root.join("app"), payload))
        .chain(
            state
                .history
                .iter()
                .map(|entry| (root.join(&entry.directory), &entry.payload)),
        )
        .collect();
    // Check the whole removal plan before deleting any program file. Modified
    // and unexpected files are preserved and require an explicit user decision.
    for (directory, payload) in &payloads {
        if directory.exists() {
            verify_payload(directory, payload, state.uninstalling)?;
            preflight_writable(directory, payload, state.uninstalling)?;
        } else {
            ensure!(
                state.uninstalling,
                "An owned payload is missing; preserve the root and repair it first"
            );
        }
    }
    state.uninstalling = true;
    atomic_json(&root.join(MARKER), &state)?;
    for (directory, payload) in payloads {
        if !directory.exists() {
            continue;
        }
        if !user_artifacts(&directory)?.is_empty() {
            let preserved = root.join("preserved-data").join(unique_id());
            fs::create_dir_all(&preserved)?;
            move_user_data(&directory, &preserved)?;
        }
        remove_payload(&directory, payload, true)?;
    }
    atomic_json(&root.join(MARKER), &State::default())?;
    Ok(())
}

fn move_user_data(from: &Path, to: &Path) -> Result<()> {
    no_reparse_ancestors(to)?;
    for source in user_artifacts(from)? {
        let name = source.file_name().context("Missing user artifact name")?;
        let destination = to.join(name);
        no_reparse_if_exists(&destination)?;
        ensure!(
            !destination.exists(),
            "Both payloads contain {}; preserve the root and resolve the data conflict manually",
            name.to_string_lossy()
        );
        fs::rename(&source, &destination)
            .with_context(|| format!("Could not preserve user artifact {}", source.display()))?;
    }
    Ok(())
}

fn is_user_artifact(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    USER_DIRS
        .iter()
        .any(|directory| name == *directory || name.starts_with(&format!(".{directory}.config-")))
}

fn check_user_artifact(path: &Path) -> Result<()> {
    no_reparse_if_exists(path)?;
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() || metadata.is_dir(),
        "User artifact must be a regular file or directory: {}",
        path.display()
    );
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            check_user_artifact(&entry?.path())?;
        }
    }
    Ok(())
}

fn user_artifacts(root: &Path) -> Result<Vec<PathBuf>> {
    let mut artifacts = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_str().is_some_and(is_user_artifact) {
            check_user_artifact(&entry.path())?;
            artifacts.push(entry.path());
        }
    }
    Ok(artifacts)
}

fn checked_root(path: &Path, create: bool) -> Result<PathBuf> {
    let root = prospective_root(path)?;
    if !root.exists() {
        ensure!(create, "Installation root does not exist");
        fs::create_dir_all(&root)?;
    }
    ensure!(root.is_dir(), "Installation root is not a directory");
    if !root.join(MARKER).exists() && !root.join(JOURNAL).exists() {
        ensure!(
            create,
            "This root has no ProtoInputMultiPlex ownership marker"
        );
        for item in fs::read_dir(&root)? {
            let item = item?;
            let metadata = fs::symlink_metadata(item.path())?;
            ensure!(
                item.file_name() == LEASE && metadata.is_file() && metadata.len() == 0,
                "Refusing a nonempty unowned installation root"
            );
            no_reparse_if_exists(&item.path())?;
        }
    }
    for name in [MARKER, JOURNAL, "app", "releases", STAGE, "preserved-data"] {
        no_reparse_if_exists(&root.join(name))?;
    }
    Ok(root)
}

fn prospective_root(path: &Path) -> Result<PathBuf> {
    ensure!(
        path.is_absolute(),
        "Installation root must be an absolute path"
    );
    ensure!(
        !path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir)),
        "Installation root must not contain dot components"
    );
    ensure!(
        path.file_name().is_some(),
        "A filesystem root cannot be an installation root"
    );
    no_reparse_ancestors(path)?;
    let mut ancestor = path;
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        suffix.push(
            ancestor
                .file_name()
                .context("Missing installation root ancestor")?
                .to_os_string(),
        );
        ancestor = ancestor
            .parent()
            .context("Missing installation root parent")?;
    }
    let mut root = ancestor.canonicalize()?;
    for name in suffix.into_iter().rev() {
        root.push(name);
    }
    Ok(root)
}

fn load_state(root: &Path) -> Result<State> {
    let state = read_json(&root.join(MARKER))
        .context("Invalid or missing installation ownership marker")?;
    validate_state(&state)?;
    Ok(state)
}

fn validate_state(state: &State) -> Result<()> {
    ensure!(
        state.schema_version == 1 && state.application == INSTALLATION_IDENTITY,
        "Unsupported installation ownership marker"
    );
    if let Some(payload) = &state.current {
        validate_payload(payload)?;
    }
    let mut seen = BTreeSet::new();
    for entry in &state.history {
        validate_archive(&entry.directory)?;
        ensure!(
            seen.insert(entry.directory.to_ascii_lowercase()),
            "Duplicate retained payload directory"
        );
        validate_payload(&entry.payload)?;
    }
    Ok(())
}

fn validate_archive(path: &str) -> Result<()> {
    ensure!(
        path.starts_with("releases/") && path.split('/').count() == 2,
        "Invalid retained release location"
    );
    validate_relative(path)
}

fn read_bundle(root: &Path) -> Result<Payload> {
    let manifest = root.join(MANIFEST);
    no_reparse_if_exists(&manifest)?;
    let bundle: Bundle = read_json(&manifest)?;
    ensure!(
        bundle.schema_version == 1
            && bundle.application == crate::APP_NAME
            && bundle.platform == "windows-x64",
        "Unsupported portable bundle manifest"
    );
    let mut payload = Payload {
        version: bundle.version,
        files: bundle.files,
    };
    ensure!(
        !payload
            .files
            .iter()
            .any(|entry| entry.path.eq_ignore_ascii_case(MANIFEST)),
        "Bundle manifest must not hash itself"
    );
    payload.files.push(Entry {
        path: MANIFEST.into(),
        sha256: hash_file(&manifest)?,
        bytes: manifest.metadata()?.len(),
    });
    validate_payload(&payload)?;
    Ok(payload)
}

fn validate_payload(payload: &Payload) -> Result<()> {
    ensure!(
        !payload.version.is_empty() && payload.files.len() <= 100_000,
        "Invalid payload metadata"
    );
    let mut names = BTreeSet::new();
    for entry in &payload.files {
        validate_relative(&entry.path)?;
        ensure!(
            !entry.path.split('/').next().is_some_and(is_user_artifact),
            "Bundle may not own user data or configuration maintenance artifacts"
        );
        ensure!(
            entry.sha256.len() == 64 && entry.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Invalid SHA-256 value"
        );
        ensure!(
            names.insert(entry.path.to_ascii_lowercase()),
            "Case-insensitive duplicate payload path"
        );
    }
    for name in &names {
        let mut parent = name.as_str();
        while let Some((prefix, _)) = parent.rsplit_once('/') {
            ensure!(
                !names.contains(prefix),
                "Payload path is both a file and a directory"
            );
            parent = prefix;
        }
    }
    ensure!(
        names.contains("protoinputmultiplex.exe") && names.contains(MANIFEST),
        "Bundle is missing the application or manifest"
    );
    for bits in [32, 64] {
        for (kind, extension) in [
            ("hooks", "dll"),
            ("ij", "exe"),
            ("ijp", "dll"),
            ("loader", "dll"),
        ] {
            ensure!(
                names.contains(&format!("runtime/protoinput{kind}{bits}.{extension}")),
                "Bundle is missing a required ProtoInput runtime component"
            );
        }
        ensure!(
            names.contains(&format!(
                "runtime/protoinputmultiplex_cursor_observer{bits}.dll"
            )),
            "Bundle is missing a cursor observer helper"
        );
    }
    Ok(())
}

fn validate_relative(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty() && path.is_ascii() && !path.contains('\\'),
        "Payload paths must be slash-separated ASCII relative names"
    );
    for part in path.split('/') {
        ensure!(
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with(['.', ' '])
                && !part
                    .bytes()
                    .any(|byte| byte < 32 || b"<>:\"|?*".contains(&byte)),
            "Unsafe payload path: {path}"
        );
        let stem = part
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        ensure!(
            ![
                "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
                "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8",
                "LPT9"
            ]
            .contains(&stem.as_str()),
            "Reserved Windows filename: {path}"
        );
    }
    Ok(())
}

fn verify_payload(root: &Path, payload: &Payload, allow_missing: bool) -> Result<()> {
    no_reparse_ancestors(root)?;
    let expected: BTreeSet<_> = payload
        .files
        .iter()
        .map(|entry| entry.path.to_ascii_lowercase())
        .collect();
    check_tree(root, root, &expected)?;
    for entry in &payload.files {
        let path = root.join(&entry.path);
        if allow_missing && !path.exists() {
            continue;
        }
        ensure!(
            path.is_file(),
            "Payload file is missing: {}",
            path.display()
        );
        ensure!(
            path.metadata()?.len() == entry.bytes
                && hash_file(&path)?.eq_ignore_ascii_case(&entry.sha256),
            "Payload hash mismatch; preserved file: {}",
            path.display()
        );
    }
    Ok(())
}

fn check_tree(base: &Path, directory: &Path, expected: &BTreeSet<String>) -> Result<()> {
    for item in fs::read_dir(directory)? {
        let item = item?;
        let path = item.path();
        no_reparse_if_exists(&path)?;
        let relative = path
            .strip_prefix(base)?
            .to_str()
            .context("Non-Unicode payload path")?
            .replace('\\', "/");
        if !relative.contains('/') && is_user_artifact(&relative) {
            if USER_DIRS
                .iter()
                .any(|name| relative.eq_ignore_ascii_case(name))
            {
                ensure!(path.is_dir(), "User data location must be a directory");
            }
            check_user_artifact(&path)?;
            continue;
        }
        if path.is_dir() {
            ensure!(
                expected
                    .iter()
                    .any(|name| name.starts_with(&(relative.to_ascii_lowercase() + "/"))),
                "Unexpected payload directory preserved: {}",
                path.display()
            );
            check_tree(base, &path, expected)?;
        } else {
            ensure!(
                expected.contains(&relative.to_ascii_lowercase()),
                "Unexpected payload file preserved: {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn preflight_writable(root: &Path, payload: &Payload, allow_missing: bool) -> Result<()> {
    for entry in &payload.files {
        let path = root.join(&entry.path);
        if allow_missing && !path.exists() {
            continue;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        let _file = options.open(&path).with_context(|| {
            format!(
                "Owned file is locked or read-only; close installed processes: {}",
                path.display()
            )
        })?;
    }
    Ok(())
}

fn remove_payload(root: &Path, payload: &Payload, allow_missing: bool) -> Result<()> {
    verify_payload(root, payload, allow_missing)?;
    remove_known_files(root, payload)
}

fn remove_known_files(root: &Path, payload: &Payload) -> Result<()> {
    for entry in &payload.files {
        let path = root.join(&entry.path);
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    remove_empty_directories(root)?;
    Ok(())
}

fn remove_empty_directories(root: &Path) -> Result<()> {
    for item in fs::read_dir(root)? {
        let path = item?.path();
        no_reparse_if_exists(&path)?;
        ensure!(
            path.is_dir(),
            "Unexpected file preserved: {}",
            path.display()
        );
        remove_empty_directories(&path)?;
    }
    fs::remove_dir(root).with_context(|| {
        format!(
            "Directory contains preserved user files: {}",
            root.display()
        )
    })
}

fn no_reparse_if_exists(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                !metadata.file_type().is_symlink(),
                "Symbolic links are not permitted: {}",
                path.display()
            );
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                ensure!(
                    metadata.file_attributes() & 0x400 == 0,
                    "Reparse points are not permitted: {}",
                    path.display()
                );
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn no_reparse_ancestors(path: &Path) -> Result<()> {
    for parent in path.ancestors() {
        no_reparse_if_exists(parent)?;
    }
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    no_reparse_ancestors(path)?;
    let file = File::open(path)?;
    ensure!(
        file.metadata()?.len() <= 32 * 1024 * 1024,
        "Installation metadata is too large"
    );
    Ok(serde_json::from_reader(file)?)
}

fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    no_reparse_if_exists(path)?;
    let temp = path.with_extension(format!("{}.tmp", unique_id()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        file.write_all(&serde_json::to_vec_pretty(value)?)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let from: Vec<_> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<_> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: both path buffers are valid nul-terminated UTF-16 strings.
    ensure!(
        unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } != 0,
        "Could not commit installation metadata: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> Result<()> {
    Ok(fs::rename(from, to)?)
}

fn unique_id() -> String {
    format!(
        "{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id()
    )
}

// Streaming SHA-256 keeps manifest validation independent of external programs
// and adds no dependency or dynamic runtime to the pinned application.
fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash.finish())
}

struct Sha256 {
    state: [u32; 8],
    block: [u8; 64],
    used: usize,
    bytes: u64,
}

impl Sha256 {
    fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            block: [0; 64],
            used: 0,
            bytes: 0,
        }
    }
    fn update(&mut self, input: &[u8]) {
        self.bytes = self.bytes.wrapping_add(input.len() as u64);
        for &byte in input {
            self.block[self.used] = byte;
            self.used += 1;
            if self.used == 64 {
                self.compress();
                self.used = 0;
            }
        }
    }
    fn finish(mut self) -> String {
        let bits = self.bytes.wrapping_mul(8);
        self.update(&[0x80]);
        while self.used != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_be_bytes());
        self.state
            .iter()
            .map(|word| format!("{word:08x}"))
            .collect()
    }
    fn compress(&mut self) {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let mut words = [0_u32; 64];
        for (index, bytes) in self.block.as_chunks::<4>().0.iter().enumerate() {
            words[index] = u32::from_be_bytes(*bytes);
        }
        for index in 16..64 {
            let x = words[index - 15];
            let y = words[index - 2];
            words[index] = words[index - 16]
                .wrapping_add(x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3))
                .wrapping_add(words[index - 7])
                .wrapping_add(y.rotate_right(17) ^ y.rotate_right(19) ^ (y >> 10));
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for index in 0..64 {
            let t1 = h
                .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
                .wrapping_add((e & f) ^ (!e & g))
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
                .wrapping_add((a & b) ^ (a & c) ^ (b & c));
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (state, value) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *state = state.wrapping_add(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "ProtoInputMultiPlex install fixture {}",
                unique_id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn bundle(&self, name: &str, version: &str) -> PathBuf {
            let root = self.0.join(name);
            fs::create_dir(&root).unwrap();
            let mut names = vec!["protoinputmultiplex.exe".to_string()];
            for bits in [32, 64] {
                for (kind, extension) in [
                    ("Hooks", "dll"),
                    ("IJ", "exe"),
                    ("IJP", "dll"),
                    ("Loader", "dll"),
                ] {
                    names.push(format!("runtime/ProtoInput{kind}{bits}.{extension}"));
                }
                names.push(format!(
                    "runtime/protoinputmultiplex_cursor_observer{bits}.dll"
                ));
            }
            let files: Vec<_> = names
                .iter()
                .map(|name| {
                    let path = root.join(name);
                    fs::create_dir_all(path.parent().unwrap()).unwrap();
                    fs::write(&path, format!("{version}:{name}")).unwrap();
                    Entry {
                        path: name.clone(),
                        bytes: path.metadata().unwrap().len(),
                        sha256: hash_file(&path).unwrap(),
                    }
                })
                .collect();
            fs::write(
                root.join(MANIFEST),
                serde_json::to_vec(&serde_json::json!({
                "schema_version":1,"application":crate::APP_NAME,"version":version,
                "platform":"windows-x64","files":files}))
                .unwrap(),
            )
            .unwrap();
            root
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn seed_config_artifacts(base: &Path) -> Vec<(String, Vec<u8>)> {
        let mut files = Vec::new();
        for name in USER_DIRS {
            let original = format!(".{name}.config-original-123-1");
            let stage = format!(".{name}.config-stage-123-1");
            let journal = serde_json::to_vec(&serde_json::json!({
                "version":1,"original_existed":true,"stage_name":stage,"original_name":original,
            }))
            .unwrap();
            files.extend([
                (format!(".{name}.config-lock"), Vec::new()),
                (format!(".{name}.config-transaction.json"), journal),
                (format!("{original}/nested/opaque.bin"), vec![0, 255, 17, 0]),
                (
                    format!("{stage}/candidate.json"),
                    b"opaque candidate bytes".to_vec(),
                ),
            ]);
            fs::create_dir_all(base.join(&original).join("empty")).unwrap();
        }
        for (name, bytes) in &files {
            let path = base.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        files
    }

    fn assert_config_artifacts(base: &Path, files: &[(String, Vec<u8>)]) {
        for (name, bytes) in files {
            assert_eq!(
                fs::read(base.join(name)).unwrap(),
                *bytes,
                "changed artifact {name}"
            );
        }
        for name in USER_DIRS {
            assert!(
                base.join(format!(".{name}.config-original-123-1/empty"))
                    .is_dir()
            );
        }
    }

    #[test]
    fn sha256_known_vectors_and_chunk_boundaries() {
        for (input, expected) in [
            (
                "",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                "abc",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
        ] {
            let mut hash = Sha256::new();
            for chunk in input.as_bytes().chunks(3) {
                hash.update(chunk);
            }
            assert_eq!(hash.finish(), expected);
        }
    }

    #[test]
    fn install_upgrade_uninstall_preserves_user_data() {
        let fixture = Fixture::new();
        let v1 = fixture.bundle("bundle 1", "1");
        let v2 = fixture.bundle("bundle 2", "2");
        let root = fixture.0.join("installed app");
        install(&v1, &root, false).unwrap();
        fs::create_dir(root.join("app/data")).unwrap();
        fs::write(root.join("app/data/preferences.json"), b"keep").unwrap();
        fs::create_dir(root.join("data")).unwrap();
        fs::write(root.join("data/root-preferences.json"), b"root keep").unwrap();
        let artifacts = seed_config_artifacts(&root.join("app"));
        install(&v2, &root, false).unwrap();
        assert_config_artifacts(&root.join("app"), &artifacts);
        assert_eq!(
            fs::read(root.join("app/data/preferences.json")).unwrap(),
            b"keep"
        );
        assert_eq!(load_state(&root).unwrap().history.len(), 1);
        uninstall(&root).unwrap();
        assert!(!root.join("app").exists());
        assert_eq!(
            fs::read(root.join("data/root-preferences.json")).unwrap(),
            b"root keep"
        );
        let preserved = fs::read_dir(root.join("preserved-data"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            fs::read(preserved.join("data/preferences.json")).unwrap(),
            b"keep"
        );
        assert_config_artifacts(&preserved, &artifacts);
        install(&v1, &root, false).unwrap();
    }

    #[test]
    fn rejects_corruption_unknown_files_and_active_lease_without_mutation() {
        let fixture = Fixture::new();
        let bundle = fixture.bundle("bundle", "1");
        let root = fixture.0.join("root");
        install(&bundle, &root, false).unwrap();
        let lease = acquire_lease(&root, false).unwrap();
        assert!(uninstall(&root).is_err());
        drop(lease);
        fs::write(root.join("app/private.txt"), b"keep").unwrap();
        assert!(uninstall(&root).is_err());
        assert!(root.join("app/protoinputmultiplex.exe").exists());
        fs::remove_file(root.join("app/private.txt")).unwrap();
        fs::write(bundle.join("protoinputmultiplex.exe"), b"corrupt").unwrap();
        assert!(install(&bundle, &root, false).is_err());
        assert_eq!(load_state(&root).unwrap().current.unwrap().version, "1");
    }

    #[test]
    fn interrupted_swap_recovers_user_directories_and_previous_payload() {
        let fixture = Fixture::new();
        let v1 = fixture.bundle("one", "1");
        let v2 = fixture.bundle("two", "2");
        let root = fixture.0.join("root");
        install(&v1, &root, false).unwrap();
        fs::create_dir(root.join("app/profiles")).unwrap();
        fs::write(root.join("app/profiles/mine.json"), b"keep").unwrap();
        let artifacts = seed_config_artifacts(&root.join("app"));
        let before = load_state(&root).unwrap();
        let incoming = read_bundle(&v2).unwrap();
        let archived = "releases/interrupted".to_string();
        fs::create_dir(root.join("releases")).unwrap();
        atomic_json(
            &root.join(JOURNAL),
            &Journal {
                schema_version: 1,
                before,
                incoming: incoming.clone(),
                archived: Some(archived.clone()),
            },
        )
        .unwrap();
        fs::rename(root.join("app"), root.join(&archived)).unwrap();
        fs::create_dir(root.join(STAGE)).unwrap();
        // Simulate a failure before all incoming files have been copied.
        fs::copy(
            v2.join("protoinputmultiplex.exe"),
            root.join(STAGE).join("protoinputmultiplex.exe"),
        )
        .unwrap();
        move_user_data(&root.join(&archived), &root.join(STAGE)).unwrap();
        assert!(recover(&root).unwrap());
        assert_eq!(
            fs::read(root.join("app/profiles/mine.json")).unwrap(),
            b"keep"
        );
        assert_eq!(load_state(&root).unwrap().current.unwrap().version, "1");
        assert_config_artifacts(&root.join("app"), &artifacts);
        assert!(!root.join(STAGE).exists());
        assert!(!root.join(JOURNAL).exists());
    }

    #[test]
    fn path_validation_rejects_windows_aliases_and_escape_names() {
        for path in [
            "../escape",
            "/absolute",
            "runtime//file",
            "runtime/CON.txt",
            "file:stream",
            "file.",
            "file ",
            "a\\b",
            "a/../b",
        ] {
            assert!(validate_relative(path).is_err(), "accepted {path}");
        }
    }

    #[test]
    fn successful_release_rollback_carries_user_data_forward() {
        let fixture = Fixture::new();
        let v1 = fixture.bundle("one", "1");
        let v2 = fixture.bundle("two", "2");
        let root = fixture.0.join("root");
        install(&v1, &root, false).unwrap();
        install(&v2, &root, false).unwrap();
        fs::create_dir(root.join("app/macros")).unwrap();
        fs::write(root.join("app/macros/mine.json"), b"new user data").unwrap();
        let artifacts = seed_config_artifacts(&root.join("app"));
        let previous = load_state(&root)
            .unwrap()
            .history
            .last()
            .unwrap()
            .directory
            .clone();
        install(&root.join(previous), &root, true).unwrap();
        assert_eq!(load_state(&root).unwrap().current.unwrap().version, "1");
        assert_eq!(
            fs::read(root.join("app/macros/mine.json")).unwrap(),
            b"new user data"
        );
        assert_eq!(load_state(&root).unwrap().history.len(), 2);
        assert_config_artifacts(&root.join("app"), &artifacts);
    }

    #[test]
    fn crash_after_new_marker_restores_old_payload_and_data() {
        let fixture = Fixture::new();
        let v1 = fixture.bundle("one", "1");
        let v2 = fixture.bundle("two", "2");
        let root = fixture.0.join("root");
        install(&v1, &root, false).unwrap();
        let before = load_state(&root).unwrap();
        install(&v2, &root, false).unwrap();
        let after = load_state(&root).unwrap();
        fs::create_dir(root.join("app/data")).unwrap();
        fs::write(root.join("app/data/keep.json"), b"keep").unwrap();
        let artifacts = seed_config_artifacts(&root.join("app"));
        atomic_json(
            &root.join(JOURNAL),
            &Journal {
                schema_version: 1,
                before,
                incoming: after.current.clone().unwrap(),
                archived: Some(after.history.last().unwrap().directory.clone()),
            },
        )
        .unwrap();
        assert!(recover(&root).unwrap());
        assert_eq!(load_state(&root).unwrap().current.unwrap().version, "1");
        assert_eq!(fs::read(root.join("app/data/keep.json")).unwrap(), b"keep");
        assert_config_artifacts(&root.join("app"), &artifacts);
        assert!(!root.join(JOURNAL).exists());
    }

    #[test]
    fn interrupted_uninstall_can_be_retried_without_removing_user_data() {
        let fixture = Fixture::new();
        let bundle = fixture.bundle("bundle", "1");
        let root = fixture.0.join("root");
        install(&bundle, &root, false).unwrap();
        fs::create_dir(root.join("data")).unwrap();
        fs::write(root.join("data/keep.json"), b"keep").unwrap();
        let artifacts = seed_config_artifacts(&root.join("app"));
        let mut state = load_state(&root).unwrap();
        state.uninstalling = true;
        atomic_json(&root.join(MARKER), &state).unwrap();
        fs::remove_file(root.join("app/protoinputmultiplex.exe")).unwrap();
        uninstall(&root).unwrap();
        assert!(load_state(&root).unwrap().current.is_none());
        assert_eq!(fs::read(root.join("data/keep.json")).unwrap(), b"keep");
        let preserved = fs::read_dir(root.join("preserved-data"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_config_artifacts(&preserved, &artifacts);
    }

    #[test]
    fn duplicate_case_insensitive_manifest_entries_are_rejected() {
        let fixture = Fixture::new();
        let bundle = fixture.bundle("bundle", "1");
        let path = bundle.join(MANIFEST);
        let mut manifest: serde_json::Value = read_json(&path).unwrap();
        let files = manifest["files"].as_array_mut().unwrap();
        let mut duplicate = files[0].clone();
        duplicate["path"] = "PROTOINPUTMULTIPLEX.EXE".into();
        files.push(duplicate);
        fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(read_bundle(&bundle).is_err());
    }

    #[test]
    fn overlap_rejection_does_not_create_a_directory_inside_bundle() {
        let fixture = Fixture::new();
        let bundle = fixture.bundle("bundle", "1");
        let nested = bundle.join("unwanted child");
        assert!(install(&bundle, &nested, false).is_err());
        assert!(!nested.exists());
    }

    #[test]
    fn bundle_manifest_cannot_own_configuration_maintenance_artifacts() {
        let fixture = Fixture::new();
        let bundle = fixture.bundle("bundle", "1");
        let original = read_bundle(&bundle).unwrap();
        for path in [
            ".data.config-lock",
            ".profiles.config-original-123-1/profile.json",
            ".macros.config-stage-123-1/file",
            ".DATA.CONFIG-transaction.json",
        ] {
            let mut payload = original.clone();
            payload.files.push(Entry {
                path: path.into(),
                bytes: 0,
                sha256: "0".repeat(64),
            });
            assert!(validate_payload(&payload).is_err(), "bundle owned {path}");
        }
    }

    #[test]
    fn external_configuration_lease_excludes_installer_mutation_and_pending_recovery() {
        let fixture = Fixture::new();
        let bundle = fixture.bundle("bundle", "1");
        let root = fixture.0.join("root");
        install(&bundle, &root, false).unwrap();
        let lease = data_root_lease(&root.join("app/data/not-created-yet"))
            .unwrap()
            .unwrap();
        assert!(install(&bundle, &root, false).is_err());
        assert!(uninstall(&root).is_err());
        drop(lease);
        let exclusive = acquire_lease(&root, true).unwrap();
        assert!(data_root_lease(&root.join("app/profiles")).is_err());
        drop(exclusive);
        fs::write(root.join(JOURNAL), b"pending transaction").unwrap();
        assert!(data_root_lease(&root.join("app/macros")).is_err());
        fs::remove_file(root.join(JOURNAL)).unwrap();
        assert!(
            data_root_lease(&fixture.0.join("ordinary data"))
                .unwrap()
                .is_none()
        );
    }
}
