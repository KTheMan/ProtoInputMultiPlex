use crate::app_preferences::{Action, Preferences, Shortcut};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub const WINDOW_CLASS_NAME: &str = "ProtoInputMultiPlex.AppLifecycle.v1";
pub const TRAY_ICON_ID: u32 = 0x5049;
pub const TRAY_CALLBACK_MESSAGE: u32 = 0x8000 + 0x51;
pub const TRAY_MENU_RESTORE: u32 = 0x6101;
pub const TRAY_MENU_PAUSE_RESUME: u32 = 0x6102;
pub const TRAY_MENU_STOP: u32 = 0x6103;
pub const TRAY_MENU_QUIT: u32 = 0x6104;
const INITIAL_HOTKEY_ID: u32 = 0x5100;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleAction {
    Restore,
    PauseResume,
    Stop,
    ToggleMode,
    ToggleRouting,
    Switched,
    Concurrent,
    ToggleKeyboard,
    EmergencyUnavailable,
    Quit,
}

fn shortcut_action(action: Action) -> LifecycleAction {
    match action {
        Action::Stop => LifecycleAction::Stop,
        Action::PauseResume => LifecycleAction::PauseResume,
        Action::ToggleMode => LifecycleAction::ToggleMode,
        Action::ToggleRouting => LifecycleAction::ToggleRouting,
        Action::Switched => LifecycleAction::Switched,
        Action::Concurrent => LifecycleAction::Concurrent,
        Action::ToggleKeyboard => LifecycleAction::ToggleKeyboard,
    }
}

pub trait HotkeyBackend {
    fn register(&mut self, id: u32, shortcut: Shortcut) -> Result<()>;
    fn unregister(&mut self, id: u32) -> Result<()>;
    fn poll_shared(&mut self) -> Result<Vec<u32>> {
        Ok(Vec::new())
    }
    fn retry_shared(&mut self) -> Result<()> {
        Ok(())
    }
    fn failed_shared_id(&self) -> Option<u32> {
        None
    }
}

struct PendingHotkeys {
    token: u64,
    candidate: BTreeMap<u32, Shortcut>,
    staged_ids: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ShortcutStatus {
    pub active: bool,
    pub error: Option<String>,
}

pub struct HotkeyRegistry<B: HotkeyBackend> {
    backend: B,
    active: BTreeMap<u32, Shortcut>,
    pending: Option<PendingHotkeys>,
    next_id: u32,
    // Includes staged or orphaned registrations until their removal succeeds.
    registered_ids: BTreeSet<u32>,
    retry_source: Vec<Shortcut>,
    status: Arc<Mutex<ShortcutStatus>>,
}
impl<B: HotkeyBackend> HotkeyRegistry<B> {
    #[cfg(test)]
    pub fn new(backend: B) -> Self {
        Self::with_status(
            backend,
            Arc::new(Mutex::new(ShortcutStatus {
                active: false,
                error: Some("app shortcuts have not been registered".into()),
            })),
        )
    }

    fn with_status(backend: B, status: Arc<Mutex<ShortcutStatus>>) -> Self {
        Self {
            backend,
            active: BTreeMap::new(),
            pending: None,
            next_id: INITIAL_HOTKEY_ID,
            registered_ids: BTreeSet::new(),
            retry_source: Vec::new(),
            status,
        }
    }

    fn set_status(&self, active: bool, error: Option<String>) {
        if let Ok(mut state) = self.status.lock() {
            *state = ShortcutStatus { active, error };
        }
    }

    fn remember_failure(&self, error: &anyhow::Error) {
        if let Ok(mut state) = self.status.lock()
            && !state.active
        {
            state.error = Some(format!("{error:#}"));
        }
    }

    fn unregister(&mut self, id: u32) -> Result<()> {
        self.backend.unregister(id)?;
        self.registered_ids.remove(&id);
        Ok(())
    }

