//! Concurrent virtual-device routing. All policy and cursor ownership live in Rust.
use anyhow::{Context, Result, ensure};
use mux_core::{
    Profile, ResolvedSeat,
    desktop::{Bounds, Cursor, CursorSettings, HorizontalWheelPolicy},
    emulated::{BASE, MASK, Packet},
};
use mux_windows::{
    keyboard_focus::{self, RoutingState},
    overlay::{DesktopGuard, Overlay, OverlayStatus},
    protoinput::Session,
    targets,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, error::Error as StdError, fmt, time::Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseFailureDetail {
    pub user: usize,
    pub device: u32,
    pub identity: u32,
    pub detail: String,
}

/// Saved native releases that remain owned by their original user and recipient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseFailure {
    pub failures: Vec<ReleaseFailureDetail>,
}

impl fmt::Display for ReleaseFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} input release(s) remain pending", self.failures.len())?;
        for failure in &self.failures {
            write!(
                f,
                "; user {} device {} identity {}: {}",
                failure.user, failure.device, failure.identity, failure.detail
            )?;
        }
        Ok(())
    }
}

impl StdError for ReleaseFailure {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub name: String,
    pub pid: u32,
    pub client: (i32, i32),
    pub bounds: Bounds,
    #[serde(default)]
    pub movement_bounds: Bounds,
    pub buttons: Vec<u32>,
    pub keys: Vec<u32>,
    pub overlay: OverlayStatus,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub root: bool,
    #[serde(default)]
    pub captured: bool,
    #[serde(default)]
    pub keyboard_pid: u32,
    #[serde(default)]
    pub mode: Option<mux_core::routing::Mode>,
    #[serde(default)]
    pub pending_releases: Option<usize>,
    #[serde(default)]
    pub keyboard: Option<RoutingState>,
}

pub(crate) fn keyboard_supported(seat: &ResolvedSeat, desktop: bool) -> bool {
    desktop
        && seat.mouse & MASK == BASE
        && seat
            .keyboard
            .is_some_and(|keyboard| keyboard & MASK == BASE)
}

pub(crate) fn validate_keyboard_settings(
    settings: &CursorSettings,
    seat: &ResolvedSeat,
    desktop: bool,
) -> Result<()> {
    ensure!(
        settings.keyboard == mux_core::keyboard::KeyboardSettings::default()
            || keyboard_supported(seat, desktop),
        "Keyboard options require a paired virtual keyboard with desktop cursor routing"
    );
    Ok(())
}
struct DesktopSeat {
    last_mouse_activity: Instant,
    cursor: Cursor,
    rect: Bounds,
    hwnd: usize,
    keyboard_hwnd: usize,
    overlay: Overlay,
    settings: CursorSettings,
}
struct Held {
    packet: Packet,
    user: usize,
    window: Option<mux_core::routing::Window>,
    release_error: Option<String>,
    last_release_attempt: Option<Instant>,
}

fn remember_sent_down(
    held: &mut BTreeMap<(u32, u32), Held>,
    token: (u32, u32),
    saved: Option<Held>,
    delivered: Result<()>,
) -> Result<()> {
    delivered?;
    if let Some(saved) = saved {
        held.insert(token, saved);
    }
    Ok(())
}

fn remember_release_failure(held: &mut Held, detail: String, attempted: Instant) {
    held.release_error = Some(detail);
    held.last_release_attempt = Some(attempted);
}

fn finish_release(
    held_tokens: &mut BTreeMap<(u32, u32), Held>,
    token: (u32, u32),
    result: Result<()>,
    original_process_alive: bool,
) -> Option<ReleaseFailureDetail> {
    let held = held_tokens.get(&token)?;
    let user = held.user;
    match result {
        Ok(()) => {
            held_tokens.remove(&token);
            None
        }
        Err(_) if !original_process_alive => {
            held_tokens.remove(&token);
            None
        }
        Err(error) => {
            let detail = format!("{error:#}");
            if let Some(held) = held_tokens.get_mut(&token) {
                remember_release_failure(held, detail.clone(), Instant::now());
            }
            Some(ReleaseFailureDetail {
                user,
                device: token.0,
                identity: token.1,
                detail,
            })
        }
    }
}

fn retire_user_tokens_if_exited(
    held_tokens: &mut BTreeMap<(u32, u32), Held>,
    user: usize,
    original_process_alive: bool,
) -> bool {
    if original_process_alive {
        return false;
    }
    held_tokens.retain(|_, held| held.user != user);
    true
}

fn attempt_all_releases(
    tokens: impl IntoIterator<Item = (u32, u32)>,
    mut attempt: impl FnMut((u32, u32)) -> Option<ReleaseFailureDetail>,
) -> Vec<ReleaseFailureDetail> {
    tokens.into_iter().filter_map(&mut attempt).collect()
}

fn continue_resume_seats(
    count: usize,
    mut resume: impl FnMut(usize) -> Result<()>,
) -> Vec<(usize, anyhow::Error)> {
    let mut failures = Vec::new();
    for user in 0..count {
        if let Err(error) = resume(user) {
            failures.push((user, error));
        }
    }
    failures
}

fn snapshot_state(
    seats: &[ResolvedSeat],
    held: &BTreeMap<(u32, u32), Held>,
    seat_errors: &[Option<String>],
    user: usize,
    pid: u32,
    seat: &DesktopSeat,
) -> Result<State> {
    let release_failures: Vec<_> = held
        .values()
        .filter(|held| held.user == user)
        .filter_map(|held| held.release_error.as_deref())
        .collect();
    let pending_releases = release_failures.len();
    let mut status = seat_errors
        .get(user)
        .and_then(Option::as_deref)
        .unwrap_or("Assigned application")
        .to_owned();
    if let Some(detail) = release_failures.first() {
        let release_status = format!("{pending_releases} release(s) pending: {detail}");
        if status == "Assigned application" {
            status = release_status;
        } else {
            status = format!("{status}; {release_status}");
        }
    }
    Ok(State {
        name: seats[user].name.clone(),
        pid,
        client: (seat.cursor.x, seat.cursor.y),
        bounds: seat.rect,
        movement_bounds: mux_windows::monitors::resolve(&seat.settings, Some(seat.rect))?,
        buttons: held
            .values()
            .filter(|held| held.user == user && held.packet.kind == 0)
            .map(|held| held.packet.flags)
            .collect(),
        keys: held
            .values()
            .filter(|held| held.user == user && held.packet.kind == 1)
            .map(|held| held.packet.data)
            .collect(),
        overlay: seat.overlay.status.clone(),
        title: String::new(),
        status,
        root: false,
        captured: held.values().any(|held| held.user == user),
        keyboard_pid: if pid == 0 { 0 } else { seats[user].target_pid },
        mode: None,
        pending_releases: Some(pending_releases),
        keyboard: keyboard_supported(&seats[user], true).then(|| {
            let clicked = Some(mux_core::routing::Window {
                hwnd: seat.keyboard_hwnd,
                pid: seats[user].target_pid,
            });
            if pid == 0 {
                RoutingState {
                    settings: seat.settings.keyboard,
                    clicked,
                    resolved: None,
                    fallback: false,
                    status: "Keyboard recipient unavailable: assigned process exited".into(),
                }
            } else {
                RoutingState::observe(seat.settings.keyboard, clicked)
            }
        }),
    })
}

