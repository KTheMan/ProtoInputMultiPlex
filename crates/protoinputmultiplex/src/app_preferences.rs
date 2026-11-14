use anyhow::{Context, Result, bail, ensure};
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub const VERSION: u32 = 1;
pub const MAX_PREFERENCES_BYTES: usize = 64 * 1024;
pub const MIN_ZOOM_PERCENT: u16 = 50;
pub const MAX_ZOOM_PERCENT: u16 = 200;
pub const ZOOM_STEP_PERCENT: u16 = 5;
const MAX_SHORTCUTS: usize = 7;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

pub const EMERGENCY_STOP_SHORTCUT: Shortcut = Shortcut {
    action: Action::Stop,
    modifiers: Modifiers {
        control: true,
        alt: true,
        shift: true,
        windows: false,
    },
    key: Key::F11,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AppearanceMode {
    #[default]
    Dark,
    Light,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Stop,
    PauseResume,
    ToggleMode,
    ToggleRouting,
    Switched,
    Concurrent,
    ToggleKeyboard,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,
    F21,
    F22,
    F23,
    F24,
    Escape,
    Enter,
    Space,
    Tab,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Left,
    Right,
    Up,
    Down,
}
impl Key {
    pub const fn virtual_key(self) -> u32 {
        match self {
            Self::A => 0x41,
            Self::B => 0x42,
            Self::C => 0x43,
            Self::D => 0x44,
            Self::E => 0x45,
            Self::F => 0x46,
            Self::G => 0x47,
            Self::H => 0x48,
            Self::I => 0x49,
            Self::J => 0x4A,
            Self::K => 0x4B,
            Self::L => 0x4C,
            Self::M => 0x4D,
            Self::N => 0x4E,
            Self::O => 0x4F,
            Self::P => 0x50,
            Self::Q => 0x51,
            Self::R => 0x52,
            Self::S => 0x53,
            Self::T => 0x54,
            Self::U => 0x55,
            Self::V => 0x56,
            Self::W => 0x57,
            Self::X => 0x58,
            Self::Y => 0x59,
            Self::Z => 0x5A,
            Self::Digit0 => 0x30,
            Self::Digit1 => 0x31,
            Self::Digit2 => 0x32,
            Self::Digit3 => 0x33,
            Self::Digit4 => 0x34,
            Self::Digit5 => 0x35,
            Self::Digit6 => 0x36,
            Self::Digit7 => 0x37,
            Self::Digit8 => 0x38,
            Self::Digit9 => 0x39,
            Self::F1 => 0x70,
            Self::F2 => 0x71,
            Self::F3 => 0x72,
            Self::F4 => 0x73,
            Self::F5 => 0x74,
            Self::F6 => 0x75,
            Self::F7 => 0x76,
            Self::F8 => 0x77,
            Self::F9 => 0x78,
            Self::F10 => 0x79,
            Self::F11 => 0x7A,
            Self::F12 => 0x7B,
            Self::F13 => 0x7C,
            Self::F14 => 0x7D,
            Self::F15 => 0x7E,
            Self::F16 => 0x7F,
            Self::F17 => 0x80,
            Self::F18 => 0x81,
            Self::F19 => 0x82,
            Self::F20 => 0x83,
            Self::F21 => 0x84,
            Self::F22 => 0x85,
            Self::F23 => 0x86,
            Self::F24 => 0x87,
            Self::Escape => 0x1B,
            Self::Enter => 0x0D,
            Self::Space => 0x20,
            Self::Tab => 0x09,
            Self::Backspace => 0x08,
            Self::Delete => 0x2E,
            Self::Insert => 0x2D,
            Self::Home => 0x24,
            Self::End => 0x23,
            Self::PageUp => 0x21,
            Self::PageDown => 0x22,
            Self::Left => 0x25,
            Self::Up => 0x26,
            Self::Right => 0x27,
            Self::Down => 0x28,
        }
    }
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(default, deny_unknown_fields)]
pub struct Modifiers {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub windows: bool,
}
impl Modifiers {
    pub const fn has_safe_prefix(self) -> bool {
        self.control || self.alt || self.windows
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct Shortcut {
    pub action: Action,
    pub modifiers: Modifiers,
    pub key: Key,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Preferences {
    pub version: u32,
    pub appearance_mode: AppearanceMode,
    pub zoom_percent: u16,
    pub show_app_launcher: bool,
    pub launch_minimized: bool,
    pub minimize_to_tray: bool,
    pub close_to_tray: bool,
    pub launch_at_login: bool,
    pub load_profile_at_startup: bool,
    pub save_profile_on_exit: bool,
    pub shortcuts: Vec<Shortcut>,
}

/// A complete, narrowly scoped replacement authorized by stopped setup access.
/// Live app-preference Configure deliberately does not accept these fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProfilePersistence {
    pub load_profile_at_startup: bool,
    pub save_profile_on_exit: bool,
}

impl Preferences {
    pub fn profile_persistence(&self) -> ProfilePersistence {
        ProfilePersistence {
            load_profile_at_startup: self.load_profile_at_startup,
            save_profile_on_exit: self.save_profile_on_exit,
        }
    }

    pub fn with_profile_persistence(&self, policy: ProfilePersistence) -> Self {
        Self {
            load_profile_at_startup: policy.load_profile_at_startup,
            save_profile_on_exit: policy.save_profile_on_exit,
            ..self.clone()
        }
    }
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            version: VERSION,
            appearance_mode: AppearanceMode::Dark,
            zoom_percent: 100,
            show_app_launcher: true,
            launch_minimized: false,
            minimize_to_tray: false,
            close_to_tray: false,
            launch_at_login: false,
            load_profile_at_startup: true,
            save_profile_on_exit: false,
            shortcuts: vec![EMERGENCY_STOP_SHORTCUT],
        }
    }
}
impl Preferences {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == VERSION,
            "unsupported app preferences version"
        );
        ensure!(
            (MIN_ZOOM_PERCENT..=MAX_ZOOM_PERCENT).contains(&self.zoom_percent)
                && self.zoom_percent.is_multiple_of(ZOOM_STEP_PERCENT),
            "zoom must be 50–200 percent in 5-point steps"
        );
        ensure!(
            self.shortcuts.len() <= MAX_SHORTCUTS,
            "too many app shortcuts"
        );
        let mut actions = std::collections::BTreeSet::new();
        let mut chords = std::collections::BTreeSet::new();
        for shortcut in &self.shortcuts {
            ensure!(
                shortcut.modifiers.has_safe_prefix(),
                "a shortcut must include Ctrl, Alt, or Windows"
            );
            ensure!(
                actions.insert(shortcut.action),
                "an app action can have only one shortcut"
            );
            ensure!(
                shortcut.action == Action::Stop || !same_chord(*shortcut, EMERGENCY_STOP_SHORTCUT),
                "Ctrl+Alt+Shift+F11 is reserved for Stop"
            );
            ensure!(
                chords.insert((
                    shortcut.modifiers.control,
                    shortcut.modifiers.alt,
                    shortcut.modifiers.shift,
                    shortcut.modifiers.windows,
                    shortcut.key.virtual_key()
                )),
                "app shortcuts cannot share a key chord"
            );
        }
        Ok(())
    }
}