    fn cleanup_ids(&mut self, ids: &[u32]) -> Result<()> {
        let mut first_error = None;
        for &id in ids {
            if let Err(error) = self.unregister(id) {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn deactivate(&mut self, error: &anyhow::Error) {
        self.active.clear();
        self.pending = None;
        self.set_status(false, Some(format!("{error:#}")));
        // Keep failed removals tracked for the next explicit recovery attempt.
        let ids = self.registered_ids.iter().copied().collect::<Vec<_>>();
        if let Err(cleanup) = self.cleanup_ids(&ids) {
            self.set_status(
                false,
                Some(format!("{error:#}; shortcut cleanup failed: {cleanup:#}")),
            );
        }
    }

    fn initialize(&mut self, shortcuts: &[Shortcut]) {
        self.retry_source = shortcuts.to_vec();
        let _ = self.retry();
    }

    fn retry(&mut self) -> Result<()> {
        ensure!(
            self.pending.is_none(),
            "cannot retry app shortcuts while preference changes are pending"
        );
        let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
        let shortcuts = self.retry_source.clone();
        let result = self
            .prepare(token, &shortcuts)
            .and_then(|()| self.commit(token));
        if let Err(error) = &result {
            self.remember_failure(error);
        }
        result
    }

    pub fn prepare(&mut self, token: u64, shortcuts: &[Shortcut]) -> Result<()> {
        ensure!(
            self.pending.is_none(),
            "a shortcut update is already pending"
        );
        // An inactive set has no dispatchable registrations. Remove leftovers before
        // acquiring a complete replacement so a previous cleanup failure is visible.
        if self.active.is_empty() && !self.registered_ids.is_empty() {
            let ids = self.registered_ids.iter().copied().collect::<Vec<_>>();
            if let Err(error) = self
                .cleanup_ids(&ids)
                .context("clean up inactive app shortcuts")
            {
                self.remember_failure(&error);
                return Err(error);
            }
        }
        let mut candidate = BTreeMap::new();
        let mut staged_ids = Vec::new();
        let result = (|| -> Result<()> {
            for shortcut in shortcuts {
                let id =
                    if let Some((&id, _)) = self.active.iter().find(|(_, old)| {
                        old.key == shortcut.key && old.modifiers == shortcut.modifiers
                    }) {
                        id
                    } else {
                        let id = self.next_id;
                        self.next_id = self
                            .next_id
                            .checked_add(1)
                            .context("app shortcut ID space exhausted")?;
                        self.backend
                            .register(id, *shortcut)
                            .context("register candidate app shortcut")?;
                        self.registered_ids.insert(id);
                        staged_ids.push(id);
                        id
                    };
                candidate.insert(id, *shortcut);
            }
            Ok(())
        })();
        if let Err(error) = result {
            if let Err(cleanup) = self.cleanup_ids(&staged_ids) {
                let error =
                    cleanup.context(format!("{error:#}; roll back candidate app shortcuts"));
                self.deactivate(&error);
                return Err(error);
            }
            self.remember_failure(&error);
            return Err(error);
        }
        self.pending = Some(PendingHotkeys {
            token,
            candidate,
            staged_ids,
        });
        Ok(())
    }

    pub fn commit(&mut self, token: u64) -> Result<()> {
        let pending = self
            .pending
            .take()
            .context("no shortcut update is pending")?;
        if pending.token != token {
            self.pending = Some(pending);
            bail!("shortcut update token does not match");
        }
        let removed = self
            .active
            .keys()
            .filter(|id| !pending.candidate.contains_key(id))
            .copied()
            .collect::<Vec<_>>();
        let mut unregistered = Vec::new();
        for id in removed {
            if let Err(error) = self.unregister(id) {
                let mut rollback_error = None;
                for restored in unregistered.iter().rev().copied() {
                    let shortcut = self.active[&restored];
                    if let Err(restore) = self.backend.register(restored, shortcut) {
                        rollback_error = Some(restore);
                    } else {
                        self.registered_ids.insert(restored);
                    }
                }
                if let Err(cleanup) = self.cleanup_ids(&pending.staged_ids) {
                    rollback_error.get_or_insert(cleanup);
                }
                if let Some(rollback) = rollback_error {
                    let error = rollback.context(format!(
                        "{error:#}; restore prior app shortcuts after commit failure"
                    ));
                    self.deactivate(&error);
                    return Err(error);
                }
                let error = error.context("unregister previous app shortcut");
                self.remember_failure(&error);
                return Err(error);
            }
            unregistered.push(id);
        }
        self.retry_source = pending.candidate.values().copied().collect();
        self.active = pending.candidate;
        self.set_status(true, None);
        Ok(())
    }

    pub fn rollback(&mut self, token: u64) -> Result<()> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        if pending.token != token {
            self.pending = Some(pending);
            bail!("shortcut update token does not match");
        }
        if let Err(error) = self
            .cleanup_ids(&pending.staged_ids)
            .context("roll back staged app shortcuts")
        {
            self.deactivate(&error);
            return Err(error);
        }
        Ok(())
    }

    fn settled(&self) -> bool {
        self.pending.is_none() && self.registered_ids == self.active.keys().copied().collect()
    }

    pub fn action(&self, id: u32) -> Option<LifecycleAction> {
        self.active
            .get(&id)
            .map(|shortcut| shortcut_action(shortcut.action))
    }

    pub fn accepted(&self) -> &BTreeMap<u32, Shortcut> {
        &self.active
    }
}

fn ensure_shortcuts_active(status: &Arc<Mutex<ShortcutStatus>>) -> Result<()> {
    let status = status
        .lock()
        .map_err(|_| anyhow::anyhow!("app shortcut health state is unavailable"))?;
    ensure!(
        status.active,
        "app shortcuts are unavailable: {}",
        status.error.as_deref().unwrap_or("retry app shortcuts")
    );
    Ok(())
}

fn accepted_uses_shared_stop(shortcuts: &BTreeMap<u32, Shortcut>) -> bool {
    shortcuts
        .values()
        .any(|shortcut| *shortcut == crate::app_preferences::EMERGENCY_STOP_SHORTCUT)
}

/// Marker for a native rejection whose prior registrations/tray were restored.
#[derive(Debug)]
struct KnownRejection(String);
impl std::fmt::Display for KnownRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for KnownRejection {}
fn classify_native(error: anyhow::Error, known: bool) -> anyhow::Error {
    if known {
        KnownRejection(format!("{error:#}")).into()
    } else {
        error
    }
}
#[derive(Debug)]
pub struct OperationFailure {
    pub error: anyhow::Error,
    pub uncertain: bool,
}
impl OperationFailure {
    fn unknown(error: impl Into<anyhow::Error>) -> Self {
        Self {
            error: error.into(),
            uncertain: true,
        }
    }
    fn known(error: impl Into<anyhow::Error>) -> Self {
        Self {
            error: error.into(),
            uncertain: false,
        }
    }
    fn native(error: anyhow::Error) -> Self {
        Self {
            uncertain: error.downcast_ref::<KnownRejection>().is_none(),
            error,
        }
    }
}

/// One linearization point shared by request revocation and native activation.
#[derive(Debug, Default)]
pub struct CommitDecision(AtomicU8);
impl CommitDecision {
    pub fn cancel(&self) -> bool {
        self.0
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    pub fn cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire) == 2
    }
    pub fn claim(&self) -> bool {
        self.0
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    pub fn claimed(&self) -> bool {
        matches!(self.0.load(Ordering::Acquire), 1 | 3)
    }
    fn complete(&self) {
        self.0.store(3, Ordering::Release);
    }
}
#[derive(Debug)]
pub struct CommitFailure {
    pub error: anyhow::Error,
    pub uncertain: bool,
    pub rollback_files: bool,
}

enum Command {
    Prepare {
        token: u64,
        candidate: Preferences,
        reply: SyncSender<Result<()>>,
    },
    Commit {
        token: u64,
        decision: Arc<CommitDecision>,
        reply: SyncSender<Result<()>>,
    },
    Rollback {
        token: u64,
        reply: Option<SyncSender<Result<()>>>,
    },
    EnsureEmergency(SyncSender<Result<()>>),
    RetryEmergency(SyncSender<Result<()>>),
    RetryShortcuts(SyncSender<Result<()>>),
    Shutdown(SyncSender<Result<()>>),
}

/// A command-only handle for fresh Start health checks off the GUI thread.
/// It neither owns registrations nor changes their policy.
#[derive(Clone, Debug)]
pub struct EmergencyCheck {
    commands: Sender<Command>,
    window: usize,
}
impl EmergencyCheck {
    pub fn ensure(&self) -> Result<()> {
        let (reply, response) = mpsc::sync_channel(1);
        self.commands.send(Command::EnsureEmergency(reply))?;
        #[cfg(windows)]
        unsafe {
            let _ = windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
                self.window as windows_sys::Win32::Foundation::HWND,
                windows_sys::Win32::UI::WindowsAndMessaging::WM_CANCELMODE,
                0,
                0,
            );
        }
        response
            .recv_timeout(COMMAND_TIMEOUT)
            .context("app lifecycle shortcut command timed out")?
    }
}

pub struct Service {
    commands: Sender<Command>,
    actions: Receiver<LifecycleAction>,
    thread: Option<JoinHandle<()>>,
    window: usize,
    emergency_error: Arc<Mutex<Option<String>>>,
    shortcut_status: Arc<Mutex<ShortcutStatus>>,
    shutdown: bool,
}
impl Service {
    pub fn start(preferences: &Preferences) -> Result<Self> {
        preferences.validate()?;
        let (commands, command_rx) = mpsc::channel();
        let (action_tx, actions) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let emergency_error = Arc::new(Mutex::new(None));
        let shortcut_status = Arc::new(Mutex::new(ShortcutStatus {
            active: false,
            error: Some("app shortcuts have not been registered".into()),
        }));
        let thread_shortcut_status = shortcut_status.clone();
        let initial = preferences.clone();
        let thread_emergency_error = emergency_error.clone();
        let thread = thread::Builder::new()
            .name("protoinputmultiplex-app-lifecycle".into())
            .spawn(move || {
                service_thread(
                    initial,
                    command_rx,
                    action_tx,
                    ready_tx,
                    thread_emergency_error,
                    thread_shortcut_status,
                )
            })?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(window)) => Ok(Self {
                commands,
                actions,
                thread: Some(thread),
                window,
                emergency_error,
                shortcut_status,
                shutdown: false,
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(error) => {
                let _ = commands.send(Command::Shutdown(mpsc::sync_channel(1).0));
                let _ = thread.join();
                Err(error).context("app lifecycle startup timed out")
            }
        }
    }