fn record_motion_failure(
    seats: &[ResolvedSeat],
    seat_errors: &mut [Option<String>],
    error: &anyhow::Error,
) {
    if let Some(error) = error.downcast_ref::<mux_windows::protoinput::MotionPublicationError>() {
        for failure in &error.failures {
            if let Some(user) = seats.iter().position(|seat| {
                seat.mouse == failure.device || seat.keyboard == Some(failure.device)
            }) {
                seat_errors[user] = Some(format!("Motion publication failed: {}", failure.detail));
            }
        }
    }
}
pub struct Router {
    session: Session,
    seats: Vec<ResolvedSeat>,
    desktop: Vec<DesktopSeat>,
    input_settings: Vec<CursorSettings>,
    input_remainders: Vec<[f64; 2]>,
    guard: Option<DesktopGuard>,
    held: BTreeMap<(u32, u32), Held>,
    text_recipients: BTreeMap<usize, Option<keyboard_focus::RetainedRecipient>>,
    seat_errors: Vec<Option<String>>,
    pub paused: bool,
    user_pauses: Vec<crate::routing_engine::UserPauseState>,
    user_pause_retry: Vec<Option<Instant>>,
    shutdown_started: bool,
    shutdown_error: Option<String>,
}
impl Router {
    pub fn begin_text_delivery(&mut self, user: usize) -> Result<()> {
        ensure!(
            !self.text_recipients.contains_key(&user),
            "Nested Text delivery"
        );
        ensure!(user < self.seats.len(), "Unknown Text user");
        let recipient = if self.paused
            || self.user_pause_state(user).paused
            || !self.keyboard_enabled(user)
            || self.desktop.is_empty()
        {
            None
        } else {
            let clicked = mux_core::routing::Window {
                hwnd: self.desktop[user].keyboard_hwnd,
                pid: self.seats[user].target_pid,
            };
            Some(keyboard_focus::RetainedRecipient::capture(
                clicked,
                self.input_settings[user].keyboard.focus,
            )?)
        };
        self.text_recipients.insert(user, recipient);
        Ok(())
    }
    pub fn end_text_delivery(&mut self, user: usize) {
        self.text_recipients.remove(&user);
    }
    pub fn poll_motion(&mut self) -> Result<()> {
        match self.session.poll_motion() {
            Ok(()) => Ok(()),
            Err(error)
                if error
                    .downcast_ref::<mux_windows::protoinput::MotionPublicationError>()
                    .is_some() =>
            {
                record_motion_failure(&self.seats, &mut self.seat_errors, &error);
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
    pub fn motion_deadline(&self) -> Option<Instant> {
        self.session.motion_deadline()
    }
    fn clear_target_error(&mut self, user: usize) {
        if let Some(status) = self.seat_errors.get_mut(user)
            && status.as_ref().is_some_and(|status| {
                status.starts_with("Assigned target window unavailable:")
                    || status.starts_with("Assigned target unavailable:")
            })
        {
            *status = None;
        }
    }
    fn clear_publication_error(&mut self, user: usize) {
        if let Some(status) = self.seat_errors.get_mut(user)
            && status.as_ref().is_some_and(|status| {
                status.starts_with("Motion publication failed:")
                    || status.starts_with("Assigned target window unavailable:")
                    || status.starts_with("Assigned target unavailable:")
            })
        {
            *status = None;
        }
    }
    fn hide_overlay(&mut self, user: usize) -> Result<()> {
        let seat = self.desktop.get_mut(user).context("Unknown overlay user")?;
        let mut hidden = seat.settings.clone();
        hidden.visible = false;
        seat.overlay.draw(
            seat.cursor.x + seat.rect.left,
            seat.cursor.y + seat.rect.top,
            &self.seats[user].name,
            &hidden,
            self.paused,
        )?;
        seat.overlay.status.idle_hidden = false;
        Ok(())
    }
    pub fn note_mouse_activity(&mut self, user: usize) -> Result<()> {
        ensure!(user < self.seats.len(), "Unknown cursor user");
        if let Some(seat) = self.desktop.get_mut(user) {
            seat.last_mouse_activity = Instant::now();
        }
        Ok(())
    }
    pub fn pointer(&self, user: usize) -> Result<(i32, i32)> {
        let seat = self
            .desktop
            .get(user)
            .context("Annotation requires concurrent assigned routing")?;
        let rect = targets::client_bounds(seat.hwnd, self.seats[user].target_pid)?;
        Ok((seat.cursor.x + rect.left, seat.cursor.y + rect.top))
    }
    pub fn selected_window(&self, user: usize) -> Result<Option<mux_core::routing::Window>> {
        let seat = self
            .desktop
            .get(user)
            .context("Target capture requires concurrent assigned routing")?;
        targets::client_bounds(seat.hwnd, self.seats[user].target_pid)?;
        Ok(Some(mux_core::routing::Window {
            hwnd: seat.hwnd,
            pid: self.seats[user].target_pid,
        }))
    }
    pub fn mouse_window(&self, user: usize) -> Result<Option<mux_core::routing::Window>> {
        Ok(Some(self.pointer_window(user)?))
    }
    pub fn keyboard_window(&self, user: usize) -> Result<Option<mux_core::routing::Window>> {
        if let Some(Some(recipient)) = self.text_recipients.get(&user) {
            return recipient.validate().map(Some);
        }
        let seat = self
            .desktop
            .get(user)
            .context("Unknown assigned keyboard user")?;
        let clicked = mux_core::routing::Window {
            hwnd: seat.keyboard_hwnd,
            pid: self.seats[user].target_pid,
        };
        Ok(Some(
            keyboard_focus::resolve(clicked, self.input_settings[user].keyboard.focus)?.resolved,
        ))
    }
    pub fn keyboard_enabled(&self, user: usize) -> bool {
        self.input_settings
            .get(user)
            .is_some_and(|settings| settings.keyboard.enabled)
    }
    pub fn movement_window(
        &self,
        user: usize,
        x: i32,
        y: i32,
    ) -> Result<Option<mux_core::routing::Window>> {
        let root = self
            .desktop
            .get(user)
            .context("Unknown assigned movement user")?;
        if let Some(window) = self
            .held
            .values()
            .find(|held| held.user == user && held.packet.kind == 0)
            .and_then(|held| held.window)
        {
            return Ok(Some(window));
        }
        let rect = targets::client_bounds(root.hwnd, self.seats[user].target_pid)?;
        let bounds = mux_windows::monitors::resolve(&root.settings, Some(rect))?;
        let bounds = mux_core::desktop::Bounds {
            left: bounds.left - rect.left,
            top: bounds.top - rect.top,
            right: bounds.right - rect.left,
            bottom: bounds.bottom - rect.top,
        };
        let local_x = x
            .saturating_sub(rect.left)
            .clamp(bounds.left, bounds.right - 1);
        let local_y = y
            .saturating_sub(rect.top)
            .clamp(bounds.top, bounds.bottom - 1);
        Ok(Some(targets::child_at_point(
            mux_core::routing::Window {
                hwnd: root.hwnd,
                pid: self.seats[user].target_pid,
            },
            local_x + rect.left,
            local_y + rect.top,
        )?))
    }
    pub fn packet_window(
        &self,
        user: usize,
        packet: Packet,
    ) -> Result<Option<mux_core::routing::Window>> {
        if packet.kind != 0 || packet.flags != 0 {
            return if packet.kind == 0 {
                self.mouse_window(user)
            } else {
                self.keyboard_window(user)
            };
        }
        if let Some(window) = self
            .held
            .values()
            .find(|held| held.user == user && held.packet.kind == 0)
            .and_then(|held| held.window)
        {
            return Ok(Some(window));
        }
        let root = self
            .desktop
            .get(user)
            .context("Unknown assigned movement user")?;
        let mut cursor = root.cursor.clone();
        cursor.motion(packet.x, packet.y, &root.settings, Self::bounds(root)?);
        let screen_x = cursor.x + root.rect.left;
        let screen_y = cursor.y + root.rect.top;
        Ok(Some(targets::child_at_point(
            mux_core::routing::Window {
                hwnd: root.hwnd,
                pid: self.seats[user].target_pid,
            },
            screen_x,
            screen_y,
        )?))
    }
    fn window_lock(settings: &CursorSettings) -> bool {
        use mux_core::desktop::BoundsMode;
        settings.bounds_mode == BoundsMode::Window
            || settings.bounds_mode == BoundsMode::Legacy && settings.lock_window
    }
    pub fn preflight(profile: &Profile, seats: &[ResolvedSeat]) -> Result<Vec<usize>> {
        if !profile.concurrent {
            return Ok(Vec::new());
        }
        ensure!(
            seats
                .iter()
                .all(|s| s.mouse & MASK == BASE && s.keyboard.is_none_or(|k| k & MASK == BASE)),
            "Concurrent desktop mode currently requires virtual devices"
        );
        let windows = targets::windows()?;
        seats
            .iter()
            .map(|s| {
                let hwnd = windows
                    .iter()
                    .find(|w| w.pid == s.target_pid)
                    .context("assigned process has no visible application window")?
                    .hwnd;
                targets::client_bounds(hwnd, s.target_pid)?;
                Ok(hwnd)
            })
            .collect()
    }
    pub fn new(
        session: Session,
        profile: &Profile,
        seats: Vec<ResolvedSeat>,
        windows: Vec<usize>,
        guard: Option<DesktopGuard>,
    ) -> Result<Self> {
        for (user, seat) in seats.iter().enumerate() {
            profile.seats[user]
                .cursor
                .keyboard
                .validate_application_route(false)?;
            validate_keyboard_settings(&profile.seats[user].cursor, seat, user < windows.len())?;
        }
        ensure!(
            profile.seats.iter().all(|seat| {
                seat.cursor.horizontal_wheel != HorizontalWheelPolicy::Preserve
                    || windows.len() == profile.seats.len()
            }),
            "preserving horizontal wheel requires concurrent assigned cursor routing"
        );
        let user_pauses = profile
            .seats
            .iter()
            .enumerate()
            .map(|(user, seat)| -> Result<_> {
                Ok(
                    if profile.concurrent
                        && windows.len() == seats.len()
                        && crate::routing_engine::virtual_pause_devices(seat)?
                        && seats
                            .iter()
                            .filter(|other| {
                                other.mouse == seats[user].mouse
                                    || other.keyboard == seats[user].keyboard
                            })
                            .count()
                            == 1
                    {
                        crate::routing_engine::UserPauseState {
                            user,
                            supported: true,
                            ..Default::default()
                        }
                    } else {
                        crate::routing_engine::UserPauseState::unsupported(
                            user,
                            "Per-user Pause requires concurrent assigned virtual routing",
                        )
                    },
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let mut result = Self {
            session,
            seats,
            desktop: Vec::new(),
            text_recipients: BTreeMap::new(),
            input_settings: profile
                .seats
                .iter()
                .map(|seat| seat.cursor.clone())
                .collect(),
            input_remainders: vec![[0.; 2]; profile.seats.len()],
            guard,
            held: BTreeMap::new(),
            seat_errors: vec![None; profile.seats.len()],
            paused: false,
            user_pauses,
            user_pause_retry: vec![None; profile.seats.len()],
            shutdown_started: false,
            shutdown_error: None,
        };
        for (i, hwnd) in windows.into_iter().enumerate() {
            let rect = targets::client_bounds(hwnd, result.seats[i].target_pid)?;
            let x = (rect.right - rect.left) / 2;
            let y = (rect.bottom - rect.top) / 2;
            result.session.desktop(i, hwnd, true)?;
            // Home only this virtual input stream using the backend's client bounds.
            result
                .session
                .submit(Packet::motion(result.seats[i].mouse, -32767, -32767))?;
            result
                .session
                .submit(Packet::motion(result.seats[i].mouse, x + 1, y + 1))?;
            let settings = profile.seats[i].cursor.clone();
            result
                .session
                .desktop(i, hwnd, Self::window_lock(&settings))?;
            result.desktop.push(DesktopSeat {
                last_mouse_activity: Instant::now(),
                cursor: Cursor::new(x, y),
                rect,
                hwnd,
                keyboard_hwnd: hwnd,
                overlay: Overlay::new(&result.seats[i].name)?,
                settings,
            });
        }
        Ok(result)
    }
    pub fn key_held(&self, device: u32, key: u32) -> bool {
        self.held.contains_key(&(device, key))
    }
    pub fn button_held(&self, device: u32, button: u32) -> bool {
        button < 5 && self.held.contains_key(&(device, 1 << (button * 2)))
    }
    fn source_button_mask(&self, device: u32) -> u8 {
        (0..5).fold(0, |mask, button| {
            if self.button_held(device, button) {
                mask | (1 << button)
            } else {
                mask
            }
        })
    }
    pub fn button_output_disabled(&self, packet: Packet) -> bool {
        if packet.kind != 0
            || !matches!(packet.flags, 1 | 2 | 4 | 8 | 16 | 32 | 64 | 128 | 256 | 512)
        {
            return false;
        }
        self.seats
            .iter()
            .position(|seat| seat.mouse == packet.device)
            .and_then(|index| self.input_settings.get(index))
            .is_some_and(|settings| settings.transform_button_flags(packet.flags).is_none())
    }
    fn pointer_window(&self, user: usize) -> Result<mux_core::routing::Window> {
        if let Some(held) = self
            .held
            .values()
            .find(|held| held.user == user && held.packet.kind == 0)
            && let Some(window) = held.window
        {
            return Ok(window);
        }
        let seat = &self.desktop[user];
        targets::child_at_point(
            mux_core::routing::Window {
                hwnd: seat.hwnd,
                pid: self.seats[user].target_pid,
            },
            seat.cursor.x + seat.rect.left,
            seat.cursor.y + seat.rect.top,
        )
    }
    fn position_pointer(&mut self, user: usize) -> Result<mux_core::routing::Window> {
        self.publish_pointer(user, false)
    }
    fn publish_pointer(
        &mut self,
        user: usize,
        coalesce: bool,
    ) -> Result<mux_core::routing::Window> {
        let window = self.pointer_window(user)?;
        self.publish_pointer_to(user, window, coalesce)?;
        Ok(window)
    }
    fn publish_pointer_to(
        &mut self,
        user: usize,
        window: mux_core::routing::Window,
        coalesce: bool,
    ) -> Result<()> {
        if self.user_pause_state(user).paused {
            return Ok(());
        }
        let (x, y, coalescing) = {
            let seat = &self.desktop[user];
            (
                seat.cursor.x + seat.rect.left,
                seat.cursor.y + seat.rect.top,
                seat.settings.motion_coalescing,
            )
        };
        let rect = targets::client_bounds(window.hwnd, window.pid)?;
        self.clear_target_error(user);
        if coalesce {
            self.session.queue_position(
                user,
                window.hwnd,
                self.seats[user].mouse,
                x,
                y,
                rect,
                coalescing,
            )?;
        } else {
            self.session
                .position(user, window.hwnd, self.seats[user].mouse, x, y, rect)?;
            self.clear_publication_error(user);
        }
        Ok(())
    }
    fn attempt_release(&mut self, token: (u32, u32)) -> Option<ReleaseFailureDetail> {
        let held = self.held.get(&token)?;
        let user = held.user;
        let packet = held.packet;
        let window = held.window;
        if !self.session.alive(user) {
            self.held.remove(&token);
            return None;
        }

        // A motion publication failure must not suppress an UP. The native queue
        // drains all saved segments before returning typed per-device failures.
        let scoped = self.user_pauses[user].paused;
        if !scoped && let Err(error) = self.session.flush_motion() {
            record_motion_failure(&self.seats, &mut self.seat_errors, &error);
        }
        let up = Self::release(packet);
        let result = if scoped {
            window
                .context("Paused cleanup has no saved recipient")
                .and_then(|window| self.session.submit_release_to(user, window.hwnd, up))
        } else if let Some(window) = window {
            self.session.submit_to(user, window.hwnd, up)
        } else {
            self.session.submit(up)
        };
        if let Err(error) = &result {
            record_motion_failure(&self.seats, &mut self.seat_errors, error);
        } else {
            self.clear_publication_error(user);
        }
        // The watched process object proves whether a failed original-recipient UP
        // can ever be retried safely; no replacement process is used.
        finish_release(&mut self.held, token, result, self.session.alive(user))
    }

    fn release_tokens(&mut self, tokens: impl IntoIterator<Item = (u32, u32)>) -> Result<()> {
        let failures = attempt_all_releases(tokens, |token| self.attempt_release(token));
        if failures.is_empty() {
            Ok(())
        } else {
            Err(anyhow::Error::new(ReleaseFailure { failures }))
        }
    }

    fn retry_pending_releases(&mut self) {
        const RETRY_AFTER: std::time::Duration = std::time::Duration::from_millis(250);
        let now = Instant::now();
        let tokens: Vec<_> = self
            .held
            .iter()
            .filter(|(_, held)| {
                !self.user_pauses[held.user].paused
                    && held.release_error.is_some()
                    && held
                        .last_release_attempt
                        .is_none_or(|last| now.duration_since(last) >= RETRY_AFTER)
            })
            .map(|(token, _)| *token)
            .collect();
        for token in tokens {
            let _ = self.attempt_release(token);
        }
        for user in 0..self.user_pauses.len() {
            if self.user_pauses[user].paused
                && (self.user_pause_state(user).pending_releases != 0
                    || self.user_pauses[user].error.is_some())
                && self.user_pause_retry[user]
                    .is_none_or(|last| last.elapsed() >= std::time::Duration::from_millis(250))
            {
                let _ = self.pause_user(user, true);
            }
        }
    }

    fn retire_exited_user(&mut self, user: usize) {
        if !retire_user_tokens_if_exited(&mut self.held, user, self.session.alive(user)) {
            return;
        }
        if let Some(error) = self.seat_errors.get_mut(user) {
            *error =
                Some("Assigned process exited; pending releases retired without delivery".into());
        }
    }

    fn release_status(&self, user: usize) -> (usize, Option<String>) {
        let failures: Vec<_> = self
            .held
            .values()
            .filter(|held| held.user == user)
            .filter_map(|held| held.release_error.as_deref())
            .collect();
        let detail = if failures.is_empty() {
            None
        } else {
            Some(format!(
                "{} release(s) pending: {}",
                failures.len(),
                failures[0]
            ))
        };
        (failures.len(), detail)
    }

    fn seat_status(&self, user: usize) -> String {
        let (pending, release_detail) = self.release_status(user);
        let mut status = self
            .seat_errors
            .get(user)
            .and_then(Option::as_deref)
            .unwrap_or("Assigned application")
            .to_owned();
        if let Some(detail) = release_detail {
            if status == "Assigned application" {
                status = detail;
            } else {
                status = format!("{status}; {detail}");
            }
        } else if pending == 0 && self.session.alive(user) && status == "Assigned process exited" {
            status = "Assigned application".into();
        }
        status
    }

    pub fn input(&mut self, packet: Packet) -> Result<()> {
        self.input_inner(packet, None)
    }
    pub fn input_with_source_buttons(&mut self, packet: Packet, source_buttons: u8) -> Result<()> {
        ensure!(source_buttons & !0x1f == 0, "invalid source button mask");
        self.input_inner(packet, Some(source_buttons))
    }
    fn input_inner(&mut self, mut packet: Packet, source_buttons: Option<u8>) -> Result<()> {
        packet.validate()?;
        let index = self
            .seats
            .iter()
            .position(|s| {
                if packet.kind == 0 {
                    s.mouse == packet.device
                } else {
                    s.keyboard == Some(packet.device)
                }
            })
            .context("unassigned virtual device")?;
        let release = if packet.kind == 1 {
            packet.flags & 1 != 0
        } else {
            matches!(packet.flags, 2 | 8 | 32 | 128 | 512)
        };
        if self.user_pauses[index].paused && !release {
            return Ok(());
        }
        if packet.kind == 0 {
            self.note_mouse_activity(index)?;
        }
        if self.paused && !release {
            return Ok(());
        }
        let identity = if packet.kind == 1 {
            packet.data
        } else {
            match packet.flags {
                1 | 2 => 1,
                4 | 8 => 4,
                16 | 32 => 16,
                64 | 128 => 64,
                256 | 512 => 256,
                _ => 0,
            }
        };
        let token = (packet.device, identity);
        if release {
            return self.release_tokens([token]);
        }
        if let Some(seat) = self.desktop.get_mut(index)
            && packet.kind == 0
        {
            if packet.flags == 0 {
                let bounds = Self::bounds(seat)?;
                let (dx, dy) = seat
                    .cursor
                    .motion(packet.x, packet.y, &seat.settings, bounds);
                packet.x = dx;
                packet.y = dy;
            } else {
                let inside = seat.rect.contains(
                    seat.cursor.x + seat.rect.left,
                    seat.cursor.y + seat.rect.top,
                );
                if !inside {
                    return Ok(());
                }
            }
        } else if packet.kind == 0 && packet.flags == 0 {
            (packet.x, packet.y) = self.input_settings[index].transform_motion(
                packet.x,
                packet.y,
                &mut self.input_remainders[index],
            );
        }
        if packet.kind == 0 && packet.flags != 0 {
            let settings = &self.input_settings[index];
            if matches!(packet.flags, 0x400 | 0x800) {
                let buttons =
                    source_buttons.unwrap_or_else(|| self.source_button_mask(packet.device));
                let Some(transformed) =
                    settings.transform_wheel_packet_with_source_buttons(packet, buttons)
                else {
                    return Ok(());
                };
                packet = transformed;
            } else {
                let Some(flags) = settings.transform_button_flags(packet.flags) else {
                    return Ok(());
                };
                packet.flags = flags;
            }
        }
        if packet.kind == 0 && packet.flags == 0x800 {
            ensure!(
                index < self.desktop.len(),
                "horizontal wheel delivery requires automatic desktop or concurrent assigned routing"
            );
            let window = self.position_pointer(index)?;
            let modifiers = self.wheel_modifiers(index);
            mux_windows::wheel::post_horizontal_wheel(
                window.hwnd,
                window.pid,
                self.pointer(index)?,
                packet.data as u16 as i16,
                modifiers,
            )?;
            return Ok(());
        }
        let window = if !self.desktop.is_empty() {
            if packet.kind == 0 {
                let window = self.pointer_window(index)?;
                let keyboard_focus_changed =
                    identity != 0 && self.desktop[index].keyboard_hwnd != window.hwnd;
                if keyboard_focus_changed {
                    let keys: Vec<_> = self
                        .held
                        .iter()
                        .filter(|(_, held)| held.user == index && held.packet.kind == 1)
                        .map(|(token, _)| *token)
                        .collect();
                    self.release_tokens(keys)?;
                }
                self.publish_pointer_to(index, window, packet.flags == 0)?;
                if keyboard_focus_changed {
                    self.desktop[index].keyboard_hwnd = window.hwnd;
                }
                Some(window)
            } else {
                self.keyboard_window(index)?
            }
        } else {
            None
        };
        let new_held = if identity != 0 && (packet.kind == 1 || packet.flags != 0x400) {
            if self.held.contains_key(&token) {
                return Ok(());
            }
            Some(Held {
                packet,
                user: index,
                window,
                release_error: None,
                last_release_attempt: None,
            })
        } else {
            None
        };
        if let Some(window) = window {
            // Absolute positioning already accepted the transformed virtual motion for publication.
            let delivered = if packet.kind != 0 || packet.flags != 0 {
                self.session.submit_to(index, window.hwnd, packet)
            } else {
                Ok(())
            };
            if delivered.is_ok() && (packet.kind != 0 || packet.flags != 0) {
                self.clear_publication_error(index);
            }
            return remember_sent_down(&mut self.held, token, new_held, delivered);
        }
        // The publication queue splits effective deltas to the bounded native packet ABI.
        let delivered = if packet.kind == 0 && packet.flags == 0 {
            if packet.x != 0 || packet.y != 0 {
                self.session
                    .queue_motion(packet, self.input_settings[index].motion_coalescing)
            } else {
                Ok(())
            }
        } else {
            self.session.submit(packet)
        };
        remember_sent_down(&mut self.held, token, new_held, delivered)
    }
    fn bounds(seat: &DesktopSeat) -> Result<Bounds> {
        let b = mux_windows::monitors::resolve(&seat.settings, Some(seat.rect))?;
        Ok(Bounds {
            left: b.left - seat.rect.left,
            top: b.top - seat.rect.top,
            right: b.right - seat.rect.left,
            bottom: b.bottom - seat.rect.top,
        })
    }
    fn release(mut p: Packet) -> Packet {
        if p.kind == 1 {
            p.flags |= 1;
        } else {
            p.flags *= 2;
        }
        p
    }
    pub fn release_all(&mut self) -> Result<()> {
        self.release_tokens(self.held.keys().copied().collect::<Vec<_>>())
    }
    pub fn user_pause_state(&self, user: usize) -> crate::routing_engine::UserPauseState {
        let Some(state) = self.user_pauses.get(user) else {
            return crate::routing_engine::UserPauseState::unsupported(user, "Unknown user");
        };
        let mut state = state.clone();
        state.pending_releases = if state.paused {
            self.held.values().filter(|held| held.user == user).count()
        } else {
            0
        };
        state
    }
    pub fn validate_mode_change(&self, _multi_keyboard: bool) -> Result<()> {
        crate::routing_engine::validate_user_pause_mode_change(&self.user_pauses)
    }
    pub fn pause_user(&mut self, user: usize, paused: bool) -> Result<()> {
        let state = self.user_pause_state(user);
        ensure!(
            state.supported,
            "{}",
            state
                .error
                .as_deref()
                .unwrap_or("Per-user Pause is unavailable")
        );
        if !paused {
            self.user_pauses[user].resume(self.paused, state.pending_releases)?;
            self.user_pause_retry[user] = None;
            return Ok(());
        }
        self.user_pauses[user].paused = true;
        self.user_pauses[user].error = None;
        self.text_recipients.remove(&user);
        self.user_pause_retry[user] = Some(Instant::now());
        self.input_remainders[user] = [0.0; 2];
        let discarded = self.session.discard_user_motion(user).map(|_| ());
        let tokens: Vec<_> = self
            .held
            .iter()
            .filter(|(_, held)| held.user == user)
            .map(|(token, _)| *token)
            .collect();
        let released = self.release_tokens(tokens);
        let result = discarded.and(released);
        if let Err(error) = &result {
            self.user_pauses[user].error = Some(format!("{error:#}"));
        }
        result
    }
    pub fn shutdown(&mut self) -> Result<()> {
        if self.shutdown_started {
            return self
                .shutdown_error
                .as_ref()
                .map_or(Ok(()), |error| Err(anyhow::anyhow!("{error}")));
        }
        self.shutdown_started = true;
        let released = self.release_all();
        let drained = self.session.shutdown();
        let result = released.and(drained.map(|_| ()));
        self.shutdown_error = result.as_ref().err().map(|error| format!("{error:#}"));
        result
    }
    pub fn pause(&mut self, paused: bool) -> Result<()> {
        if paused {
            self.release_all()?;
            if let Err(error) = self.session.flush_motion() {
                record_motion_failure(&self.seats, &mut self.seat_errors, &error);
                return Err(error);
            }
        } else if let Err(error) = self.session.flush_motion() {
            record_motion_failure(&self.seats, &mut self.seat_errors, &error);
            if error
                .downcast_ref::<mux_windows::protoinput::MotionPublicationError>()
                .is_none()
            {
                return Err(error);
            }
        }
        self.session.pause(paused)?;
        self.paused = paused;
        if !paused {
            let failures = continue_resume_seats(self.desktop.len(), |user| {
                if self.user_pauses[user].paused {
                    return Ok(());
                }
                if !self.session.alive(user) {
                    self.retire_exited_user(user);
                    return self.hide_overlay(user);
                }
                self.position_pointer(user).map(|_| ())
            });
            for (user, error) in failures {
                if !self.session.alive(user)
                    && let Some(status) = self.seat_errors.get_mut(user)
                {
                    let current =
                        status.get_or_insert_with(|| "Assigned process exited".to_owned());
                    current.push_str(&format!("; overlay update failed: {error:#}"));
                    continue;
                }
                if error
                    .downcast_ref::<mux_windows::protoinput::MotionPublicationError>()
                    .is_some()
                {
                    record_motion_failure(&self.seats, &mut self.seat_errors, &error);
                    continue;
                } else if let Some(status) = self.seat_errors.get_mut(user) {
                    *status = Some(format!("Assigned target unavailable: {error:#}"));
                }
                if let Err(overlay_error) = self.hide_overlay(user)
                    && let Some(status) = self.seat_errors.get_mut(user)
                {
                    let current = status.get_or_insert_with(String::new);
                    if !current.is_empty() {
                        current.push_str("; ");
                    }
                    current.push_str(&format!("overlay update failed: {overlay_error:#}"));
                }
            }
        }
        Ok(())
    }
    pub fn settings(&mut self, index: usize, settings: CursorSettings) -> Result<()> {
        mux_windows::monitors::validate_settings(&settings)?;
        let devices = self.seats.get(index).context("Unknown user")?;
        settings.keyboard.validate_application_route(false)?;
        settings
            .keyboard
            .validate_live_update(self.input_settings[index].keyboard)?;
        validate_keyboard_settings(&settings, devices, index < self.desktop.len())?;
        ensure!(
            !settings.requires_extended_overlay() || index < self.desktop.len(),
            "Extended cursor appearance and idle hiding require desktop cursor routing"
        );
        ensure!(
            (settings.horizontal_wheel != HorizontalWheelPolicy::Preserve
                && settings.horizontal_wheel_emulation
                    == mux_core::desktop::HorizontalWheelEmulation::Never)
                || index < self.desktop.len(),
            "horizontal wheel delivery requires automatic desktop or concurrent assigned routing"
        );
        ensure!(
            settings.horizontal_wheel_emulation
                == mux_core::desktop::HorizontalWheelEmulation::Never
                || devices.mouse & mux_core::emulated::MASK == mux_core::emulated::BASE,
            "horizontal wheel emulation currently requires a virtual mouse"
        );
        let bounds = self
            .desktop
            .get(index)
            .map(|seat| mux_windows::monitors::resolve(&settings, Some(seat.rect)))
            .transpose()?;
        let tokens: Vec<_> = self
            .held
            .keys()
            .copied()
            .filter(|(device, _)| *device == devices.mouse || Some(*device) == devices.keyboard)
            .collect();
        self.release_tokens(tokens)?;
        if let Err(error) = self.session.flush_motion() {
            record_motion_failure(&self.seats, &mut self.seat_errors, &error);
            return Err(error);
        }
        if let Some(seat) = self.desktop.get(index) {
            self.session
                .desktop(index, seat.hwnd, Self::window_lock(&settings))?;
        }
        let input = self.input_settings.get_mut(index).context("Unknown user")?;
        *input = settings.clone();
        self.input_remainders[index] = [0.; 2];
        if let Some(seat) = self.desktop.get_mut(index) {
            seat.settings = settings;
            seat.last_mouse_activity = Instant::now();
            if let Some(bounds) = bounds {
                let x = seat.cursor.x + seat.rect.left;
                let y = seat.cursor.y + seat.rect.top;
                let new_x = x.clamp(bounds.left, bounds.right - 1) - seat.rect.left;
                let new_y = y.clamp(bounds.top, bounds.bottom - 1) - seat.rect.top;
                seat.cursor = Cursor::new(new_x, new_y);
            }
        }
        if !self.paused && !self.user_pauses[index].paused && index < self.desktop.len() {
            self.position_pointer(index)?;
        }
        Ok(())
    }
    fn wheel_modifiers(&self, user: usize) -> u16 {
        const MK_LBUTTON: u16 = 0x0001;
        const MK_RBUTTON: u16 = 0x0002;
        const MK_SHIFT: u16 = 0x0004;
        const MK_CONTROL: u16 = 0x0008;
        const MK_MBUTTON: u16 = 0x0010;
        const MK_XBUTTON1: u16 = 0x0020;
        const MK_XBUTTON2: u16 = 0x0040;
        let mut result = 0;
        for held in self.held.values().filter(|held| held.user == user) {
            if held.packet.kind == 1 {
                match held.packet.data {
                    0x10 | 0xA0 | 0xA1 => result |= MK_SHIFT,
                    0x11 | 0xA2 | 0xA3 => result |= MK_CONTROL,
                    _ => (),
                }
            } else if held.packet.kind == 0 {
                result |= match held.packet.flags {
                    0x001 => MK_LBUTTON,
                    0x004 => MK_RBUTTON,
                    0x010 => MK_MBUTTON,
                    0x040 => MK_XBUTTON1,
                    0x100 => MK_XBUTTON2,
                    _ => 0,
                };
            }
        }
        result
    }
    pub fn move_to(&mut self, user: usize, x: i32, y: i32) -> Result<()> {
        if self.user_pause_state(user).paused {
            return Ok(());
        }
        self.session.flush_motion()?;
        let seat = self
            .desktop
            .get_mut(user)
            .context("Absolute movement requires concurrent assigned routing")?;
        if self.paused {
            return Ok(());
        }
        let rect = targets::client_bounds(seat.hwnd, self.seats[user].target_pid)?;
        if rect != seat.rect {
            self.session
                .desktop(user, seat.hwnd, Self::window_lock(&seat.settings))?;
            seat.rect = rect;
        }
        let bounds = Self::bounds(seat)?;
        let new_x = (x.saturating_sub(rect.left)).clamp(bounds.left, bounds.right - 1);
        let new_y = (y.saturating_sub(rect.top)).clamp(bounds.top, bounds.bottom - 1);
        seat.cursor = Cursor::new(new_x, new_y);
        seat.last_mouse_activity = Instant::now();
        self.position_pointer(user)?;
        Ok(())
    }
    pub fn tick(&mut self) -> Result<(bool, Vec<State>)> {
        let stop = self.guard.as_ref().is_some_and(|g| g.pump());
        for user in 0..self.desktop.len() {
            self.retire_exited_user(user);
        }
        self.retry_pending_releases();
        let mut states = Vec::new();
        let mut reposition = Vec::new();
        for i in 0..self.desktop.len() {
            let pid = self.seats[i].target_pid;
            if !self.session.alive(i) {
                {
                    let seat = &mut self.desktop[i];
                    let mut hidden = seat.settings.clone();
                    hidden.visible = false;
                    seat.overlay.draw(
                        seat.cursor.x + seat.rect.left,
                        seat.cursor.y + seat.rect.top,
                        &self.seats[i].name,
                        &hidden,
                        self.paused,
                    )?;
                    seat.overlay.status.idle_hidden = false;
                }
                states.push(snapshot_state(
                    &self.seats,
                    &self.held,
                    &self.seat_errors,
                    i,
                    0,
                    &self.desktop[i],
                )?);
                continue;
            }
            let rect = match targets::client_bounds(self.desktop[i].hwnd, pid) {
                Ok(rect) => rect,
                Err(error) => {
                    self.seat_errors[i] =
                        Some(format!("Assigned target window unavailable: {error:#}"));
                    {
                        let seat = &mut self.desktop[i];
                        let mut hidden = seat.settings.clone();
                        hidden.visible = false;
                        seat.overlay.draw(
                            seat.cursor.x + seat.rect.left,
                            seat.cursor.y + seat.rect.top,
                            &self.seats[i].name,
                            &hidden,
                            self.paused,
                        )?;
                    }
                    states.push(snapshot_state(
                        &self.seats,
                        &self.held,
                        &self.seat_errors,
                        i,
                        pid,
                        &self.desktop[i],
                    )?);
                    continue;
                }
            };
            self.clear_target_error(i);
            let mut transition_failed = false;
            let mut motion_failure = None;
            let seat = &mut self.desktop[i];
            if rect != seat.rect && !self.user_pauses[i].paused {
                if let Err(error) =
                    self.session
                        .desktop(i, seat.hwnd, Self::window_lock(&seat.settings))
                {
                    if error
                        .downcast_ref::<mux_windows::protoinput::MotionPublicationError>()
                        .is_some()
                    {
                        motion_failure = Some(error);
                    } else {
                        self.seat_errors[i] =
                            Some(format!("Assigned target unavailable: {error:#}"));
                    }
                    let mut hidden = seat.settings.clone();
                    hidden.visible = false;
                    seat.overlay.draw(
                        seat.cursor.x + seat.rect.left,
                        seat.cursor.y + seat.rect.top,
                        &self.seats[i].name,
                        &hidden,
                        self.paused,
                    )?;
                    transition_failed = true;
                } else {
                    seat.rect = rect;
                    if !self.paused && !self.user_pauses[i].paused {
                        reposition.push(i);
                    }
                }
            }
            if let Some(error) = motion_failure {
                record_motion_failure(&self.seats, &mut self.seat_errors, &error);
            }
            if transition_failed {
                states.push(snapshot_state(
                    &self.seats,
                    &self.held,
                    &self.seat_errors,
                    i,
                    pid,
                    seat,
                )?);
                continue;
            }
            if !self.paused && !self.user_pauses[i].paused {
                let bounds = Self::bounds(seat)?;
                let (dx, dy) = seat.cursor.motion(0, 0, &CursorSettings::default(), bounds);
                if dx != 0 || dy != 0 {
                    reposition.push(i);
                }
            }
            let mouse_held = self
                .held
                .values()
                .any(|held| held.user == i && held.packet.kind == 0);
            let idle_hidden = seat.settings.visible
                && seat.settings.idle_hidden(
                    seat.last_mouse_activity
                        .elapsed()
                        .as_millis()
                        .min(u128::from(u64::MAX)) as u64,
                    mouse_held,
                );
            let mut effective = seat.settings.clone();
            effective.visible &= !idle_hidden;
            seat.overlay.draw(
                seat.cursor.x + rect.left,
                seat.cursor.y + rect.top,
                &self.seats[i].name,
                &effective,
                self.paused,
            )?;
            seat.overlay.status.idle_hidden = idle_hidden;
            states.push(snapshot_state(
                &self.seats,
                &self.held,
                &self.seat_errors,
                i,
                pid,
                seat,
            )?);
        }
        for user in reposition {
            if let Err(error) = self.position_pointer(user) {
                self.seat_errors[user] = Some(format!("Assigned target unavailable: {error:#}"));
                if let Some(state) = states
                    .iter_mut()
                    .find(|state| state.pid == self.seats[user].target_pid)
                {
                    state.status = self.seat_status(user);
                }
            }
        }
        Ok((stop, states))
    }
}
impl Drop for Router {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_options_require_paired_virtual_desktop_routing() {
        let mut seat = ResolvedSeat {
            name: "virtual".into(),
            mouse: BASE | 2,
            keyboard: Some(BASE | 3),
            target_pid: 10,
        };
        let legacy = CursorSettings::default();
        let mut configured = legacy.clone();
        configured.keyboard = mux_core::keyboard::KeyboardSettings::recommended();
        assert!(validate_keyboard_settings(&configured, &seat, true).is_ok());
        assert!(validate_keyboard_settings(&configured, &seat, false).is_err());
        assert!(validate_keyboard_settings(&legacy, &seat, false).is_ok());
        seat.keyboard = None;
        assert!(validate_keyboard_settings(&configured, &seat, true).is_err());
        seat.keyboard = Some(3);
        assert!(validate_keyboard_settings(&configured, &seat, true).is_err());
        configured.keyboard = legacy.keyboard;
        configured.keyboard.enabled = false;
        assert!(validate_keyboard_settings(&configured, &seat, true).is_err());
        seat.keyboard = Some(BASE | 3);
        seat.mouse = 2;
        assert!(validate_keyboard_settings(&configured, &seat, true).is_err());
        assert!(validate_keyboard_settings(&legacy, &seat, true).is_ok());
    }

    fn held(packet: Packet, user: usize, window: Option<mux_core::routing::Window>) -> Held {
        Held {
            packet,
            user,
            window,
            release_error: None,
            last_release_attempt: None,
        }
    }

    #[test]
    fn failed_native_down_does_not_create_a_held_token() {
        let token = (17, 1);
        let saved = Some(held(
            Packet::button(token.0, 0, false),
            2,
            Some(mux_core::routing::Window {
                hwnd: 0x1234,
                pid: 900,
            }),
        ));
        let mut held_tokens = BTreeMap::new();

        let result = remember_sent_down(
            &mut held_tokens,
            token,
            saved,
            Err(anyhow::anyhow!("native down rejected")),
        );

        assert!(result.is_err());
        assert!(held_tokens.is_empty());
    }

    #[test]
    fn successful_native_down_keeps_its_original_recipient() {
        let token = (17, 1);
        let window = mux_core::routing::Window {
            hwnd: 0x1234,
            pid: 900,
        };
        let saved = Some(held(Packet::button(token.0, 0, false), 2, Some(window)));
        let mut held_tokens = BTreeMap::new();

        remember_sent_down(&mut held_tokens, token, saved, Ok(())).unwrap();

        let retained = held_tokens.get(&token).unwrap();
        assert_eq!(retained.user, 2);
        assert_eq!(retained.window, Some(window));
        assert_eq!(retained.packet.flags, 1);
    }

    #[test]
    fn failed_release_retains_original_owner_and_is_retryable() {
        let token = (23, 0x41);
        let window = mux_core::routing::Window {
            hwnd: 0x5678,
            pid: 901,
        };
        let mut retained = held(Packet::key(token.0, token.1, false), 3, Some(window));
        let attempted = Instant::now();

        remember_release_failure(&mut retained, "window unavailable".into(), attempted);

        assert_eq!(retained.user, 3);
        assert_eq!(retained.window, Some(window));
        assert_eq!(retained.packet.data, token.1);
        assert_eq!(
            retained.release_error.as_deref(),
            Some("window unavailable")
        );
        assert_eq!(retained.last_release_attempt, Some(attempted));
        let up = Router::release(retained.packet);
        assert_eq!(up.device, token.0);
        assert_eq!(up.data, token.1);
        assert_eq!(up.flags & 1, 1);
    }

    #[test]
    fn failed_release_retries_exact_token_and_success_clears_it() {
        let token = (23, 0x41);
        let window = mux_core::routing::Window {
            hwnd: 0x5678,
            pid: 901,
        };
        let mut held_tokens = BTreeMap::from([(
            token,
            held(Packet::key(token.0, token.1, false), 3, Some(window)),
        )]);

        let failed = finish_release(
            &mut held_tokens,
            token,
            Err(anyhow::anyhow!("temporary native failure")),
            true,
        );
        assert!(failed.is_some());
        assert_eq!(held_tokens.get(&token).unwrap().window, Some(window));
        assert!(
            held_tokens
                .get(&token)
                .unwrap()
                .last_release_attempt
                .is_some()
        );

        let retried = finish_release(&mut held_tokens, token, Ok(()), true);

        assert!(retried.is_none());
        assert!(!held_tokens.contains_key(&token));
    }

    #[test]
    fn exited_original_process_retires_tokens_without_a_delivery_attempt() {
        let token = (31, 64);
        let mut held_tokens = BTreeMap::from([(
            token,
            held(
                Packet::button(token.0, 3, false),
                5,
                Some(mux_core::routing::Window {
                    hwnd: 0x9876,
                    pid: 902,
                }),
            ),
        )]);

        assert!(retire_user_tokens_if_exited(&mut held_tokens, 5, false));
        assert!(held_tokens.is_empty());
    }

    #[test]
    fn release_failure_is_downcastable_and_identifies_each_token() {
        let failure = ReleaseFailure {
            failures: vec![ReleaseFailureDetail {
                user: 4,
                device: 29,
                identity: 16,
                detail: "native release rejected".into(),
            }],
        };
        let error = anyhow::Error::new(failure);
        let saved = error.downcast_ref::<ReleaseFailure>().unwrap();
        assert_eq!(saved.failures[0].user, 4);
        assert_eq!(saved.failures[0].device, 29);
        assert_eq!(saved.failures[0].identity, 16);
        assert!(
            error
                .to_string()
                .contains("1 input release(s) remain pending")
        );
    }

    #[test]
    fn release_sweep_attempts_other_users_after_a_failure() {
        let tokens = [(17, 1), (19, 4), (23, 0x41)];
        let mut attempted = Vec::new();

        let failures = attempt_all_releases(tokens, |(device, identity)| {
            attempted.push((device, identity));
            (device == 19).then(|| ReleaseFailureDetail {
                user: 1,
                device,
                identity,
                detail: "temporary failure".into(),
            })
        });

        assert_eq!(attempted, tokens.to_vec());
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].device, 19);
        assert_eq!(failures[0].identity, 4);
    }

    #[test]
    fn resume_attempts_healthy_seats_after_one_seat_is_unavailable() {
        let mut attempted = Vec::new();
        let failures = continue_resume_seats(2, |user| {
            attempted.push(user);
            if user == 0 {
                anyhow::bail!("original target is unavailable");
            }
            Ok(())
        });

        assert_eq!(attempted, vec![0, 1]);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, 0);
        assert!(failures[0].1.to_string().contains("original target"));
    }
}