pub const fn same_chord(left: Shortcut, right: Shortcut) -> bool {
    left.modifiers.control == right.modifiers.control
        && left.modifiers.alt == right.modifiers.alt
        && left.modifiers.shift == right.modifiers.shift
        && left.modifiers.windows == right.modifiers.windows
        && left.key.virtual_key() == right.key.virtual_key()
}

pub fn decode(bytes: &[u8]) -> Result<Preferences> {
    ensure!(
        bytes.len() <= MAX_PREFERENCES_BYTES,
        "app preferences exceed the size limit"
    );
    let preferences: Preferences =
        serde_json::from_slice(bytes).context("parse app preferences")?;
    preferences.validate()?;
    Ok(preferences)
}

pub fn encode(preferences: &Preferences) -> Result<Vec<u8>> {
    preferences.validate()?;
    let bytes = serde_json::to_vec_pretty(preferences).context("serialize app preferences")?;
    ensure!(
        bytes.len() <= MAX_PREFERENCES_BYTES,
        "app preferences exceed the size limit"
    );
    Ok(bytes)
}

pub struct Store;
impl Store {
    pub fn load(path: &Path) -> Result<Preferences> {
        match Self::original(path)? {
            Some(bytes) => decode(&bytes),
            None => Ok(Preferences::default()),
        }
    }