    pub fn take_actions(&mut self) -> Receiver<LifecycleAction> {
        std::mem::replace(&mut self.actions, mpsc::channel().1)
    }
    pub fn handle(&self) -> Handle {
        Handle {
            commands: self.commands.clone(),
            window: self.window,
            emergency_error: self.emergency_error.clone(),
            shortcut_status: self.shortcut_status.clone(),
        }
    }
    pub fn shutdown(&mut self) -> Result<()> {
        if self.shutdown {
            return Ok(());
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.commands.send(Command::Shutdown(reply))?;
        self.handle().wake_service();
        let result = response
            .recv_timeout(COMMAND_TIMEOUT)
            .context("app lifecycle shutdown timed out")?;
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("app lifecycle thread panicked"))?;
        }
        self.shutdown = true;
        result
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.shutdown();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Clone)]
pub struct Handle {
    commands: Sender<Command>,
    window: usize,
    emergency_error: Arc<Mutex<Option<String>>>,
    shortcut_status: Arc<Mutex<ShortcutStatus>>,
}
impl Handle {
    pub fn emergency_error(&self) -> Option<String> {
        self.emergency_error
            .lock()
            .ok()
            .and_then(|error| error.clone())
    }

    pub fn shortcut_status(&self) -> ShortcutStatus {
        self.shortcut_status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_else(|_| ShortcutStatus {
                active: false,
                error: Some("app shortcut health state is unavailable".into()),
            })
    }

    pub fn retry_shortcuts(&self) -> std::result::Result<(), OperationFailure> {
        self.emergency_command(Command::RetryShortcuts)
    }

    pub fn emergency_check(&self) -> EmergencyCheck {
        EmergencyCheck {
            commands: self.commands.clone(),
            window: self.window,
        }
    }

    pub fn retry_emergency(&self) -> std::result::Result<(), OperationFailure> {
        self.emergency_command(Command::RetryEmergency)
    }

    fn emergency_command(
        &self,
        command: impl FnOnce(SyncSender<Result<()>>) -> Command,
    ) -> std::result::Result<(), OperationFailure> {
        let (reply, response) = mpsc::sync_channel(1);
        self.commands
            .send(command(reply))
            .map_err(OperationFailure::unknown)?;
        self.wake_service();
        response
            .recv_timeout(COMMAND_TIMEOUT)
            .map_err(|error| {
                OperationFailure::unknown(anyhow::anyhow!(
                    "app lifecycle shortcut command timed out: {error}"
                ))
            })?
            .map_err(OperationFailure::native)
    }

    pub fn prepare_preferences(
        &self,
        preferences: &Preferences,
    ) -> std::result::Result<Prepared, OperationFailure> {
        preferences.validate().map_err(OperationFailure::known)?;
        let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
        let (reply, response) = mpsc::sync_channel(1);
        self.commands
            .send(Command::Prepare {
                token,
                candidate: preferences.clone(),
                reply,
            })
            .map_err(OperationFailure::unknown)?;
        self.wake_service();
        match response.recv_timeout(COMMAND_TIMEOUT) {
            Ok(result) => result.map_err(OperationFailure::native)?,
            Err(error) => {
                let (reply, _) = mpsc::sync_channel(1);
                let _ = self.commands.send(Command::Rollback {
                    token,
                    reply: Some(reply),
                });
                self.wake_service();
                return Err(OperationFailure::unknown(anyhow::anyhow!(
                    "app lifecycle did not prepare preferences: {error}"
                )));
            }
        }
        Ok(Prepared {
            commands: self.commands.clone(),
            token,
            window: self.window,
            finished: false,
        })
    }

    fn wake_service(&self) {
        #[cfg(windows)]
        if self.window != 0 {
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
                    self.window as windows_sys::Win32::Foundation::HWND,
                    windows_sys::Win32::UI::WindowsAndMessaging::WM_CANCELMODE,
                    0,
                    0,
                );
            }
        }
    }
}