    pub fn original(path: &Path) -> Result<Option<Vec<u8>>> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).with_context(|| format!("open {}", path.display())),
        };
        let mut bytes = Vec::new();
        file.take(MAX_PREFERENCES_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        decode(&bytes)?;
        Ok(Some(bytes))
    }

    pub fn save(path: &Path, preferences: &Preferences) -> Result<()> {
        let bytes = encode(preferences)?;
        // Preserve damaged existing files for explicit recovery.
        Self::original(path)?;
        Self::replace(path, &bytes)
    }

    pub fn restore_original(path: &Path, original: Option<&[u8]>) -> Result<()> {
        if let Some(bytes) = original {
            decode(bytes)?;
            Self::replace(path, bytes)
        } else {
            match fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error).context("remove uncommitted preferences file"),
            }
        }
    }

    fn replace(path: &Path, bytes: &[u8]) -> Result<()> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .with_context(|| format!("create preferences directory {}", parent.display()))?;
        let file_name = path
            .file_name()
            .context("app preferences path has no file name")?
            .to_string_lossy();
        let mut temporary = None;
        let mut file = None;
        for _ in 0..8 {
            let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let candidate = parent.join(format!(
                ".{file_name}.tmp.{}.{}",
                std::process::id(),
                sequence
            ));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(opened) => {
                    temporary = Some(candidate);
                    file = Some(opened);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error).context("create app preferences temporary file"),
            }
        }
        let temporary =
            temporary.context("could not allocate an app preferences temporary file")?;
        let mut cleanup = TempFile(Some(temporary.clone()));
        let mut file = file.context("app preferences temporary file handle missing")?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, path)?;
        cleanup.0 = None;
        Ok(())
    }
}

struct TempFile(Option<PathBuf>);
impl Drop for TempFile {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let from_wide = from
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let to_wide = to
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let moved = unsafe {
        MoveFileExW(
            from_wide.as_ptr(),
            to_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    ensure!(
        moved != 0,
        "replace app preferences: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> Result<()> {
    fs::rename(from, to).context("replace app preferences")
}

pub fn default_path() -> Result<PathBuf> {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .context("LOCALAPPDATA is unavailable; pass --preferences explicitly")?;
    Ok(PathBuf::from(local_app_data)
        .join("ProtoInputMultiPlex")
        .join("app-preferences.json"))
}

pub fn validate_startup_test_key(value: &str) -> Result<()> {
    const PREFIX: &str = "Software\\ProtoInputMultiPlex\\Tests\\";
    ensure!(
        value.starts_with(PREFIX),
        "startup test key must be under {PREFIX}"
    );
    ensure!(
        value.len() <= 512 && !value.chars().any(char::is_control),
        "startup test key is invalid"
    );
    ensure!(
        value.len() > PREFIX.len(),
        "startup test key must include a disposable subkey"
    );
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupValue {
    pub kind: u32,
    pub data: Vec<u8>,
}

pub trait StartupRegistration: Send {
    /// Returns the exact existing registry type and bytes for rollback.
    fn snapshot(&self) -> Result<Option<StartupValue>>;
    /// Writes the exact value or removes the entry when `None` is supplied.
    fn write(&self, value: Option<&StartupValue>) -> Result<()>;
}

#[cfg(windows)]
pub struct CurrentUserRunKey {
    key_path: String,
}

#[cfg(windows)]
struct OwnedRegistryKey(windows_sys::Win32::System::Registry::HKEY);
#[cfg(windows)]
impl Drop for OwnedRegistryKey {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::System::Registry::RegCloseKey(self.0);
        }
    }
}
#[cfg(windows)]
impl CurrentUserRunKey {
    const RUN_KEY: &'static str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

    pub fn current_user() -> Self {
        Self {
            key_path: Self::RUN_KEY.into(),
        }
    }

    pub fn injected_test_key(key_path: &str) -> Result<Self> {
        validate_startup_test_key(key_path)?;
        Ok(Self {
            key_path: key_path.into(),
        })
    }
}
#[cfg(windows)]
impl StartupRegistration for CurrentUserRunKey {
    fn snapshot(&self) -> Result<Option<StartupValue>> {
        use windows_sys::Win32::{
            Foundation::ERROR_FILE_NOT_FOUND,
            System::Registry::{
                HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, RegOpenKeyExW, RegQueryValueExW,
            },
        };
        let key_path = self
            .key_path
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let name = "ProtoInputMultiPlex"
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let mut key: HKEY = std::ptr::null_mut();
        let opened = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                key_path.as_ptr(),
                0,
                KEY_QUERY_VALUE,
                &mut key,
            )
        };
        if opened == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        ensure!(opened == 0, "open current-user startup key: {opened}");
        let key = OwnedRegistryKey(key);
        let mut ty = 0u32;
        let mut bytes = 0u32;
        let queried = unsafe {
            RegQueryValueExW(
                key.0,
                name.as_ptr(),
                std::ptr::null(),
                &mut ty,
                std::ptr::null_mut(),
                &mut bytes,
            )
        };
        if queried == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if queried != 0 {
            bail!("query current-user startup value: {queried}");
        }
        ensure!(
            bytes as usize <= 32 * 1024,
            "startup value exceeds the size limit"
        );
        let mut value = vec![0u8; bytes as usize];
        let queried = unsafe {
            RegQueryValueExW(
                key.0,
                name.as_ptr(),
                std::ptr::null(),
                &mut ty,
                value.as_mut_ptr(),
                &mut bytes,
            )
        };
        ensure!(queried == 0, "read current-user startup value: {queried}");
        value.truncate(bytes as usize);
        Ok(Some(StartupValue {
            kind: ty,
            data: value,
        }))
    }