pub struct Prepared {
    commands: Sender<Command>,
    token: u64,
    window: usize,
    finished: bool,
}
impl Prepared {
    pub fn commit(
        mut self,
        decision: Arc<CommitDecision>,
    ) -> std::result::Result<(), CommitFailure> {
        let (reply, response) = mpsc::sync_channel(1);
        let mut acknowledged = false;
        let result = (|| -> Result<()> {
            self.commands.send(Command::Commit {
                token: self.token,
                decision: decision.clone(),
                reply,
            })?;
            #[cfg(windows)]
            if self.window != 0 {
                unsafe {
                    let _ = windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
                        self.window as windows_sys::Win32::Foundation::HWND,
                        windows_sys::Win32::UI::WindowsAndMessaging::WM_CANCELMODE,
                        0,
                        0,
                    );
                }
            }
            match response.recv_timeout(COMMAND_TIMEOUT) {
                Ok(result) => {
                    acknowledged = true;
                    result
                }
                Err(error) => {
                    if decision.cancel() || decision.cancelled() {
                        Err(error).context("app lifecycle commit was cancelled before activation")
                    } else {
                        // The SAME decision was already claimed. Only the owner
                        // waits: a requester timeout cannot reverse activation.
                        let result = response
                            .recv()
                            .context("app lifecycle commit acknowledgement was lost")?;
                        acknowledged = true;
                        result
                    }
                }
            }
        })();
        match result {
            Ok(()) => {
                self.finished = true;
                Ok(())
            }
            Err(error) => {
                // Never roll back disk merely because a claimed ACK was lost.
                // A definitive native rejection still gets explicit staged cleanup.
                let claimed = decision.claimed();
                let rollback = if claimed {
                    Ok(())
                } else {
                    self.send_rollback(Some(true))
                };
                self.finished = true;
                Err(CommitFailure {
                    uncertain: error.downcast_ref::<KnownRejection>().is_none() && claimed
                        || rollback.is_err(),
                    rollback_files: !claimed || acknowledged,
                    error: match rollback {
                        Ok(()) => error,
                        Err(rollback) => {
                            error.context(format!("shortcut rollback failed: {rollback:#}"))
                        }
                    },
                })
            }
        }
    }

    pub fn rollback(mut self) -> Result<()> {
        let result = self.send_rollback(Some(true));
        self.finished = true;
        result
    }

    fn send_rollback(&self, wait: Option<bool>) -> Result<()> {
        let (reply, response) = if wait == Some(true) {
            let (reply, response) = mpsc::sync_channel(1);
            (Some(reply), Some(response))
        } else {
            (None, None)
        };
        self.commands.send(Command::Rollback {
            token: self.token,
            reply,
        })?;
        #[cfg(windows)]
        if self.window != 0 {
            unsafe {
                let _ = windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
                    self.window as windows_sys::Win32::Foundation::HWND,
                    windows_sys::Win32::UI::WindowsAndMessaging::WM_CANCELMODE,
                    0,
                    0,
                );
            }
        }
        if let Some(response) = response {
            response
                .recv_timeout(COMMAND_TIMEOUT)
                .context("app lifecycle did not roll back preferences")??;
        }
        Ok(())
    }
}
impl Drop for Prepared {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.send_rollback(None);
            self.finished = true;
        }
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{
        ffi::c_void,
        ptr::{null, null_mut},
        sync::mpsc::Receiver,
    };
    use windows_sys::Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey},
            Shell::{
                NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
                Shell_NotifyIconW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
                DestroyWindow, DispatchMessageW, EndMenu, GWLP_USERDATA, GetMessagePos,
                GetWindowLongPtrW, IDI_APPLICATION, LoadIconW, MF_STRING, MSG, PM_REMOVE,
                PeekMessageW, PostMessageW, RegisterClassW, SetForegroundWindow, SetWindowLongPtrW,
                TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, UnregisterClassW,
                WM_CANCELMODE, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_HOTKEY, WM_LBUTTONUP,
                WM_NCCREATE, WM_NULL, WM_RBUTTONUP, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
            },
        },
    };

    enum NativeEvent {
        Menu(u32),
    }
    struct WindowState {
        native_events: Sender<NativeEvent>,
        actions: Sender<LifecycleAction>,
    }
    type NativeWindow = (HWND, Vec<u16>, HINSTANCE, Box<WindowState>);
    struct WinHotkeyBackend {
        hwnd: HWND,
        subscriptions: BTreeMap<u32, crate::emergency_stop::Subscription>,
        shared_id: Option<u32>,
        failed_shared_id: Option<u32>,
        shared_failed: bool,
    }
    impl HotkeyBackend for WinHotkeyBackend {
        fn register(&mut self, id: u32, shortcut: Shortcut) -> Result<()> {
            if shortcut == crate::app_preferences::EMERGENCY_STOP_SHORTCUT {
                let subscription = crate::emergency_stop::Subscription::connect()
                    .context("subscribe to shared emergency Stop")?;
                self.subscriptions.insert(id, subscription);
                self.shared_id = Some(id);
                return Ok(());
            }
            let modifiers = shortcut.modifiers;
            let mut flags = 0u32;
            if modifiers.control {
                flags |= 0x0002;
            }
            if modifiers.alt {
                flags |= 0x0001;
            }
            if modifiers.shift {
                flags |= 0x0004;
            }
            if modifiers.windows {
                flags |= 0x0008;
            }
            flags |= 0x4000; // MOD_NOREPEAT
            let registered =
                unsafe { RegisterHotKey(self.hwnd, id as i32, flags, shortcut.key.virtual_key()) };
            ensure!(
                registered != 0,
                "register global shortcut: {}",
                std::io::Error::last_os_error()
            );
            Ok(())
        }
        fn unregister(&mut self, id: u32) -> Result<()> {
            if self.subscriptions.remove(&id).is_some() {
                if self.subscriptions.is_empty() {
                    self.shared_id = None;
                    self.shared_failed = false;
                    self.failed_shared_id = None;
                }
                return Ok(());
            }
            let removed = unsafe { UnregisterHotKey(self.hwnd, id as i32) };
            ensure!(
                removed != 0,
                "unregister global shortcut: {}",
                std::io::Error::last_os_error()
            );
            Ok(())
        }

        fn poll_shared(&mut self) -> Result<Vec<u32>> {
            if self.shared_failed {
                return Ok(Vec::new());
            }
            let mut ready = Vec::new();
            for (&id, subscription) in &mut self.subscriptions {
                match subscription.poll() {
                    Ok(true) => ready.push(id),
                    Ok(false) => {}
                    Err(error) => {
                        self.shared_failed = true;
                        self.failed_shared_id = Some(id);
                        return Err(error).context("poll shared emergency Stop");
                    }
                }
            }
            Ok(ready)
        }

        fn retry_shared(&mut self) -> Result<()> {
            let Some(id) = self.shared_id else {
                bail!("the shared Ctrl+Alt+Shift+F11 Stop shortcut is not enabled");
            };
            self.subscriptions.clear();
            self.shared_failed = true;
            self.failed_shared_id = Some(id);
            let subscription = crate::emergency_stop::Subscription::connect()
                .context("retry shared emergency Stop subscription")?;
            self.subscriptions.insert(id, subscription);
            self.shared_failed = false;
            self.failed_shared_id = None;
            Ok(())
        }

        fn failed_shared_id(&self) -> Option<u32> {
            self.failed_shared_id
        }
    }

    struct ServiceHealth {
        emergency_error: Arc<Mutex<Option<String>>>,
        shortcut_status: Arc<Mutex<ShortcutStatus>>,
    }

    struct Pending {
        token: u64,
        candidate: Preferences,
        tray_added: bool,
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }

    fn create_window(
        native_events: Sender<NativeEvent>,
        actions: Sender<LifecycleAction>,
    ) -> Result<NativeWindow> {
        let class = wide(WINDOW_CLASS_NAME);
        let instance = unsafe { GetModuleHandleW(null()) };
        ensure!(!instance.is_null(), "get app lifecycle module handle");
        let mut state = Box::new(WindowState {
            native_events,
            actions,
        });
        let registration = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: null_mut(),
            hCursor: null_mut(),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: class.as_ptr(),
        };
        let registered = unsafe { RegisterClassW(&registration) };
        ensure!(registered != 0, "register app lifecycle window class");
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class.as_ptr(),
                class.as_ptr(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                instance,
                (&mut *state as *mut WindowState).cast::<c_void>(),
            )
        };
        if hwnd.is_null() {
            unsafe { UnregisterClassW(class.as_ptr(), instance) };
            bail!("create app lifecycle message window");
        }
        Ok((hwnd, class, instance, state))
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if message == WM_NCCREATE {
            // SAFETY: WM_NCCREATE carries the pointer supplied to CreateWindowExW.
            let create = unsafe {
                &*(lparam as *const windows_sys::Win32::UI::WindowsAndMessaging::CREATESTRUCTW)
            };
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as _) };
            return 1;
        }
        let state = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState };
        if state.is_null() {
            return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
        }
        let state = unsafe { &*state };
        match message {
            TRAY_CALLBACK_MESSAGE
                if lparam as u32 == WM_RBUTTONUP || lparam as u32 == WM_CONTEXTMENU =>
            {
                show_tray_menu(hwnd, unsafe { GetMessagePos() }, state);
                0
            }
            TRAY_CALLBACK_MESSAGE if lparam as u32 == WM_LBUTTONUP => {
                let _ = state.actions.send(LifecycleAction::Restore);
                0
            }
            WM_COMMAND => {
                let _ = state
                    .native_events
                    .send(NativeEvent::Menu((wparam & 0xFFFF) as u32));
                0
            }
            WM_CANCELMODE => {
                unsafe { EndMenu() };
                0
            }
            WM_DESTROY => 0,
            _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        }
    }

    fn show_tray_menu(hwnd: HWND, message_pos: u32, state: &WindowState) {
        unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return;
            }
            let entries = [
                (TRAY_MENU_RESTORE, "Open ProtoInputMultiPlex"),
                (TRAY_MENU_PAUSE_RESUME, "Pause / Resume"),
                (TRAY_MENU_STOP, "Stop routing"),
                (TRAY_MENU_QUIT, "Quit"),
            ];
            for (id, label) in entries {
                let text = wide(label);
                AppendMenuW(menu, MF_STRING, id as usize, text.as_ptr());
            }
            let x = (message_pos & 0xFFFF) as i16 as i32;
            let y = ((message_pos >> 16) & 0xFFFF) as i16 as i32;
            SetForegroundWindow(hwnd);
            let selected =
                TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, x, y, 0, hwnd, null());
            if selected != 0 {
                let _ = state.native_events.send(NativeEvent::Menu(selected as u32));
            }
            PostMessageW(hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu);
        }
    }

    fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
        let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = TRAY_ICON_ID;
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.uCallbackMessage = TRAY_CALLBACK_MESSAGE;
        data.hIcon = unsafe { LoadIconW(null_mut(), IDI_APPLICATION) };
        let tip = wide(crate::APP_NAME);
        // NOTIFYICONDATAW is packed on x86; never borrow its wide-array field.
        let mut tray_tip = [0u16; 128];
        let copy = tip.len().min(tray_tip.len());
        tray_tip[..copy].copy_from_slice(&tip[..copy]);
        data.szTip = tray_tip;
        data
    }

    fn tray_add(hwnd: HWND) -> Result<()> {
        let data = tray_data(hwnd);
        let added = unsafe { Shell_NotifyIconW(NIM_ADD, &data) };
        ensure!(added != 0, "add ProtoInputMultiPlex tray icon");
        Ok(())
    }

    fn tray_delete(hwnd: HWND) -> Result<()> {
        let data = tray_data(hwnd);
        let deleted = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
        ensure!(deleted != 0, "remove ProtoInputMultiPlex tray icon");
        Ok(())
    }

    fn needs_tray(preferences: &Preferences) -> bool {
        preferences.launch_minimized || preferences.minimize_to_tray || preferences.close_to_tray
    }

    fn poll_emergency(
        hotkeys: &mut HotkeyRegistry<WinHotkeyBackend>,
        actions: &Sender<LifecycleAction>,
        error_state: &Arc<Mutex<Option<String>>>,
    ) -> Result<()> {
        match hotkeys.backend.poll_shared() {
            Ok(ids) => {
                for id in ids {
                    if let Some(action) = hotkeys.action(id) {
                        let _ = actions.send(action);
                    }
                }
                Ok(())
            }
            Err(error) => {
                let accepted = hotkeys
                    .backend
                    .failed_shared_id()
                    .and_then(|id| hotkeys.action(id))
                    .is_some();
                if accepted {
                    let message = format!("{error:#}");
                    let mut state = error_state.lock().map_err(|_| {
                        anyhow::anyhow!("emergency-stop health state is unavailable")
                    })?;
                    if state.is_none() {
                        *state = Some(message);
                        let _ = actions.send(LifecycleAction::EmergencyUnavailable);
                    }
                }
                Err(error)
            }
        }
    }

    fn run_loop(
        window: NativeWindow,
        initial: Preferences,
        commands: Receiver<Command>,
        native_events: Receiver<NativeEvent>,
        actions: Sender<LifecycleAction>,
        ready: SyncSender<Result<usize>>,
        health: ServiceHealth,
    ) -> Result<()> {
        let ServiceHealth {
            emergency_error,
            shortcut_status,
        } = health;
        let (hwnd, class, instance, window_state) = window;
        let backend = WinHotkeyBackend {
            hwnd,
            subscriptions: BTreeMap::new(),
            shared_id: None,
            failed_shared_id: None,
            shared_failed: false,
        };
        let mut hotkeys = HotkeyRegistry::with_status(backend, shortcut_status.clone());
        hotkeys.initialize(&initial.shortcuts);
        let mut tray_installed = false;
        if needs_tray(&initial) {
            if let Err(error) = tray_add(hwnd) {
                let ids = hotkeys.registered_ids.iter().copied().collect::<Vec<_>>();
                let _ = hotkeys.cleanup_ids(&ids);
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    DestroyWindow(hwnd);
                    UnregisterClassW(class.as_ptr(), instance);
                }
                drop(window_state);
                return Err(error);
            }
            tray_installed = true;
        }
        let _ = ready.send(Ok(hwnd as usize));
        let mut pending: Option<Pending> = None;
        let mut stopping = false;
        let mut shutdown_reply = None;
        while !stopping {
            unsafe {
                let mut message: MSG = std::mem::zeroed();
                while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
                    if message.message == WM_HOTKEY {
                        if let Some(action) = hotkeys.action(message.wParam as u32) {
                            let _ = actions.send(action);
                        }
                    } else {
                        TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }
            }
            if poll_emergency(&mut hotkeys, &actions, &emergency_error).is_err()
                && let Some(failed_id) = hotkeys.backend.failed_shared_id()
                && hotkeys.action(failed_id).is_none()
                && let Some(stage) = pending.take()
            {
                let _ = hotkeys.rollback(stage.token);
                if stage.tray_added {
                    let _ = tray_delete(hwnd);
                }
            }
            for native in native_events.try_iter() {
                match native {
                    NativeEvent::Menu(TRAY_MENU_RESTORE) => {
                        let _ = actions.send(LifecycleAction::Restore);
                    }
                    NativeEvent::Menu(TRAY_MENU_PAUSE_RESUME) => {
                        let _ = actions.send(LifecycleAction::PauseResume);
                    }
                    NativeEvent::Menu(TRAY_MENU_STOP) => {
                        let _ = actions.send(LifecycleAction::Stop);
                    }
                    NativeEvent::Menu(TRAY_MENU_QUIT) => {
                        let _ = actions.send(LifecycleAction::Quit);
                    }
                    NativeEvent::Menu(_) => {}
                }
            }
            match commands.recv_timeout(Duration::from_millis(15)) {
                Ok(Command::Prepare {
                    token,
                    candidate,
                    reply,
                }) => {
                    if pending.is_some() {
                        let _ = reply.send(Err(anyhow::anyhow!(
                            "another app preference update is pending"
                        )));
                        continue;
                    }
                    if let Err(error) = candidate.validate() {
                        let _ = reply.send(Err(classify_native(error, true)));
                        continue;
                    }
                    let previous = hotkeys.accepted().clone();
                    if let Err(error) = hotkeys.prepare(token, &candidate.shortcuts) {
                        let known = hotkeys.settled() && *hotkeys.accepted() == previous;
                        let _ = reply.send(Err(classify_native(error, known)));
                        continue;
                    }
                    let tray_added = needs_tray(&candidate) && !tray_installed;
                    if tray_added && let Err(error) = tray_add(hwnd) {
                        let rollback = hotkeys.rollback(token);
                        let known = rollback.is_ok()
                            && hotkeys.settled()
                            && *hotkeys.accepted() == previous;
                        let error = match rollback {
                            Ok(()) => error,
                            Err(rollback) => {
                                error.context(format!("shortcut rollback failed: {rollback:#}"))
                            }
                        };
                        let _ = reply.send(Err(classify_native(error, known)));
                        continue;
                    }
                    pending = Some(Pending {
                        token,
                        candidate,
                        tray_added,
                    });
                    if reply.send(Ok(())).is_err()
                        && let Some(stage) = pending.take()
                    {
                        let _ = hotkeys.rollback(stage.token);
                        if stage.tray_added {
                            let _ = tray_delete(hwnd);
                        }
                    }
                }
                Ok(Command::Commit {
                    token,
                    decision,
                    reply,
                }) => {
                    if !decision.claim() {
                        let _ =
                            reply.send(Err(anyhow::anyhow!("app preference commit was cancelled")));
                        continue;
                    }
                    let previous = hotkeys.accepted().clone();
                    let previous_tray = tray_installed;
                    let result = (|| -> Result<()> {
                        let Some(stage) = pending.take() else {
                            bail!("no app preference update is pending");
                        };
                        if stage.token != token {
                            pending = Some(stage);
                            bail!("app preference token mismatch");
                        }
                        let disabling_tray = !needs_tray(&stage.candidate) && tray_installed;
                        if disabling_tray {
                            if let Err(error) = tray_delete(hwnd) {
                                hotkeys.rollback(token)?;
                                return Err(error);
                            }
                            tray_installed = false;
                        }
                        if let Err(error) = hotkeys.commit(token) {
                            if disabling_tray {
                                tray_add(hwnd)
                                    .context("restore tray icon after shortcut commit failure")?;
                                tray_installed = true;
                            }
                            if stage.tray_added {
                                let _ = tray_delete(hwnd);
                            }
                            return Err(error);
                        }
                        if stage.tray_added {
                            tray_installed = true;
                        }
                        if disabling_tray {
                            // Removing the restore entry must not strand a hidden GUI.
                            let _ = actions.send(LifecycleAction::Restore);
                        }
                        Ok(())
                    })();
                    if result.is_ok()
                        && !accepted_uses_shared_stop(hotkeys.accepted())
                        && let Ok(mut state) = emergency_error.lock()
                    {
                        *state = None;
                    }
                    let known = hotkeys.settled()
                        && *hotkeys.accepted() == previous
                        && tray_installed == previous_tray;
                    decision.complete();
                    let _ = reply.send(result.map_err(|error| classify_native(error, known)));
                }
                Ok(Command::Rollback { token, reply }) => {
                    let Some(stage) = pending.take() else {
                        if let Some(reply) = reply {
                            let _ = reply.send(Ok(()));
                        }
                        continue;
                    };
                    if stage.token != token {
                        pending = Some(stage);
                        if let Some(reply) = reply {
                            let _ =
                                reply.send(Err(anyhow::anyhow!("app preference token mismatch")));
                        }
                        continue;
                    }
                    let mut result = hotkeys.rollback(token);
                    if stage.tray_added
                        && let Err(error) = tray_delete(hwnd)
                        && result.is_ok()
                    {
                        result = Err(error);
                    }
                    if let Some(reply) = reply {
                        let _ = reply.send(result);
                    }
                }
                Ok(Command::EnsureEmergency(reply)) => {
                    if let Err(error) = ensure_shortcuts_active(&shortcut_status) {
                        let _ = reply.send(Err(error));
                        continue;
                    }
                    let result = poll_emergency(&mut hotkeys, &actions, &emergency_error);
                    if result.is_err()
                        && let Some(stage) = pending.take()
                    {
                        let _ = hotkeys.rollback(stage.token);
                        if stage.tray_added {
                            let _ = tray_delete(hwnd);
                        }
                    }
                    let result = result.and_then(|()| {
                        if let Some(error) = emergency_error
                            .lock()
                            .map_err(|_| {
                                anyhow::anyhow!("emergency-stop health state is unavailable")
                            })?
                            .clone()
                        {
                            bail!("shared emergency Stop is unavailable: {error}");
                        }
                        Ok(())
                    });
                    let _ = reply.send(result);
                }
                Ok(Command::RetryShortcuts(reply)) => {
                    if pending.is_some() {
                        let _ = reply.send(Err(anyhow::anyhow!(
                            "cannot retry app shortcuts while preference changes are pending"
                        )));
                        continue;
                    }
                    let result = hotkeys.retry();
                    if result.is_ok()
                        && !accepted_uses_shared_stop(hotkeys.accepted())
                        && let Ok(mut state) = emergency_error.lock()
                    {
                        *state = None;
                    }
                    let known = hotkeys.settled();
                    let _ = reply.send(result.map_err(|error| classify_native(error, known)));
                }
                Ok(Command::RetryEmergency(reply)) => {
                    if pending.is_some() {
                        let _ = reply.send(Err(anyhow::anyhow!(
                            "cannot retry emergency Stop while preference changes are pending"
                        )));
                        continue;
                    }
                    let result = hotkeys.backend.retry_shared();
                    if result.is_ok() {
                        if let Ok(mut state) = emergency_error.lock() {
                            *state = None;
                        }
                    } else if let Err(error) = &result
                        && let Ok(mut state) = emergency_error.lock()
                        && state.is_none()
                    {
                        *state = Some(format!("{error:#}"));
                        let _ = actions.send(LifecycleAction::EmergencyUnavailable);
                    }
                    let known = hotkeys.settled();
                    let _ = reply.send(result.map_err(|error| classify_native(error, known)));
                }
                Ok(Command::Shutdown(reply)) => {
                    shutdown_reply = Some(reply);
                    stopping = true;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => stopping = true,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        if let Some(stage) = pending.take() {
            let _ = hotkeys.rollback(stage.token);
            if stage.tray_added {
                let _ = tray_delete(hwnd);
            }
        }
        let mut cleanup = Ok(());
        if tray_installed {
            cleanup = tray_delete(hwnd);
        }
        let active_ids = hotkeys.registered_ids.iter().copied().collect::<Vec<_>>();
        for id in active_ids {
            if let Err(error) = hotkeys.backend.unregister(id)
                && cleanup.is_ok()
            {
                cleanup = Err(error);
            }
        }
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DestroyWindow(hwnd);
            UnregisterClassW(class.as_ptr(), instance);
        }
        // Keep the callback context alive until the window is fully destroyed.
        drop(window_state);
        if let Some(reply) = shutdown_reply {
            let _ = reply.send(cleanup);
            return Ok(());
        }
        cleanup
    }

    pub(super) fn service_thread(
        initial: Preferences,
        commands: Receiver<Command>,
        actions: Sender<LifecycleAction>,
        ready: SyncSender<Result<usize>>,
        emergency_error: Arc<Mutex<Option<String>>>,
        shortcut_status: Arc<Mutex<ShortcutStatus>>,
    ) {
        let (native_tx, native_rx) = mpsc::channel();
        let result = create_window(native_tx, actions.clone()).and_then(|window| {
            run_loop(
                window,
                initial,
                commands,
                native_rx,
                actions,
                ready.clone(),
                ServiceHealth {
                    emergency_error,
                    shortcut_status,
                },
            )
        });
        if let Err(error) = result {
            let _ = ready.try_send(Err(anyhow::anyhow!("{error:#}")));
        }
    }
}