    fn write(&self, value: Option<&StartupValue>) -> Result<()> {
        use windows_sys::Win32::System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, RegCreateKeyExW,
            RegDeleteValueW, RegOpenKeyExW, RegSetValueExW,
        };
        if let Some(value) = value {
            ensure!(
                value.data.len() <= 32 * 1024,
                "startup value exceeds the size limit"
            );
        }
        let key_path = self
            .key_path
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let name = "ProtoInputMultiPlex"
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let mut key: HKEY = std::ptr::null_mut();
        let opened = unsafe {
            if value.is_some() {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    key_path.as_ptr(),
                    0,
                    std::ptr::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_SET_VALUE,
                    std::ptr::null(),
                    &mut key,
                    std::ptr::null_mut(),
                )
            } else {
                RegOpenKeyExW(
                    HKEY_CURRENT_USER,
                    key_path.as_ptr(),
                    0,
                    KEY_SET_VALUE,
                    &mut key,
                )
            }
        };
        if value.is_none() && opened == windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        ensure!(
            opened == 0,
            "open current-user startup key for update: {opened}"
        );
        let key = OwnedRegistryKey(key);
        let result = if let Some(value) = value {
            unsafe {
                RegSetValueExW(
                    key.0,
                    name.as_ptr(),
                    0,
                    value.kind,
                    value.data.as_ptr(),
                    value.data.len() as u32,
                )
            }
        } else {
            unsafe { RegDeleteValueW(key.0, name.as_ptr()) }
        };
        if value.is_none() && result == windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        ensure!(result == 0, "update current-user startup value: {result}");
        Ok(())
    }
}

#[cfg(windows)]
fn startup_shell_path(path: &Path) -> Result<std::borrow::Cow<'_, str>> {
    use std::path::{Component, Prefix};

    let text = path.to_str().context("startup path is not valid Unicode")?;
    let Some(Component::Prefix(prefix)) = path.components().next() else {
        bail!("startup path is not a Windows drive or network path");
    };
    let conventional: std::borrow::Cow<'_, str> = match prefix.kind() {
        Prefix::Disk(_) | Prefix::UNC(..) if text.contains('/') => text.replace('/', "\\").into(),
        Prefix::Disk(_) | Prefix::UNC(..) => text.into(),
        Prefix::VerbatimDisk(_) => text
            .strip_prefix(r"\\?\")
            .context("unrecognized verbatim startup drive prefix")?
            .into(),
        Prefix::VerbatimUNC(..) => format!(
            "\\\\{}",
            text.get(8..)
                .context("invalid verbatim startup UNC prefix")?
        )
        .into(),
        _ => bail!("startup path is not a Windows drive or network path"),
    };
    // A verbatim path can name files the shell would normalize or treat as a
    // device. Validate the emitted spelling before quoting it for the Run key.
    let tail = if let Some(tail) = conventional.strip_prefix(r"\\") {
        ensure!(
            tail.split('\\').count() >= 3,
            "startup network path needs a server, share and filename"
        );
        tail
    } else {
        let bytes = conventional.as_bytes();
        ensure!(
            bytes.len() >= 4 && bytes[0].is_ascii_alphabetic() && &bytes[1..3] == b":\\",
            "startup path must have an absolute drive prefix and filename"
        );
        &conventional[3..]
    };
    let mut parts = tail.split('\\');
    ensure!(
        parts.all(startup_shell_component),
        "startup path cannot be represented by the Windows shell"
    );
    Ok(conventional)
}

#[cfg(windows)]
fn startup_shell_component(name: &str) -> bool {
    if name.is_empty()
        || name.ends_with([' ', '.'])
        || name
            .chars()
            .any(|ch| ch.is_control() || "<>:\"/|?*".contains(ch))
    {
        return false;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ');
    let stem = stem.to_ascii_uppercase();
    let numbered_device = stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        });
    !numbered_device
        && !matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        )
}

pub fn startup_command_value(executable: &Path, preferences: &Path) -> Result<StartupValue> {
    #[cfg(windows)]
    use windows_sys::Win32::System::Registry::REG_SZ;
    let executable = executable
        .canonicalize()
        .context("resolve startup executable path")?;
    let preferences = if preferences.exists() {
        preferences
            .canonicalize()
            .context("resolve startup preferences path")?
    } else {
        std::path::absolute(preferences).context("resolve startup preferences path")?
    };
    ensure!(
        executable.is_absolute() && preferences.is_absolute(),
        "startup paths must be absolute"
    );
    #[cfg(windows)]
    let exe = startup_shell_path(&executable)?;
    #[cfg(not(windows))]
    let exe = executable
        .to_str()
        .context("executable path is not valid Unicode")?;
    #[cfg(windows)]
    let pref = startup_shell_path(&preferences)?;
    #[cfg(not(windows))]
    let pref = preferences
        .to_str()
        .context("preferences path is not valid Unicode")?;
    ensure!(
        !exe.chars().any(|ch| ch.is_control() || ch == '"')
            && !pref.chars().any(|ch| ch.is_control() || ch == '"'),
        "startup paths contain unsupported quoting or control characters"
    );
    let command = format!("\"{exe}\" gui --preferences \"{pref}\"");
    ensure!(
        command.encode_utf16().count() <= 260,
        "Windows login startup supports at most 260 characters. Use a shorter executable or preferences path."
    );
    #[cfg(windows)]
    let mut data = command
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    data.extend_from_slice(&[0, 0]);
    #[cfg(windows)]
    let kind = REG_SZ;
    #[cfg(not(windows))]
    let kind = 1;
    Ok(StartupValue { kind, data })
}