#[cfg(windows)]
fn service_thread(
    initial: Preferences,
    commands: Receiver<Command>,
    actions: Sender<LifecycleAction>,
    ready: SyncSender<Result<usize>>,
    emergency_error: Arc<Mutex<Option<String>>>,
    shortcut_status: Arc<Mutex<ShortcutStatus>>,
) {
    windows::service_thread(
        initial,
        commands,
        actions,
        ready,
        emergency_error,
        shortcut_status,
    )
}

#[cfg(not(windows))]
fn service_thread(
    _initial: Preferences,
    _commands: Receiver<Command>,
    _actions: Sender<LifecycleAction>,
    ready: SyncSender<Result<usize>>,
    _emergency_error: Arc<Mutex<Option<String>>>,
    _shortcut_status: Arc<Mutex<ShortcutStatus>>,
) {
    let _ = ready.send(Err(anyhow::anyhow!(
        "app lifecycle is only available on Windows"
    )));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_preferences::{Key, Modifiers};
    use std::collections::BTreeSet;

    #[test]
    fn preferences_transaction_commit_uses_original_shared_cancellation_and_retains_claimed_ack() {
        for cancel_first in [true, false] {
            let (commands, incoming) = mpsc::channel();
            let prepared = Prepared {
                commands,
                token: 91,
                window: 0,
                finished: false,
            };
            let decision = Arc::new(CommitDecision::default());
            if cancel_first {
                assert!(decision.cancel());
            }
            let expected = decision.clone();
            let (claimed_tx, claimed_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let native = thread::spawn(move || {
                let Command::Commit {
                    decision, reply, ..
                } = incoming.recv().unwrap()
                else {
                    panic!("expected commit")
                };
                assert!(Arc::ptr_eq(&decision, &expected));
                let claimed = decision.claim();
                claimed_tx.send(claimed).unwrap();
                release_rx.recv().unwrap();
                if claimed {
                    decision.complete();
                    reply.send(Ok(())).unwrap();
                } else {
                    reply
                        .send(Err(classify_native(anyhow::anyhow!("cancelled"), true)))
                        .unwrap();
                    let Command::Rollback {
                        reply: Some(reply), ..
                    } = incoming.recv().unwrap()
                    else {
                        panic!("expected rollback")
                    };
                    reply.send(Ok(())).unwrap();
                }
            });
            let cancel = decision.clone();
            let worker = thread::spawn(move || prepared.commit(decision));
            let claimed = claimed_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            assert_eq!(claimed, !cancel_first);
            // A post-claim disconnect/revoke cannot win over the same native CAS.
            if claimed {
                assert!(!cancel.cancel());
            }
            release_tx.send(()).unwrap();
            let outcome = worker.join().unwrap();
            if cancel_first {
                let failure = outcome.unwrap_err();
                assert!(!failure.uncertain);
                assert!(failure.rollback_files);
            } else {
                assert!(outcome.is_ok());
            }
            native.join().unwrap();
        }
    }
    #[test]
    fn preferences_transaction_lost_claimed_ack_is_uncertain_and_does_not_roll_back_file() {
        let (commands, incoming) = mpsc::channel();
        let prepared = Prepared {
            commands,
            token: 92,
            window: 0,
            finished: false,
        };
        let decision = Arc::new(CommitDecision::default());
        let observed = decision.clone();
        let native = thread::spawn(move || {
            let Command::Commit {
                decision, reply, ..
            } = incoming.recv().unwrap()
            else {
                panic!("expected commit")
            };
            assert!(decision.claim());
            assert!(!decision.cancel());
            drop(reply);
            assert!(
                incoming.recv_timeout(Duration::from_millis(100)).is_err(),
                "claimed ACK loss attempted rollback"
            );
        });
        let error = prepared.commit(decision).unwrap_err();
        assert!(error.uncertain);
        assert!(!error.rollback_files);
        assert!(observed.claimed());
        native.join().unwrap();
    }
    #[test]
    fn preferences_transaction_preparation_distinguishes_definitive_conflict_from_lost_ack() {
        for known in [true, false] {
            let (commands, incoming) = mpsc::channel();
            let handle = Handle {
                commands,
                window: 0,
                emergency_error: Arc::new(Mutex::new(None)),
                shortcut_status: Arc::new(Mutex::new(ShortcutStatus {
                    active: true,
                    error: None,
                })),
            };
            let native = thread::spawn(move || {
                let Command::Prepare { reply, .. } = incoming.recv().unwrap() else {
                    panic!("expected prepare")
                };
                if known {
                    reply
                        .send(Err(classify_native(
                            anyhow::anyhow!("occupied shortcut"),
                            true,
                        )))
                        .unwrap();
                } else {
                    drop(reply);
                }
            });
            let error = handle
                .prepare_preferences(&Preferences::default())
                .err()
                .expect("expected failure");
            assert_eq!(error.uncertain, !known);
            native.join().unwrap();
        }
    }

    #[derive(Default)]
    struct FakeHotkeys {
        occupied: BTreeSet<(Modifiers, Key)>,
        registered: BTreeMap<u32, Shortcut>,
        fail_register_id: Option<u32>,
        fail_unregister_once: BTreeSet<u32>,
    }
    impl HotkeyBackend for FakeHotkeys {
        fn register(&mut self, id: u32, shortcut: Shortcut) -> Result<()> {
            ensure!(
                self.fail_register_id != Some(id),
                "injected registration failure"
            );
            let chord = (shortcut.modifiers, shortcut.key);
            ensure!(!self.occupied.contains(&chord), "shortcut conflict");
            self.occupied.insert(chord);
            self.registered.insert(id, shortcut);
            Ok(())
        }
        fn unregister(&mut self, id: u32) -> Result<()> {
            ensure!(
                !self.fail_unregister_once.remove(&id),
                "injected removal failure"
            );
            if let Some(shortcut) = self.registered.remove(&id) {
                self.occupied.remove(&(shortcut.modifiers, shortcut.key));
            }
            Ok(())
        }
    }

    fn shortcut(action: Action, key: Key) -> Shortcut {
        Shortcut {
            action,
            modifiers: Modifiers {
                control: true,
                ..Default::default()
            },
            key,
        }
    }

    #[test]
    fn startup_conflict_releases_every_staged_binding_and_retry_recovers_saved_set() {
        let stop = crate::app_preferences::EMERGENCY_STOP_SHORTCUT;
        let pause = shortcut(Action::PauseResume, Key::F10);
        let mut backend = FakeHotkeys::default();
        backend.occupied.insert((pause.modifiers, pause.key));
        let mut registry = HotkeyRegistry::new(backend);
        registry.initialize(&[stop, pause]);

        assert!(registry.accepted().is_empty());
        assert!(registry.backend.registered.is_empty());
        assert!(ensure_shortcuts_active(&registry.status).is_err());
        assert!(
            registry
                .status
                .lock()
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .contains("shortcut conflict")
        );
        assert_eq!(registry.action(INITIAL_HOTKEY_ID), None);
        assert!(registry.retry().is_err());
        assert!(registry.backend.registered.is_empty());
        assert!(!registry.status.lock().unwrap().active);

        registry
            .backend
            .occupied
            .remove(&(pause.modifiers, pause.key));
        registry.retry().unwrap();
        assert_eq!(registry.accepted().len(), 2);
        assert_eq!(registry.backend.registered.len(), 2);
        assert_eq!(
            *registry.status.lock().unwrap(),
            ShortcutStatus {
                active: true,
                error: None
            }
        );
        assert!(ensure_shortcuts_active(&registry.status).is_ok());
    }

    #[test]
    fn inactive_set_requires_commit_and_retries_latest_accepted_edits() {
        let blocked = shortcut(Action::Stop, Key::F10);
        let edited = shortcut(Action::PauseResume, Key::F9);
        let mut backend = FakeHotkeys::default();
        backend.occupied.insert((blocked.modifiers, blocked.key));
        let mut registry = HotkeyRegistry::new(backend);
        registry.initialize(&[blocked]);
        let initial_error = registry.status.lock().unwrap().error.clone();

        registry.prepare(10, &[edited]).unwrap();
        let staged_id = registry.pending.as_ref().unwrap().staged_ids[0];
        assert_eq!(registry.action(staged_id), None);
        assert!(registry.retry().is_err());
        registry.rollback(10).unwrap();
        assert_eq!(registry.status.lock().unwrap().error, initial_error);
        assert!(registry.retry().is_err());
        assert_eq!(registry.retry_source, vec![blocked]);

        registry.prepare(11, &[edited]).unwrap();
        registry.commit(11).unwrap();
        assert_eq!(registry.retry_source, vec![edited]);
        registry.retry().unwrap();
        assert_eq!(
            registry.accepted().values().copied().collect::<Vec<_>>(),
            vec![edited]
        );
        assert!(registry.status.lock().unwrap().active);
        assert!(registry.status.lock().unwrap().error.is_none());

        registry.prepare(12, &[]).unwrap();
        registry.commit(12).unwrap();
        registry.retry().unwrap();
        assert!(registry.accepted().is_empty());
        assert!(registry.backend.registered.is_empty());
        assert!(ensure_shortcuts_active(&registry.status).is_ok());
    }

    #[test]
    fn commit_rollback_preserves_healthy_set_but_failed_restoration_disables_dispatch() {
        let first = shortcut(Action::Stop, Key::F11);
        let second = shortcut(Action::PauseResume, Key::F10);
        let replacement = shortcut(Action::ToggleKeyboard, Key::F9);
        let mut registry = HotkeyRegistry::new(FakeHotkeys::default());
        registry.initialize(&[first, second]);
        let ids = registry.accepted().keys().copied().collect::<Vec<_>>();
        registry.prepare(20, &[replacement]).unwrap();
        registry.backend.fail_unregister_once.insert(ids[1]);
        assert!(registry.commit(20).is_err());
        assert_eq!(registry.action(ids[0]), Some(LifecycleAction::Stop));
        assert_eq!(registry.action(ids[1]), Some(LifecycleAction::PauseResume));
        assert!(registry.status.lock().unwrap().active);
        assert_eq!(registry.backend.registered.len(), 2);

        registry.prepare(21, &[replacement]).unwrap();
        registry.backend.fail_unregister_once.insert(ids[1]);
        registry.backend.fail_register_id = Some(ids[0]);
        assert!(registry.commit(21).is_err());
        assert!(registry.accepted().is_empty());
        assert_eq!(registry.action(ids[0]), None);
        assert_eq!(registry.action(ids[1]), None);
        assert!(registry.backend.registered.is_empty());
        assert!(ensure_shortcuts_active(&registry.status).is_err());
        assert_eq!(registry.retry_source, vec![first, second]);
        registry.retry().unwrap();
        assert_eq!(registry.accepted().len(), 2);
        assert!(registry.status.lock().unwrap().active);
    }

    #[test]
    fn candidate_conflict_keeps_previous_stop_binding_dispatchable() {
        let original = shortcut(Action::Stop, Key::F11);
        let mut backend = FakeHotkeys::default();
        backend.occupied.insert((
            Modifiers {
                control: true,
                ..Default::default()
            },
            Key::F10,
        ));
        let mut registry = HotkeyRegistry::new(backend);
        registry.prepare(1, &[original]).unwrap();
        registry.commit(1).unwrap();
        let old_id = *registry.accepted().keys().next().unwrap();

        assert!(
            registry
                .prepare(2, &[shortcut(Action::Stop, Key::F10)])
                .is_err()
        );
        assert_eq!(registry.action(old_id), Some(LifecycleAction::Stop));
    }

    #[test]
    fn staged_binding_is_ignored_until_commit_and_drop_can_rollback() {
        let original = shortcut(Action::Stop, Key::F11);
        let candidate = shortcut(Action::PauseResume, Key::F10);
        let mut registry = HotkeyRegistry::new(FakeHotkeys::default());
        registry.prepare(1, &[original]).unwrap();
        registry.commit(1).unwrap();
        let old_id = *registry.accepted().keys().next().unwrap();
        registry.prepare(2, &[original, candidate]).unwrap();
        let staged_id = *registry
            .pending
            .as_ref()
            .unwrap()
            .staged_ids
            .last()
            .unwrap();
        assert_eq!(registry.action(old_id), Some(LifecycleAction::Stop));
        assert_eq!(registry.action(staged_id), None);
        registry.rollback(2).unwrap();
        assert_eq!(registry.accepted().len(), 1);
        assert_eq!(registry.action(old_id), Some(LifecycleAction::Stop));

        registry.prepare(3, &[original, candidate]).unwrap();
        registry.commit(3).unwrap();
        let pause_id = *registry
            .accepted()
            .iter()
            .find(|(_, shortcut)| shortcut.action == Action::PauseResume)
            .unwrap()
            .0;
        assert_eq!(
            registry.action(pause_id),
            Some(LifecycleAction::PauseResume)
        );
    }

    #[test]
    fn shared_stop_binding_is_dispatched_only_after_commit_and_released_on_rollback() {
        let stop = crate::app_preferences::EMERGENCY_STOP_SHORTCUT;
        let mut registry = HotkeyRegistry::new(FakeHotkeys::default());
        registry.prepare(1, &[stop]).unwrap();
        let staged_id = registry.pending.as_ref().unwrap().staged_ids[0];
        assert_eq!(registry.action(staged_id), None);
        registry.rollback(1).unwrap();
        assert!(registry.accepted().is_empty());
        assert!(registry.backend.registered.is_empty());

        registry.prepare(2, &[stop]).unwrap();
        registry.commit(2).unwrap();
        let accepted_id = *registry.accepted().keys().next().unwrap();
        assert_eq!(registry.action(accepted_id), Some(LifecycleAction::Stop));
    }

    #[test]
    fn shared_stop_health_policy_tracks_only_committed_shortcut_preferences() {
        let shared_stop = crate::app_preferences::EMERGENCY_STOP_SHORTCUT;
        let custom_stop = Shortcut {
            key: Key::F10,
            ..shared_stop
        };
        let mut registry = HotkeyRegistry::new(FakeHotkeys::default());
        registry.prepare(1, &[shared_stop]).unwrap();
        registry.commit(1).unwrap();
        assert!(accepted_uses_shared_stop(registry.accepted()));

        registry.prepare(2, &[custom_stop]).unwrap();
        assert!(accepted_uses_shared_stop(registry.accepted()));
        registry.rollback(2).unwrap();
        assert!(accepted_uses_shared_stop(registry.accepted()));

        registry.prepare(3, &[custom_stop]).unwrap();
        registry.commit(3).unwrap();
        assert!(!accepted_uses_shared_stop(registry.accepted()));
    }

    #[test]
    fn action_reassignment_reuses_the_accepted_hotkey_chord() {
        let chord = Modifiers {
            control: true,
            alt: true,
            ..Default::default()
        };
        let stop = Shortcut {
            action: Action::Stop,
            modifiers: chord,
            key: Key::F11,
        };
        let pause = Shortcut {
            action: Action::PauseResume,
            ..stop
        };
        let mut registry = HotkeyRegistry::new(FakeHotkeys::default());
        registry.prepare(1, &[stop]).unwrap();
        registry.commit(1).unwrap();
        let id = *registry.accepted().keys().next().unwrap();

        registry.prepare(2, &[pause]).unwrap();
        assert!(registry.pending.as_ref().unwrap().staged_ids.is_empty());
        assert_eq!(registry.action(id), Some(LifecycleAction::Stop));
        registry.commit(2).unwrap();

        assert_eq!(registry.accepted().len(), 1);
        assert_eq!(registry.accepted().get(&id), Some(&pause));
        assert_eq!(registry.action(id), Some(LifecycleAction::PauseResume));
    }
}