pub fn reject_mcp_login_change(current: &Preferences, candidate: &Preferences) -> Result<()> {
    if current.launch_at_login != candidate.launch_at_login {
        bail!("launch-at-login is a local-only preference");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn startup_uses_shell_paths_without_verbatim_prefixes() {
        for (input, expected) in [
            (
                r"\\?\C:\Program Files\ProtoInputMultiPlex\app.exe",
                r"C:\Program Files\ProtoInputMultiPlex\app.exe",
            ),
            (
                r"\\?\UNC\server\share\app preferences.json",
                r"\\server\share\app preferences.json",
            ),
            (
                r"C:/data/app-preferences.json",
                r"C:\data\app-preferences.json",
            ),
        ] {
            assert_eq!(startup_shell_path(Path::new(input)).unwrap(), expected);
        }
        for input in [
            r"\\?\Volume{1234}\app.exe",
            r"\\.\C:\app.exe",
            r"\\?\C:\data.\app.exe",
            r"\\?\C:\data\..\app.exe",
            r"\\?\C:\data\NUL.exe",
            r"\\?\UNC\server\share\LPT².txt",
            r"C:\data\profile.json:stream",
            r"C:\data\NUL.json",
            r"C:\data\prefs.json ",
            r"C:\data\",
            r"C:\",
            r"C:relative.json",
            r"\\server\share",
        ] {
            assert!(startup_shell_path(Path::new(input)).is_err(), "{input}");
        }
    }

    #[test]
    fn defaults_keep_routing_and_login_opt_in_separate() {
        let prefs = Preferences::default();
        assert!(!prefs.launch_minimized);
        assert!(!prefs.minimize_to_tray);
        assert!(!prefs.close_to_tray);
        assert!(!prefs.launch_at_login);
        assert!(prefs.load_profile_at_startup);
        assert!(!prefs.save_profile_on_exit);
        assert_eq!(prefs.shortcuts[0].action, Action::Stop);
        assert_eq!(prefs.shortcuts[0].key, Key::F11);
        assert!(prefs.shortcuts[0].modifiers.control);
        prefs.validate().unwrap();
    }

    #[test]
    fn invalid_or_duplicate_shortcuts_are_rejected_before_store_mutation() {
        let mut prefs = Preferences::default();
        prefs.shortcuts.push(Shortcut {
            action: Action::PauseResume,
            modifiers: prefs.shortcuts[0].modifiers,
            key: Key::F11,
        });
        assert!(prefs.validate().is_err());

        prefs.shortcuts[1].key = Key::F10;
        prefs.shortcuts[1].action = Action::Stop;
        assert!(prefs.validate().is_err());

        prefs.shortcuts[1].action = Action::PauseResume;
        prefs.shortcuts[1].modifiers = Modifiers {
            shift: true,
            ..Default::default()
        };
        assert!(prefs.validate().is_err());
    }

    #[test]
    fn emergency_chord_is_reserved_for_stop() {
        let mut preferences = Preferences::default();
        preferences.shortcuts[0].action = Action::PauseResume;
        assert!(preferences.validate().is_err());

        preferences.shortcuts[0] = Shortcut {
            action: Action::Stop,
            modifiers: Modifiers {
                control: true,
                alt: true,
                shift: true,
                windows: false,
            },
            key: Key::F10,
        };
        preferences.validate().unwrap();
    }

    #[test]
    fn codec_is_bounded_strict_and_mcp_cannot_change_startup_registration() {
        let prefs = Preferences::default();
        let bytes = encode(&prefs).unwrap();
        assert_eq!(decode(&bytes).unwrap(), prefs);
        assert!(decode(&vec![b' '; MAX_PREFERENCES_BYTES + 1]).is_err());
        assert!(decode(br#"{"version":1,"unknown":true}"#).is_err());
        let mut candidate = prefs.clone();
        candidate.launch_at_login = true;
        assert!(reject_mcp_login_change(&prefs, &candidate).is_err());
    }

    #[test]
    fn old_preferences_fill_missing_fields_and_reject_invalid_values() {
        let old: Preferences = serde_json::from_str(r#"{"version":1}"#).unwrap();
        assert_eq!(old.appearance_mode, AppearanceMode::Dark);
        assert_eq!(old.zoom_percent, 100);
        assert!(old.show_app_launcher);
        assert!(old.load_profile_at_startup);
        assert!(!old.save_profile_on_exit);
        assert_eq!(decode(&encode(&old).unwrap()).unwrap(), old);

        for zoom in [50, 75, 100, 200] {
            let mut candidate = old.clone();
            candidate.zoom_percent = zoom;
            candidate.validate().unwrap();
        }
        for zoom in [49, 51, 76, 201] {
            let mut candidate = old.clone();
            candidate.zoom_percent = zoom;
            assert!(candidate.validate().is_err());
        }
    }

    #[test]
    fn profile_persistence_preferences_round_trip_independently() {
        let preferences = Preferences {
            load_profile_at_startup: false,
            save_profile_on_exit: true,
            ..Preferences::default()
        };

        let decoded = decode(&encode(&preferences).unwrap()).unwrap();

        assert!(!decoded.load_profile_at_startup);
        assert!(decoded.save_profile_on_exit);
        assert_eq!(decoded, preferences);
        assert_eq!(decoded.version, VERSION);
    }

    #[test]
    fn startup_test_key_is_limited_to_disposable_prefix() {
        assert!(validate_startup_test_key("Software\\ProtoInputMultiPlex\\Tests\\case-1").is_ok());
        assert!(validate_startup_test_key("Software\\ProtoInputMultiPlex\\Run").is_err());
        assert!(validate_startup_test_key("Software\\ProtoInputMultiPlex\\Tests\\").is_err());
    }
}
