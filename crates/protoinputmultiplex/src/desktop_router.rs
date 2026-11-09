//! Rust desktop routing: click-selected windows, process leases, capture and switched ownership.
pub(crate) mod observation;
use crate::router::{State, keyboard_supported, validate_keyboard_settings};
use anyhow::{Context, Result, ensure};
use mux_core::{
    Profile, ResolvedSeat,
    desktop::{Bounds, Cursor, CursorSettings},
    emulated::Packet,
    keyboard::ApplicationFocus,
    routing::{Claim, DesktopRouting, Mode, Policy, Window},
};
use mux_windows::{
    keyboard_focus::{self, RoutingState},
    overlay::{DesktopGuard, Overlay, OverlayStatus},
    protoinput::DesktopBackend,
    targets::{self, Surface},
};
use observation::{Admission, Change, ConfiguredCursor, Journal, LossReason};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

struct ExplicitSelection {
    identity: targets::ValidatedSelection,
    state: crate::window_selection::AttachmentState,
}
struct User {
    observation_uncertain: bool,
    selection: Option<ExplicitSelection>,
    application_target: Option<usize>,
    active: bool,
    last_mouse_activity: Instant,
    cursor: Cursor,
    settings: CursorSettings,
    overlay: Option<Overlay>,
    capture: Option<Surface>,
    gesture: Option<Gesture>,
    status: String,
}
struct Held {
    packet: Packet,
    target: usize,
    window: Window,
    user: usize,
}
fn release_held_token(
    held_inputs: &mut BTreeMap<(u32, u32), Held>,
    token: (u32, u32),
    alive: impl Fn(usize) -> bool,
    mut send_release: impl FnMut(&Held) -> Result<()>,
) -> Result<()> {
    if let Some(held) = held_inputs.get(&token)
        && alive(held.target)
        && !(held.packet.kind == 1
            && held_inputs.iter().any(|(other_token, other)| {
                *other_token != token
                    && other.target == held.target
                    && other.packet.kind == 1
                    && other.packet.data == held.packet.data
            }))
    {
        send_release(held)?;
    }
    // A live process can lose its input HWND before its exit watch signals.
    // Keep failed releases owned so another release or shutdown can retry.
    held_inputs.remove(&token);
    Ok(())
}
struct Gesture {
    identity: u32,
    surface: Surface,
    start: (i32, i32),
    outer: Bounds,
}
pub struct DesktopRouter {
    observation: Journal,
    observation_generation: u64,
    backend: DesktopBackend,
    seats: Vec<ResolvedSeat>,
    seat_ids: Vec<String>,
    users: Vec<User>,
    forwarding_hidden: Vec<bool>,
    forwarded_motion_accepted: bool,
    held: BTreeMap<(u32, u32), Held>,
    policy: Policy,
    keyboard: Vec<Option<Window>>,
    text_recipients: BTreeMap<usize, Option<keyboard_focus::RetainedRecipient>>,
    options: DesktopRouting,
    excluded: Vec<u32>,
    guard: DesktopGuard,
    paused: bool,
    strict_mapper_delivery: bool,
    user_pauses: Vec<crate::routing_engine::UserPauseState>,
    user_pause_retry: Vec<Option<Instant>>,
    pause_virtual_users: Vec<bool>,
    failed: BTreeMap<u32, String>,
    shutdown_started: bool,
    shutdown_error: Option<String>,
}
// A failed queued move can withhold this user's new discrete action, but may
// never suppress a release or another user's input after the queue was drained.
fn motion_failure_blocks_input(packet: Packet, affected: bool) -> bool {
    let release = (packet.kind == 1 && packet.flags & 1 != 0)
        || (packet.kind == 0 && matches!(packet.flags, 2 | 8 | 32 | 128 | 512));
    affected && !release
}
fn pause_reserves_owner(paused: bool, held: bool, gesture: bool) -> bool {
    paused || held || gesture
}
impl DesktopRouter {
    pub fn mapper_delivery(&mut self, active: bool) -> Result<()> {
        self.strict_mapper_delivery = active;
        Ok(())
    }
    pub fn begin_text_delivery(&mut self, user: usize) -> Result<()> {
        ensure!(
            !self.text_recipients.contains_key(&user),
            "Nested Text delivery"
        );
        ensure!(
            self.users.get(user).is_some_and(|user| user.active),
            "Unknown or removed Text user"
        );
        let recipient =
            if self.paused || self.user_pause_state(user).paused || !self.keyboard_enabled(user) {
                None
            } else {
                let clicked = self
                    .keyboard_base(user)?
                    .context("Select a window before sending text")?;
                Some(keyboard_focus::RetainedRecipient::capture(
                    clicked,
                    self.users[user].settings.keyboard.focus,
                )?)
            };
        self.text_recipients.insert(user, recipient);
        Ok(())
    }
    pub fn end_text_delivery(&mut self, user: usize) {
        self.text_recipients.remove(&user);
    }
    fn selection_unbound(&self, user: usize) -> bool {
        self.users[user]
            .selection
            .as_ref()
            .is_some_and(|selection| selection.state.unbound())
    }
    pub fn validate_mode_change(&self, multi_keyboard: bool) -> Result<()> {
        crate::routing_engine::validate_user_pause_mode_change(&self.user_pauses)?;
        crate::window_selection::validate_keyboard_mode(
            multi_keyboard,
            self.users.iter().filter_map(|user| {
                user.selection
                    .as_ref()
                    .map(|selection| (user.active, selection.state))
            }),
        )
    }
    fn input_point(&self, user: usize) -> Result<(i32, i32)> {
        let source = self.users.get(user).context("Unknown user")?;
        let point = (source.cursor.x, source.cursor.y);
        if let Some(selection) = &source.selection {
            let surface = selection.identity.revalidate()?;
            if selection.state.unbound() {
                crate::window_selection::require_independent_keyboards(
                    self.options.multi_keyboard,
                )?;
                return Ok(selection
                    .state
                    .prospective_point(point, self.selection_bounds(user, &surface)?));
            }
        }
        Ok(point)
    }
    pub fn pending_attachment_window(&self, user: usize) -> Result<Option<Window>> {
        ensure!(self.policy.is_active(user), "Unknown or removed user");
        if self.selection_unbound(user) {
            self.input_point(user)?;
            return Ok(self.users[user]
                .selection
                .as_ref()
                .map(|selection| selection.identity.window()));
        }
        Ok(None)
    }
    fn selection_bounds(&self, user: usize, surface: &Surface) -> Result<Bounds> {
        crate::window_selection::intersect_client(
            surface.client,
            mux_windows::monitors::resolve(&self.users[user].settings, Some(surface.client))?,
        )
    }
    pub fn select_window(&mut self, selection: &crate::window_selection::Selection) -> Result<()> {
        let user = selection.user;
        crate::window_selection::require_independent_keyboards(self.options.multi_keyboard)?;
        ensure!(
            !self.paused && !self.shutdown_started,
            "Resume routing before selecting a window"
        );
        ensure!(self.policy.is_active(user), "Unknown or removed user");
        let seat = &self.seats[user];
        ensure!(
            seat.mouse & mux_core::emulated::MASK == mux_core::emulated::BASE
                && seat
                    .keyboard
                    .is_none_or(|k| k & mux_core::emulated::MASK == mux_core::emulated::BASE),
            "Window selection requires virtual devices"
        );
        ensure!(
            !self.excluded.contains(&selection.pid),
            "Cannot select the controller or routing worker"
        );
        ensure!(
            self.options.allowed_pids.is_empty()
                || self.options.allowed_pids.contains(&selection.pid),
            "Selected PID is outside this session's allowed applications"
        );
        ensure!(
            !self.held.values().any(|held| held.user == user)
                && self.users[user].capture.is_none()
                && self.users[user].gesture.is_none()
                && !self.backend.has_pending_motion(seat.mouse)?,
            "Release input and wait for motion before selecting a window"
        );
        let identity = targets::ValidatedSelection::new(
            selection.pid,
            selection.hwnd,
            &selection.creation_token,
        )?;
        let surface = identity.revalidate()?;
        self.selection_bounds(user, &surface)?;
        ensure!(
            self.policy.can_select(user, surface.window),
            "This application or switched root belongs to another user"
        );
        if let Some(error) = self.failed.get(&selection.pid) {
            anyhow::bail!("{error}");
        }
        self.backend.validate_application_focus(
            selection.pid,
            self.users[user].settings.keyboard.application_focus,
        )?;
        let cached = self
            .backend
            .validate_cached_creation(selection.pid, identity.creation())?;
        if cached.is_none() {
            let ready = identity.eligibility()?;
            ensure!(ready.can_start(true), "{}", ready.reason);
        }
        // Revalidate after all pure preflights, before an infallible policy commit.
        let current = identity.revalidate()?;
        self.selection_bounds(user, &current)?;
        let committed = self.policy.select(user, surface.window);
        debug_assert!(committed);
        self.keyboard[user] = Some(surface.window);
        self.users[user].selection = Some(ExplicitSelection {
            identity,
            state: crate::window_selection::AttachmentState::Selected,
        });
        self.users[user].status = "Selected; ready for first routed input".into();
        Ok(())
    }
    /// Called only after a real, non-filtered action reaches the leaf router.
    fn prepare_selected_input(&mut self, user: usize) -> Result<()> {
        let before = (self.users[user].cursor.x, self.users[user].cursor.y);
        let result = self.prepare_selected_input_inner(user);
        if result.is_err() {
            self.observation_uncertain(user);
        } else if before != (self.users[user].cursor.x, self.users[user].cursor.y) {
            self.observation_motion(user);
        }
        result
    }
    fn prepare_selected_input_inner(&mut self, user: usize) -> Result<()> {
        let Some(selection) = self.users[user].selection.as_ref() else {
            return Ok(());
        };
        let surface = selection.identity.revalidate()?;
        if !selection.state.unbound() {
            return Ok(());
        }
        ensure!(
            self.options.multi_keyboard,
            "Independent keyboards are required for explicit selection"
        );
        let bounds = self.selection_bounds(user, &surface)?;
        let pid = selection.identity.window().pid;
        let creation = selection.identity.creation();
        self.backend.validate_application_focus(
            pid,
            self.users[user].settings.keyboard.application_focus,
        )?;
        let cached = self.backend.validate_cached_creation(pid, creation)?;
        if cached.is_none() {
            if let Some(error) = self.failed.get(&pid) {
                anyhow::bail!("{error}");
            }
            crate::session::emit(crate::session::Event::Attaching(pid))?;
        }
        let result = self.backend.open_expected_with_focus(
            pid,
            creation,
            self.users[user].settings.keyboard.application_focus,
        );
        if cached.is_none() {
            let error = result.as_ref().err().map(|error| format!("{error:#}"));
            if let Some(error) = &error {
                self.failed.insert(pid, error.clone());
            }
            crate::session::emit(crate::session::Event::AttachmentFinished { pid, error })?;
        }
        let mut state = selection.state;
        state.on_real_input(|| result.map(|_| ()))?;
        // No clamp until original-identity attachment has succeeded.
        let cursor = &mut self.users[user].cursor;
        *cursor = Cursor::new(
            cursor.x.clamp(bounds.left, bounds.right - 1),
            cursor.y.clamp(bounds.top, bounds.bottom - 1),
        );
        self.users[user].selection.as_mut().unwrap().state = state;
        self.users[user].status = format!("Attached · {}", surface.title);
        Ok(())
    }

    fn report_motion_failure(&mut self, error: &anyhow::Error) -> bool {
        let Some(publication) =
            error.downcast_ref::<mux_windows::protoinput::MotionPublicationError>()
        else {
            return false;
        };
        for failure in &publication.failures {
            if let Some(user) = self
                .seats
                .iter()
                .position(|seat| seat.mouse == failure.device)
            {
                self.users[user].status = format!("Motion publication unavailable: {error:#}");
                self.observation_uncertain(user);
            }
        }
        true
    }
    pub fn poll_motion(&mut self) -> Result<()> {
        if let Err(error) = self.backend.poll_motion() {
            // A departed target must not stop unrelated automatic users. Failed
            // publications are discarded by the backend and remain visible here.
            if !self.report_motion_failure(&error) {
                self.observation_uncertain_all();
                return Err(error);
            }
        }
        Ok(())
    }
    pub fn motion_deadline(&self) -> Option<Instant> {
        self.backend.motion_deadline()
    }
    pub fn note_mouse_activity(&mut self, user: usize) -> Result<()> {
        let user = self.users.get_mut(user).context("Unknown cursor user")?;
        ensure!(user.active, "virtual user has been removed");
        user.last_mouse_activity = Instant::now();
        Ok(())
    }
    pub fn pointer(&self, user: usize) -> Result<(i32, i32)> {
        let user = self.users.get(user).context("Unknown user")?;
        ensure!(user.active, "virtual user has been removed");
        Ok((user.cursor.x, user.cursor.y))
    }
    pub fn selected_window(&self, user: usize) -> Result<Option<Window>> {
        ensure!(self.policy.is_active(user), "virtual user has been removed");
        if let Some(selection) = &self.users.get(user).context("Unknown user")?.selection {
            selection.identity.revalidate()?;
        }
        let window = *self.policy.focus.get(user).context("Unknown user")?;
        if let Some(window) = window {
            targets::surface(window)?;
        }
        Ok(window)
    }
    pub fn mouse_window(&self, user: usize) -> Result<Option<Window>> {
        ensure!(self.policy.is_active(user), "virtual user has been removed");
        if let Some(capture) = &self.users[user].capture {
            targets::surface(capture.window)?;
            return Ok(Some(capture.window));
        }
        let (x, y) = self.input_point(user)?;
        Ok(
            targets::at_point(x, y, &self.excluded, &self.options.allowed_pids)?
                .map(|surface| surface.window),
        )
    }
    pub fn keyboard_window(&self, user: usize) -> Result<Option<Window>> {
        if let Some(Some(recipient)) = self.text_recipients.get(&user) {
            return recipient.validate().map(Some);
        }
        self.keyboard_base(user)?
            .map(|clicked| {
                keyboard_focus::resolve(clicked, self.users[user].settings.keyboard.focus)
                    .map(|resolution| resolution.resolved)
            })
            .transpose()
    }
    fn keyboard_base(&self, user: usize) -> Result<Option<Window>> {
        ensure!(self.policy.is_active(user), "virtual user has been removed");
        let owner = if self.options.multi_keyboard {
            user
        } else {
            self.policy.active.unwrap_or(user)
        };
        if let Some(selection) = &self.users[owner].selection {
            selection.identity.revalidate()?;
        }
        let window = *self.keyboard.get(owner).context("Unknown keyboard owner")?;
        Ok(window)
    }
    pub fn keyboard_enabled(&self, user: usize) -> bool {
        self.users
            .get(user)
            .is_some_and(|source| source.active && source.settings.keyboard.enabled)
    }
    fn keyboard_state(&self, user: usize) -> Option<RoutingState> {
        keyboard_supported(&self.seats[user], true).then(|| {
            // Resolve failures are local diagnostic state and cannot stop another seat.
            let clicked = self.keyboard_base(user);
            match clicked {
                Ok(clicked) => RoutingState::observe(self.users[user].settings.keyboard, clicked),
                Err(error) => RoutingState {
                    settings: self.users[user].settings.keyboard,
                    clicked: None,
                    resolved: None,
                    fallback: false,
                    status: format!("Keyboard recipient unavailable: {error:#}"),
                },
            }
        })
    }
    pub fn movement_window(&self, user: usize, x: i32, y: i32) -> Result<Option<Window>> {
        ensure!(self.policy.is_active(user), "virtual user has been removed");
        if let Some(gesture) = &self.users[user].gesture {
            return Ok(Some(gesture.surface.window));
        }
        if let Some(capture) = &self.users[user].capture {
            targets::surface(capture.window)?;
            return Ok(Some(capture.window));
        }
        // Desktop mouse motion is delivered to the user's current policy focus;
        // moving the cursor over another window does not retarget that motion.
        // A later button-down is independently checked through mouse_window().
        let _ = (x, y);
        if self.options.mode == Mode::Switched && self.policy.root.is_some_and(|root| root != user)
        {
            return Ok(None);
        }
        let window = *self.policy.focus.get(user).context("Unknown user")?;
        if let Some(window) = window {
            targets::surface(window)?;
        }
        Ok(window)
    }
    pub fn packet_window(&self, user: usize, packet: Packet) -> Result<Option<Window>> {
        ensure!(self.policy.is_active(user), "virtual user has been removed");
        if packet.kind != 0 || packet.flags != 0 {
            return if packet.kind == 0 {
                self.mouse_window(user)
            } else {
                self.keyboard_window(user)
            };
        }
        if let Some(gesture) = &self.users[user].gesture {
            return Ok(Some(gesture.surface.window));
        }
        if let Some(capture) = &self.users[user].capture {
            targets::surface(capture.window)?;
            return Ok(Some(capture.window));
        }
        let mut cursor = self.users[user].cursor.clone();
        cursor.motion(
            packet.x,
            packet.y,
            &self.users[user].settings,
            self.motion_bounds(user)?,
        );
        self.movement_window(user, cursor.x, cursor.y)
    }
    pub fn new(
        backend: DesktopBackend,
        profile: &Profile,
        seats: Vec<ResolvedSeat>,
        controller: u32,
        guard: DesktopGuard,
    ) -> Result<Self> {
        ensure!(
            seats.len() == profile.seats.len(),
            "resolved seat count does not match the profile"
        );
        let options = profile.desktop.clone().context("desktop options missing")?;
        for (user, seat) in seats.iter().enumerate() {
            validate_keyboard_settings(&profile.seats[user].cursor, seat, true)?;
            backend.validate_application_focus(
                profile.seats[user].target_pid,
                profile.seats[user].cursor.keyboard.application_focus,
            )?;
        }
        let desktop = targets::desktop_bounds();
        let mut users = Vec::new();
        let windows = targets::windows()?;
        for (i, seat) in profile.seats.iter().enumerate() {
            let initial = seat
                .desktop_start
                .or_else(|| {
                    windows
                        .iter()
                        .find(|w| w.pid == seat.target_pid)
                        .and_then(|w| targets::client_bounds(w.hwnd, w.pid).ok())
                        .map(|r| [(r.left + r.right) / 2, (r.top + r.bottom) / 2])
                })
                .unwrap_or([
                    (desktop.left + desktop.right) / 2 + i as i32 * 32,
                    (desktop.top + desktop.bottom) / 2,
                ]);
            let mut cursor = Cursor::new(initial[0], initial[1]);
            cursor.motion(
                0,
                0,
                &CursorSettings::default(),
                mux_windows::monitors::resolve(&seat.cursor, None)?,
            );
            users.push(User {
                observation_uncertain: false,
                selection: None,
                application_target: None,
                last_mouse_activity: Instant::now(),
                active: true,
                cursor,
                settings: seat.cursor.clone(),
                overlay: Some(Overlay::new(&seat.name)?),
                capture: None,
                gesture: None,
                status: "Click a window to route input".into(),
            });
        }
        let count = seats.len();
        let pause_virtual_users = profile
            .seats
            .iter()
            .enumerate()
            .map(|(user, seat)| -> Result<_> {
                Ok(crate::routing_engine::virtual_pause_devices(seat)?
                    && seats
                        .iter()
                        .filter(|other| {
                            other.mouse == seats[user].mouse
                                || other.keyboard == seats[user].keyboard
                        })
                        .count()
                        == 1)
            })
            .collect::<Result<Vec<_>>>()?;
        let user_pauses = pause_virtual_users.iter().enumerate().map(|(user, virtual_pair)| {
            if *virtual_pair && crate::routing_engine::automatic_pause_supported(options.mode, options.multi_keyboard) {
                crate::routing_engine::UserPauseState { user, supported: true, ..Default::default() }
            } else {
                crate::routing_engine::UserPauseState::unsupported(user, "Per-user Pause requires concurrent automatic routing with independent paired virtual keyboards")
            }
        }).collect();
        Ok(Self {
            observation: Journal::default(),
            observation_generation: std::env::var(observation::GENERATION_ENV)
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0),
            backend,
            seats,
            seat_ids: profile.seats.iter().map(mux_core::Seat::identity).collect(),
            users,
            forwarding_hidden: vec![false; count],
            forwarded_motion_accepted: false,
            held: BTreeMap::new(),
            policy: Policy::new(count, options.mode),
            keyboard: vec![None; count],
            text_recipients: BTreeMap::new(),
            options,
            excluded: vec![controller, std::process::id()],
            guard,
            paused: false,
            strict_mapper_delivery: false,
            user_pauses,
            user_pause_retry: vec![None; count],
            pause_virtual_users,
            failed: BTreeMap::new(),
            shutdown_started: false,
            shutdown_error: None,
        })
    }
    pub fn paused(&self) -> bool {
        self.paused
    }
    pub fn set_forwarding_sources(&mut self, hidden: &[bool]) -> Result<()> {
        ensure!(
            hidden.len() == self.users.len(),
            "forwarding visibility mask does not match users"
        );
        ensure!(
            hidden
                .iter()
                .enumerate()
                .all(|(user, hidden)| !*hidden || self.users[user].active),
            "removed user cannot forward input"
        );
        self.forwarding_hidden.copy_from_slice(hidden);
        Ok(())
    }
    /// Appends an explicitly assigned virtual seat. Existing runtime indices never move.
    pub fn add_virtual_user(
        &mut self,
        seat: &mux_core::Seat,
        resolved: ResolvedSeat,
    ) -> Result<usize> {
        let pause_supported = crate::routing_engine::virtual_pause_devices(seat)?;
        validate_keyboard_settings(&seat.cursor, &resolved, true)?;
        self.backend
            .validate_application_focus(seat.target_pid, seat.cursor.keyboard.application_focus)?;
        self.observation_flush_motion()?;
        ensure!(
            !self.paused,
            "resume the desktop session before adding a user"
        );
        ensure!(
            seat.target_pid != 0,
            "a live virtual user needs an explicit target PID"
        );
        ensure!(
            resolved.target_pid == seat.target_pid,
            "resolved target PID does not match seat"
        );
        ensure!(
            resolved.name == seat.name,
            "resolved name does not match seat"
        );
        ensure!(
            self.options.allowed_pids.is_empty()
                || self.options.allowed_pids.contains(&seat.target_pid),
            "target PID is outside the desktop routing allowlist"
        );
        ensure!(
            !self.excluded.contains(&seat.target_pid),
            "cannot target the controller or routing worker"
        );
        ensure!(
            !self
                .users
                .iter()
                .zip(&self.seats)
                .any(|(user, existing)| { user.active && existing.target_pid == seat.target_pid }),
            "another active virtual user is assigned to this target PID"
        );
        let seat_id = seat.identity();
        ensure!(
            !self.seat_ids.iter().any(|id| id == &seat_id),
            "seat identity has already been used in this session"
        );
        ensure!(
            resolved.mouse & mux_core::emulated::MASK == mux_core::emulated::BASE
                && resolved.keyboard.is_none_or(|key| {
                    key & mux_core::emulated::MASK == mux_core::emulated::BASE
                }),
            "live virtual users require virtual device IDs"
        );
        ensure!(
            resolved.keyboard != Some(resolved.mouse),
            "mouse and keyboard device IDs must differ"
        );
        ensure!(
            self.seats.iter().all(|existing| {
                existing.mouse != resolved.mouse
                    && existing.keyboard != Some(resolved.mouse)
                    && resolved.keyboard != Some(existing.mouse)
                    && resolved
                        .keyboard
                        .is_none_or(|key| existing.keyboard != Some(key))
            }),
            "virtual device ID has already been used in this session"
        );
        ensure!(
            self.policy.owner(seat.target_pid).is_none(),
            "target PID is currently owned by another virtual user"
        );
        seat.cursor.validate()?;

        let target_window = targets::windows()?
            .into_iter()
            .find(|window| window.pid == seat.target_pid)
            .context("assigned target has no visible application window")?;
        let window = Window {
            hwnd: target_window.hwnd,
            pid: seat.target_pid,
        };
        let assigned_surface = targets::surface(window)?;
        ensure!(
            assigned_surface.client.valid(),
            "assigned target client bounds are invalid"
        );
        let cursor_bounds =
            mux_windows::monitors::resolve(&seat.cursor, Some(assigned_surface.client))?;
        let initial_bounds = assigned_surface
            .client
            .intersection(cursor_bounds)
            .context("cursor bounds do not intersect the assigned target")?;
        let initial = seat.desktop_start.unwrap_or([
            ((i64::from(assigned_surface.client.left) + i64::from(assigned_surface.client.right))
                / 2) as i32,
            ((i64::from(assigned_surface.client.top) + i64::from(assigned_surface.client.bottom))
                / 2) as i32,
        ]);
        let x = initial[0].clamp(initial_bounds.left, initial_bounds.right - 1);
        let y = initial[1].clamp(initial_bounds.top, initial_bounds.bottom - 1);
        let target_surface = targets::at_point(x, y, &self.excluded, &[seat.target_pid])?
            .filter(|surface| surface.window.pid == seat.target_pid)
            .unwrap_or(assigned_surface);
        let focused_window =
            targets::child_at_point(target_surface.window, x, y).unwrap_or(target_surface.window);
        let surface = targets::surface(focused_window)?;

        // Construct fallible local UI/native resources and attach the explicit target before
        // changing the routing vectors. Device registration itself is a void ProtoInput API.
        let overlay = Overlay::new(&seat.name)?;
        let target_index = self.open(seat.target_pid, seat.cursor.keyboard.application_focus)?;
        let user = self.users.len();
        self.backend.register_virtual_user(resolved.clone())?;

        // Position before publishing the slot so a target exit cannot leave a live
        // router user behind. ProtoInput has no unregister export, so the ID stays reserved.
        self.backend.position(
            target_index,
            surface.window.hwnd,
            resolved.mouse,
            x,
            y,
            surface.client,
        )?;

        let mut cursor = Cursor::new(x, y);
        cursor.motion(0, 0, &CursorSettings::default(), cursor_bounds);
        self.seats.push(resolved.clone());
        self.seat_ids.push(seat_id);
        self.users.push(User {
            observation_uncertain: false,
            selection: None,
            application_target: None,
            last_mouse_activity: Instant::now(),
            active: true,
            cursor,
            settings: seat.cursor.clone(),
            overlay: Some(overlay),
            capture: None,
            gesture: None,
            status: "Assigned target ready".into(),
        });
        self.forwarding_hidden.push(false);
        self.pause_virtual_users.push(pause_supported);
        self.user_pauses.push(if pause_supported && crate::routing_engine::automatic_pause_supported(self.options.mode, self.options.multi_keyboard) {
            crate::routing_engine::UserPauseState { user, supported: true, ..Default::default() }
        } else { crate::routing_engine::UserPauseState::unsupported(user, "Per-user Pause requires concurrent automatic routing with independent paired virtual keyboards") });
        self.user_pause_retry.push(None);
        self.keyboard.push(Some(surface.window));
        let policy_user = self.policy.append_user();
        ensure!(policy_user == user, "runtime user slots are out of sync");
        ensure!(
            self.policy.set_initial_focus(user, surface.window),
            "target ownership changed during virtual user creation"
        );

        Ok(user)
    }

    pub fn prepare_remove_virtual_user(&mut self, user: usize) -> Result<()> {
        ensure!(
            self.policy.is_active(user),
            "virtual user is already removed or unknown"
        );
        self.observation_flush_motion()?;
        self.release_user(user)
    }

    /// Native registrations stay reserved until worker exit.
    pub fn commit_remove_virtual_user(&mut self, user: usize) {
        self.observation.fence_user(user, LossReason::Removed);
        if let Some(target) = self.users[user].application_target.take() {
            self.backend.cursor_observation(target, 0, false);
        }
        self.keyboard[user] = None;
        self.users[user].selection = None;
        self.users[user].active = false;
        self.forwarding_hidden[user] = false;
        self.users[user].overlay.take();
        self.users[user].status = "Virtual user removed".into();
        self.policy.deactivate_user(user);
        self.user_pauses[user] =
            crate::routing_engine::UserPauseState::unsupported(user, "Unknown or removed user");
        self.user_pause_retry[user] = None;
        self.pause_virtual_users[user] = false;
    }
    pub fn key_held(&self, device: u32, key: u32) -> bool {
        self.held.contains_key(&(device, key))
    }
    pub fn button_held(&self, device: u32, button: u32) -> bool {
        if button >= 5 {
            return false;
        }
        let identity = 1 << (button * 2);
        self.held.contains_key(&(device, identity))
            || self
                .seats
                .iter()
                .position(|s| s.mouse == device)
                .is_some_and(|user| {
                    self.users[user].active && {
                        self.users[user]
                            .gesture
                            .as_ref()
                            .is_some_and(|g| g.identity == identity)
                    }
                })
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
            .and_then(|user| self.users.get(user))
            .filter(|user| user.active)
            .is_some_and(|user| user.settings.transform_button_flags(packet.flags).is_none())
    }
    fn user(&self, packet: Packet) -> Result<usize> {
        self.seats
            .iter()
            .position(|s| {
                if packet.kind == 0 {
                    s.mouse == packet.device
                } else {
                    s.keyboard == Some(packet.device)
                }
            })
            .filter(|user| self.users[*user].active)
            .context("unassigned or retired virtual device")
    }
    fn hit(&self, user: usize) -> Result<Option<Surface>> {
        targets::at_point(
            self.users[user].cursor.x,
            self.users[user].cursor.y,
            &self.excluded,
            &self.options.allowed_pids,
        )
    }
    fn open(&mut self, pid: u32, focus: ApplicationFocus) -> Result<usize> {
        self.backend.validate_application_focus(pid, focus)?;
        if let Some(index) = self.backend.index(pid) {
            return Ok(index);
        }
        if let Some(error) = self.failed.get(&pid) {
            anyhow::bail!("{error}");
        }
        crate::session::emit(crate::session::Event::Attaching(pid))?;
        let result = self.backend.open_with_focus(pid, focus);
        let error = result.as_ref().err().map(|error| format!("{error:#}"));
        if let Some(error) = &error {
            self.failed.insert(pid, error.clone());
        }
        crate::session::emit(crate::session::Event::AttachmentFinished { pid, error })?;
        result
    }
    fn claim(&mut self, user: usize, surface: &Surface, click: bool) -> Result<bool> {
        self.backend.validate_application_focus(
            surface.window.pid,
            self.users[user].settings.keyboard.application_focus,
        )?;
        let held: Vec<bool> = (0..self.users.len())
            .map(|owner| {
                pause_reserves_owner(
                    self.user_pauses[owner].paused,
                    self.held
                        .values()
                        .any(|h| h.user == owner && h.window.pid == surface.window.pid),
                    self.users[owner]
                        .gesture
                        .as_ref()
                        .is_some_and(|g| g.surface.window.pid == surface.window.pid),
                )
            })
            .collect();
        match self.policy.claim(user, surface.window, &held, click) {
            Claim::Busy(owner) => {
                self.users[user].status =
                    format!("Waiting for {} to release input", self.seats[owner].name);
                Ok(false)
            }
            Claim::Granted { evicted } => {
                if let Some(owner) = evicted {
                    if self.users[owner]
                        .selection
                        .as_ref()
                        .is_some_and(|selection| {
                            selection.identity.window().pid == surface.window.pid
                        })
                    {
                        self.users[owner].selection = None;
                    }
                    if self.keyboard[owner].is_some_and(|w| w.pid == surface.window.pid) {
                        self.keyboard[owner] = None;
                    }
                    self.users[owner].status =
                        format!("{} took this application", self.seats[user].name);
                }
                Ok(true)
            }
        }
    }
    fn position(&mut self, user: usize, surface: &Surface) -> Result<usize> {
        let result = self.position_inner(user, surface);
        if result.is_err() {
            self.observation_uncertain(user);
        }
        result
    }
    fn position_inner(&mut self, user: usize, surface: &Surface) -> Result<usize> {
        ensure!(
            !self.user_pauses[user].paused,
            "Paused user cannot publish cursor input"
        );
        let target = self.open(
            surface.window.pid,
            self.users[user].settings.keyboard.application_focus,
        )?;
        self.backend.position(
            target,
            surface.window.hwnd,
            self.seats[user].mouse,
            self.users[user].cursor.x,
            self.users[user].cursor.y,
            surface.client,
        )?;
        if let Some(selection) = &mut self.users[user].selection
            && selection.identity.window() == surface.window
        {
            selection.state.positioned();
        }
        Ok(target)
    }
    fn button_identity(packet: Packet) -> u32 {
        match packet.flags {
            1 | 2 => 1,
            4 | 8 => 4,
            16 | 32 => 16,
            64 | 128 => 64,
            256 | 512 => 256,
            _ => 0,
        }
    }
    fn release_packet(mut p: Packet) -> Packet {
        if p.kind == 1 {
            p.flags |= 1;
        } else {
            p.flags *= 2;
        }
        p
    }
    fn release_token(&mut self, token: (u32, u32)) -> Result<()> {
        let observed_user = self.held.get(&token).map(|held| held.user);
        let shared_release = self.held.get(&token).is_some_and(|held| {
            held.packet.kind == 1
                && self.held.iter().any(|(other_token, other)| {
                    *other_token != token
                        && other.target == held.target
                        && other.packet.kind == 1
                        && other.packet.data == held.packet.data
                })
        });
        let before = observed_user.and_then(|user| self.observation_before(user));
        let mut submitted = false;
        let backend = &self.backend;
        let without_motion = self
            .held
            .get(&token)
            .is_some_and(|held| self.user_pauses[held.user].paused);
        let result = release_held_token(
            &mut self.held,
            token,
            |target| backend.alive(target),
            |held| {
                let result = if without_motion {
                    backend.release_to_without_motion(
                        held.target,
                        held.window.hwnd,
                        Self::release_packet(held.packet),
                    )
                } else {
                    backend.release_to(
                        held.target,
                        held.window.hwnd,
                        Self::release_packet(held.packet),
                    )
                };
                if result.is_ok() {
                    submitted = true;
                }
                result
            },
        );
        if let Some(user) = observed_user {
            if result.is_err() {
                self.observation_uncertain(user);
            } else if let Some(before) = before {
                self.observation_hold_changes(
                    user,
                    before,
                    if submitted {
                        Admission::NativeSubmissionAccepted
                    } else if shared_release {
                        Admission::SharedNativeHold
                    } else {
                        Admission::LogicalOnly
                    },
                    false,
                );
            }
        }
        result
    }
    fn mouse_held(&self, user: usize) -> bool {
        self.held
            .values()
            .any(|h| h.user == user && h.packet.kind == 0)
    }
    fn release_user(&mut self, user: usize) -> Result<()> {
        ensure!(user < self.users.len(), "unknown user");
        let tokens: Vec<_> = self
            .held
            .iter()
            .filter(|(_, h)| h.user == user)
            .map(|(k, _)| *k)
            .collect();
        let mut result = Ok(());
        for token in tokens {
            let released = self.release_token(token);
            if result.is_ok() {
                result = released;
            }
        }
        self.users[user].capture = None;
        self.users[user].gesture = None;
        self.policy.release(user);
        result
    }
    pub fn user_pause_state(&self, user: usize) -> crate::routing_engine::UserPauseState {
        let Some(state) = self
            .user_pauses
            .get(user)
            .filter(|_| self.policy.is_active(user))
        else {
            return crate::routing_engine::UserPauseState::unsupported(
                user,
                "Unknown or removed user",
            );
        };
        let mut state = state.clone();
        state.pending_releases = if state.paused {
            self.held.values().filter(|held| held.user == user).count()
        } else {
            0
        };
        state
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
        let discarded = self
            .backend
            .discard_user_motion(self.seats[user].mouse)
            .map(|_| ());
        let released = self.release_user(user);
        let result = discarded.and(released);
        if let Err(error) = &result {
            self.observation_uncertain(user);
            self.user_pauses[user].error = Some(format!("{error:#}"));
        }
        result
    }
    fn retry_user_pause_releases(&mut self) {
        for user in 0..self.users.len() {
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
    fn idle_status(&mut self, user: usize) {
        if let Some(selection) = &self.users[user].selection {
            self.users[user].status = if selection.state.unbound() {
                "Selected; ready for first routed input"
            } else {
                "Attached"
            }
            .into();
            return;
        }

        self.users[user].status = self.policy.focus[user]
            .and_then(|window| targets::surface(window).ok())
            .map_or_else(
                || "Click a window to route input".into(),
                |surface| format!("{:?} · {}", self.policy.mode, surface.title),
            );
    }
    pub fn release_all(&mut self) -> Result<()> {
        let mut result = self.observation_flush_motion();
        for user in 0..self.users.len() {
            let released = self.release_user(user);
            if result.is_ok() {
                result = released;
            }
        }
        result
    }
    pub fn shutdown(&mut self) -> Result<()> {
        self.observation.fence(LossReason::StopIntent);
        if self.shutdown_started {
            return self
                .shutdown_error
                .as_ref()
                .map_or(Ok(()), |error| Err(anyhow::anyhow!("{error}")));
        }
        self.shutdown_started = true;
        let released = self.release_all();
        let drained = self.backend.shutdown();
        let result = released.and(drained.map(|_| ()));
        self.shutdown_error = result.as_ref().err().map(|error| format!("{error:#}"));
        result
    }
    pub fn pause(&mut self, paused: bool) -> Result<()> {
        self.observation_flush_motion()?;
        if paused {
            self.release_all()?;
        }
        let result = self.backend.pause(paused);
        if result.is_err() {
            self.observation_uncertain_all();
        }
        result?;
        self.paused = paused;
        if !paused {
            for user in 0..self.users.len() {
                if self.users[user].active && !self.user_pauses[user].paused {
                    self.motion(user)?;
                }
            }
        }
        Ok(())
    }
    pub fn settings(&mut self, index: usize, settings: CursorSettings) -> Result<()> {
        settings.validate()?;
        ensure!(
            self.policy.is_active(index),
            "virtual user has been removed or is unknown"
        );
        let seat = self.seats.get(index).context("unknown user")?;
        validate_keyboard_settings(&settings, seat, true)?;
        settings
            .keyboard
            .validate_live_update(self.users[index].settings.keyboard)?;
        ensure!(
            settings.horizontal_wheel_emulation
                == mux_core::desktop::HorizontalWheelEmulation::Never
                || seat.mouse & mux_core::emulated::MASK == mux_core::emulated::BASE,
            "horizontal wheel emulation currently requires a virtual mouse"
        );
        let window = self.policy.focus[index]
            .and_then(|w| targets::surface(w).ok())
            .map(|s| s.client);
        let bounds = mux_windows::monitors::resolve(&settings, window)?;
        self.observation_flush_motion()?;
        self.release_user(index)?;
        let user = self.users.get_mut(index).context("unknown user")?;
        user.settings = settings;
        user.last_mouse_activity = Instant::now();
        if !user.selection.as_ref().is_some_and(|s| s.state.unbound()) {
            user.cursor = Cursor::new(
                user.cursor.x.clamp(bounds.left, bounds.right - 1),
                user.cursor.y.clamp(bounds.top, bounds.bottom - 1),
            );
        }
        self.observation_motion(index);
        self.observation_record(
            index,
            Change::Cursor {
                cursor: ConfiguredCursor::from(&self.users[index].settings),
            },
            Admission::LogicalOnly,
        );
        if !self.paused && !self.user_pauses[index].paused {
            self.motion(index)?;
        }
        Ok(())
    }
    pub fn mode(&mut self, mode: Mode, multi_keyboard: bool) -> Result<()> {
        self.validate_mode_change(multi_keyboard)?;
        self.release_all()?;
        self.policy.mode(mode);
        self.options.mode = mode;
        self.options.multi_keyboard = multi_keyboard;
        for (user, state) in self.user_pauses.iter_mut().enumerate() {
            *state = if self.pause_virtual_users[user]
                && crate::routing_engine::automatic_pause_supported(mode, multi_keyboard)
            {
                crate::routing_engine::UserPauseState {
                    user,
                    supported: true,
                    ..Default::default()
                }
            } else {
                crate::routing_engine::UserPauseState::unsupported(
                    user,
                    "Per-user Pause requires concurrent automatic routing with independent paired virtual keyboards",
                )
            };
        }
        for user in 0..self.users.len() {
            if self.users[user].active {
                self.idle_status(user);
            }
        }
        Ok(())
    }
    pub fn move_to(&mut self, user: usize, x: i32, y: i32) -> Result<()> {
        ensure!(
            self.policy.is_active(user),
            "virtual user has been removed or is unknown"
        );
        if self.paused || self.user_pauses[user].paused {
            return Ok(());
        }
        if self.selection_unbound(user)
            && (x, y) == (self.users[user].cursor.x, self.users[user].cursor.y)
        {
            return Ok(());
        }
        if let Err(error) = self.prepare_selected_input(user) {
            self.users[user].status = format!("Routing unavailable: {error:#}");
            if self.strict_mapper_delivery {
                return Err(error);
            }
            return Ok(());
        }
        self.observation_flush_motion()?;
        self.note_mouse_activity(user)?;
        let b = self.motion_bounds(user)?;
        self.users[user].cursor =
            Cursor::new(x.clamp(b.left, b.right - 1), y.clamp(b.top, b.bottom - 1));
        self.observation_motion(user);
        if let Err(error) = self.motion(user) {
            self.users[user].status = format!("Routing unavailable: {error:#}");
            if self.strict_mapper_delivery {
                return Err(error);
            }
        }
        Ok(())
    }
    fn motion_bounds(&self, user: usize) -> Result<Bounds> {
        let window = self.policy.focus[user]
            .and_then(|w| targets::surface(w).ok())
            .map(|s| s.client);
        mux_windows::monitors::resolve(&self.users[user].settings, window)
    }
    pub fn input(&mut self, packet: Packet) -> Result<()> {
        self.input_with_mask(packet, None)
    }
    pub fn input_with_source_buttons(&mut self, packet: Packet, source_buttons: u8) -> Result<()> {
        ensure!(source_buttons & !0x1f == 0, "invalid source button mask");
        self.input_with_mask(packet, Some(source_buttons))
    }
    pub fn forwarded_motion(&mut self, packet: Packet, source_buttons: u8) -> Result<bool> {
        ensure!(
            packet.kind == 0 && packet.flags == 0,
            "expected pointer motion"
        );
        self.forwarded_motion_accepted = false;
        self.input_with_source_buttons(packet, source_buttons)?;
        Ok(self.forwarded_motion_accepted)
    }
    fn input_with_mask(&mut self, packet: Packet, source_buttons: Option<u8>) -> Result<()> {
        packet.validate()?;
        let user = self.user(packet)?;
        if self.user_pauses[user].paused {
            let release = (packet.kind == 1 && packet.flags & 1 != 0)
                || (packet.kind == 0 && matches!(packet.flags, 2 | 8 | 32 | 128 | 512));
            // Cleanup UPs bypass BOTH the global discrete-input barrier and
            // target-wide motion barrier. A previously cleared token is a no-op.
            return if release {
                self.route(user, packet, source_buttons)
            } else {
                Ok(())
            };
        }
        if (packet.kind != 0 || packet.flags != 0)
            && let Err(error) = self.observation_flush_motion()
        {
            if !self.report_motion_failure(&error) {
                return Err(error);
            }
            let pointer_owner = if packet.kind != 0 && !self.options.multi_keyboard {
                self.policy.active.unwrap_or(user)
            } else {
                user
            };
            let affected = error
                .downcast_ref::<mux_windows::protoinput::MotionPublicationError>()
                .is_some_and(|publication| {
                    publication
                        .failures
                        .iter()
                        .any(|failure| failure.device == self.seats[pointer_owner].mouse)
                });
            if motion_failure_blocks_input(packet, affected) {
                self.users[user].status =
                    format!("Input withheld after motion publication failure: {error:#}");
                if self.strict_mapper_delivery {
                    return Err(error);
                }
                return Ok(());
            }
        }
        if packet.kind == 0 {
            self.note_mouse_activity(user)?;
        }
        let result = self.route(user, packet, source_buttons);
        if let Err(error) = result {
            self.observation_uncertain(user);
            self.users[user].status = format!("Routing unavailable: {error:#}");
            if self.strict_mapper_delivery {
                return Err(error);
            }
        }
        Ok(())
    }
    fn route(&mut self, user: usize, mut packet: Packet, source_buttons: Option<u8>) -> Result<()> {
        ensure!(
            self.users.get(user).is_some_and(|u| u.active),
            "virtual user has been removed"
        );
        if packet.kind == 2 {
            if self.paused {
                return Ok(());
            }
            let owner = if self.options.multi_keyboard {
                user
            } else {
                self.policy.active.unwrap_or(user)
            };
            self.prepare_selected_input(owner)?;
            let clicked = self
                .keyboard_base(user)?
                .context("Click a window before sending text")?;
            let window = self
                .keyboard_window(user)?
                .context("Click a window before sending text")?;
            let surface = targets::surface(clicked)?;
            self.backend.validate_application_focus(
                window.pid,
                self.users[owner].settings.keyboard.application_focus,
            )?;
            let target = if self.users[owner]
                .selection
                .as_ref()
                .is_some_and(|selection| selection.state.needs_position())
            {
                self.position(owner, &surface)?
            } else if let Some(index) = self.backend.index(window.pid) {
                index
            } else {
                self.position(owner, &surface)?
            };
            self.backend.release_to(target, window.hwnd, packet)?;
            self.observation_record(
                user,
                Change::TextUnit {
                    unit: u16::try_from(packet.data).expect("validated text unit"),
                },
                Admission::NativeSubmissionAccepted,
            );
            return Ok(());
        }
        let identity = if packet.kind == 1 {
            packet.data
        } else {
            Self::button_identity(packet)
        };
        let release = if packet.kind == 1 {
            packet.flags & 1 != 0
        } else {
            matches!(packet.flags, 2 | 8 | 32 | 128 | 512)
        };
        if release {
            let gesture_owned = self.users[user]
                .gesture
                .as_ref()
                .is_some_and(|g| g.identity == identity);
            let owned = self.held.contains_key(&(packet.device, identity)) || gesture_owned;
            self.release_token((packet.device, identity))?;
            if packet.kind == 0 {
                if gesture_owned {
                    self.users[user].gesture = None;
                }
                if !self.mouse_held(user) {
                    self.users[user].capture = None;
                    self.policy.release(user);
                    if owned {
                        self.idle_status(user);
                    }
                }
            }
            return Ok(());
        }
        if self.paused {
            return Ok(());
        }
        if packet.kind == 1 {
            if self.held.contains_key(&(packet.device, identity)) {
                return Ok(());
            }
            let pointer_user = if self.options.multi_keyboard {
                user
            } else {
                self.policy.active.unwrap_or(user)
            };
            let Some(clicked) = self.keyboard_base(user)? else {
                self.users[user].status = "Click a window before typing".into();
                return Ok(());
            };
            // Lazy attachment can post native focus notices. Retain the intended
            // recipient before those effects, then revalidate without retargeting.
            let recipient = keyboard_focus::RetainedRecipient::capture(
                clicked,
                self.users[user].settings.keyboard.focus,
            )?;
            self.prepare_selected_input(pointer_user)?;
            let surface = targets::surface(clicked)?;
            let target = self.position(pointer_user, &surface)?;
            let window = recipient.validate()?;
            let shared_down = self.held.values().any(|held| {
                held.target == target && held.packet.kind == 1 && held.packet.data == packet.data
            });
            let before = self.observation_before(user);
            if self.strict_mapper_delivery {
                // Native submission (including restoring its selected window)
                // can fail after DOWN. Retain the original recipient first so
                // the supervised claims layer can preserve and retry its UP.
                self.held.insert(
                    (packet.device, identity),
                    Held {
                        packet,
                        target,
                        window,
                        user,
                    },
                );
            }
            if !shared_down {
                self.backend.release_to(target, window.hwnd, packet)?;
            }
            if !self.strict_mapper_delivery {
                self.held.insert(
                    (packet.device, identity),
                    Held {
                        packet,
                        target,
                        window,
                        user,
                    },
                );
            }
            if let Some(before) = before {
                self.observation_hold_changes(
                    user,
                    before,
                    if shared_down {
                        Admission::SharedNativeHold
                    } else {
                        Admission::NativeSubmissionAccepted
                    },
                    true,
                );
            }
            return Ok(());
        }
        if packet.flags == 0 {
            if self.selection_unbound(user) && packet.x == 0 && packet.y == 0 {
                return Ok(());
            }
            self.prepare_selected_input(user)?;
            let b = self.motion_bounds(user)?;
            let settings = self.users[user].settings.clone();
            self.users[user]
                .cursor
                .motion(packet.x, packet.y, &settings, b);
            // The terminal logical cursor accepted the source delta. Publication
            // may be coalesced, clamped, or report a later native failure.
            self.forwarded_motion_accepted = true;
            self.observation_motion(user);
            return self.publish_motion(user, true);
        }
        if self.held.contains_key(&(packet.device, identity)) && identity != 0 {
            return Ok(());
        }
        let settings = &self.users[user].settings;
        if matches!(packet.flags, 0x400 | 0x800) {
            let buttons = source_buttons.unwrap_or_else(|| self.source_button_mask(packet.device));
            let Some(transformed) =
                settings.transform_wheel_packet_with_source_buttons(packet, buttons)
            else {
                return Ok(());
            };
            packet = transformed;
            if self.selection_unbound(user) && packet.data as u16 == 0 {
                return Ok(());
            }
        } else {
            let Some(flags) = settings.transform_button_flags(packet.flags) else {
                return Ok(());
            };
            packet.flags = flags;
        }
        if packet.kind == 0 && packet.flags == 0x800 {
            self.prepare_selected_input(user)?;
            let surface = if let Some(capture) = &self.users[user].capture {
                Some(capture.clone())
            } else {
                self.hit(user)?
            };
            let Some(surface) = surface else {
                self.users[user].status = "No eligible window at cursor".into();
                return Ok(());
            };
            let modifiers = self.wheel_modifiers(user);
            mux_windows::wheel::post_horizontal_wheel(
                surface.window.hwnd,
                surface.window.pid,
                (self.users[user].cursor.x, self.users[user].cursor.y),
                packet.data as u16 as i16,
                modifiers,
            )?;
            if let Some(change) = observation::wheel(packet) {
                self.observation_record(user, change, Admission::NativeSubmissionAccepted);
            }
            return Ok(());
        }
        self.prepare_selected_input(user)?;
        let surface = if let Some(capture) = &self.users[user].capture {
            Some(capture.clone())
        } else {
            self.hit(user)?
        };
        let Some(surface) = surface else {
            self.users[user].status = "No eligible window at cursor".into();
            return Ok(());
        };
        let client = self.users[user].capture.is_some()
            || surface.region == 1
                && surface
                    .client
                    .contains(self.users[user].cursor.x, self.users[user].cursor.y);
        if !client {
            if packet.flags == 1
                && matches!(surface.region, 2 | 10..=17)
                && self.claim(user, &surface, false)?
            {
                self.users[user].gesture = Some(Gesture {
                    identity,
                    start: (self.users[user].cursor.x, self.users[user].cursor.y),
                    outer: surface.outer,
                    surface: surface.clone(),
                });
                self.observation.fence_user(user, LossReason::Uncertain);
                self.keyboard[user] = Some(surface.window);
                self.users[user].status = if surface.region == 2 {
                    "Moving window".into()
                } else {
                    "Resizing window".into()
                };
            }
            return Ok(());
        }
        let click = packet.flags != 0x400;
        if !self.claim(user, &surface, click)? {
            return Ok(());
        }
        if click
            && self.users[user]
                .selection
                .as_ref()
                .is_some_and(|s| s.identity.window() != surface.window)
        {
            self.users[user].selection = None;
        }
        if click && self.keyboard[user] != Some(surface.window) {
            self.release_user(user)?;
            self.keyboard[user] = Some(surface.window);
        }
        let target = self.position(user, &surface)?;
        let before = self.observation_before(user);
        if packet.flags != 0x400 {
            self.users[user].capture = Some(surface.clone());
            self.policy.press(user);
            self.held.insert(
                (packet.device, identity),
                Held {
                    packet,
                    target,
                    window: surface.window,
                    user,
                },
            );
        }
        self.backend.submit(target, packet)?;
        if let Some(before) = before {
            self.observation_hold_changes(user, before, Admission::NativeSubmissionAccepted, true);
        }
        if let Some(change) = observation::wheel(packet) {
            self.observation_record(user, change, Admission::NativeSubmissionAccepted);
        }
        self.users[user].status = format!(
            "{} · {}",
            if self.policy.mode == Mode::Switched {
                "Switched"
            } else {
                "Concurrent"
            },
            surface.title
        );
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

    fn motion(&mut self, user: usize) -> Result<()> {
        self.publish_motion(user, false)
    }
    fn publish_motion(&mut self, user: usize, coalesce: bool) -> Result<()> {
        let result = self.publish_motion_inner(user, coalesce);
        if result.is_err() {
            self.observation_uncertain(user);
        }
        result
    }
    fn publish_motion_inner(&mut self, user: usize, coalesce: bool) -> Result<()> {
        if self.user_pauses[user].paused {
            return Ok(());
        }
        if self.users[user]
            .selection
            .as_ref()
            .is_some_and(|selection| !selection.state.permits_publication())
        {
            return Ok(());
        }
        if let Some(gesture) = &self.users[user].gesture {
            let dx = self.users[user].cursor.x - gesture.start.0;
            let dy = self.users[user].cursor.y - gesture.start.1;
            let mut b = gesture.outer;
            match gesture.surface.region {
                2 => {
                    b.left += dx;
                    b.right += dx;
                    b.top += dy;
                    b.bottom += dy;
                }
                region => {
                    if matches!(region, 10 | 13 | 16) {
                        b.left = (b.left + dx).min(b.right - 160);
                    }
                    if matches!(region, 11 | 14 | 17) {
                        b.right = (b.right + dx).max(b.left + 160);
                    }
                    if matches!(region, 12..=14) {
                        b.top = (b.top + dy).min(b.bottom - 100);
                    }
                    if matches!(region, 15..=17) {
                        b.bottom = (b.bottom + dy).max(b.top + 100);
                    }
                }
            }
            let window = gesture.surface.window;
            self.observation_flush_motion()?;
            targets::place(window, b)?;
            if self.backend.index(window.pid).is_some() {
                let surface = targets::surface(window)?;
                self.position(user, &surface)?;
            }
            return Ok(());
        }
        if self.policy.mode == Mode::Switched && self.policy.root.is_some_and(|root| root != user) {
            return Ok(());
        }
        let window = self.users[user]
            .capture
            .as_ref()
            .map(|s| s.window)
            .or(self.policy.focus[user]);
        if let Some(window) = window
            && let Ok(surface) = targets::surface(window)
        {
            if coalesce {
                let target = self.open(
                    surface.window.pid,
                    self.users[user].settings.keyboard.application_focus,
                )?;
                self.backend.queue_position(
                    target,
                    surface.window.hwnd,
                    self.seats[user].mouse,
                    self.users[user].cursor.x,
                    self.users[user].cursor.y,
                    surface.client,
                    self.users[user].settings.motion_coalescing,
                )?;
            } else {
                self.position(user, &surface)?;
            }
        }
        Ok(())
    }
    pub fn tick(&mut self) -> Result<(bool, Vec<State>)> {
        self.observation_check_screens();
        let stop = self.guard.pump();
        for pid in self.backend.exited() {
            let affected = self
                .held
                .values()
                .filter_map(|held| {
                    (held.window.pid == pid && self.observation.observing_user(held.user))
                        .then_some(held.user)
                })
                .collect::<BTreeSet<_>>();
            for user in affected {
                self.observation.fence_user(user, LossReason::Removed);
            }
            for user in 0..self.users.len() {
                if self.policy.focus[user].is_some_and(|w| w.pid == pid) {
                    self.users[user].status = "Target exited; click another window".into();
                }
            }
            self.policy.forget(pid);
            for user in 0..self.users.len() {
                self.held.retain(|_, h| h.window.pid != pid);
                if self.keyboard[user].is_some_and(|w| w.pid == pid) {
                    self.keyboard[user] = None;
                }
                if self.users[user]
                    .capture
                    .as_ref()
                    .is_some_and(|s| s.window.pid == pid)
                    || self.users[user]
                        .gesture
                        .as_ref()
                        .is_some_and(|g| g.surface.window.pid == pid)
                {
                    self.users[user].capture = None;
                    self.users[user].gesture = None;
                    self.policy.release(user);
                }
            }
        }
        self.retry_user_pause_releases();
        let mut states = Vec::new();
        for user in 0..self.users.len() {
            if !self.users[user].active {
                states.push(State {
                    name: self.seats[user].name.clone(),
                    pid: 0,
                    client: (self.users[user].cursor.x, self.users[user].cursor.y),
                    bounds: targets::desktop_bounds(),
                    movement_bounds: targets::desktop_bounds(),
                    buttons: Vec::new(),
                    keys: Vec::new(),
                    pending_releases: None,
                    keyboard: None,
                    overlay: OverlayStatus {
                        presentation: Default::default(),
                        application_cursor: None,
                        hwnd: 0,
                        x: self.users[user].cursor.x,
                        y: self.users[user].cursor.y,
                        visible: false,
                        no_activate: true,
                        click_through: true,
                        painted_pixels: 0,
                        shape: self.users[user].settings.shape,
                        system_cursor_fallback: None,
                        badge: self.users[user].settings.badge.clone(),
                        render_origin: (self.users[user].cursor.x, self.users[user].cursor.y),
                        hotspot: (0, 0),
                        pixel_hash: 0,
                        pixel_telemetry_source: None,
                        canvas_size: (0, 0),
                        idle_hidden: false,
                        rendered_label: String::new(),
                        label_truncated: false,
                        paint_generation: 0,
                    },
                    title: String::new(),
                    status: self.users[user].status.clone(),
                    root: false,
                    captured: false,
                    keyboard_pid: 0,
                    mode: Some(self.policy.mode),
                });
                continue;
            }
            if let Some(window) = self.policy.focus[user]
                && (targets::surface(window).is_err()
                    || self.users[user]
                        .selection
                        .as_ref()
                        .is_some_and(|s| s.identity.revalidate().is_err()))
            {
                let released = self.release_user(user);
                self.keyboard[user] = None;
                self.users[user].selection = None;
                self.users[user].status = match released {
                    Ok(()) => {
                        self.policy.forget(window.pid);
                        "Target closed or hidden; choose another window".into()
                    }
                    Err(error) => {
                        // Retain focus and held tokens until release succeeds or
                        // the process exits; other users must keep routing.
                        format!("Target closed; release pending: {error:#}")
                    }
                };
            }
            let focus = self.policy.focus[user].and_then(|w| targets::surface(w).ok());
            let movement_bounds = self.motion_bounds(user)?;
            let old = (self.users[user].cursor.x, self.users[user].cursor.y);
            if !self.selection_unbound(user) {
                self.users[user]
                    .cursor
                    .motion(0, 0, &CursorSettings::default(), movement_bounds);
            }
            if old != (self.users[user].cursor.x, self.users[user].cursor.y) {
                self.observation_motion(user);
            }
            if !self.paused
                && !self.user_pauses[user].paused
                && old != (self.users[user].cursor.x, self.users[user].cursor.y)
                && let Err(error) = self.motion(user)
            {
                if !self.report_motion_failure(&error) {
                    return Err(error);
                }
                // The stale segment was discarded. Publish the newly clamped
                // current goal once; this never replays its old coordinates.
                if let Err(error) = self.motion(user) {
                    self.users[user].status =
                        format!("Current cursor publication unavailable: {error:#}");
                }
            }
            let mouse_held = self
                .held
                .values()
                .any(|held| held.user == user && held.packet.kind == 0)
                || self.users[user].gesture.is_some();
            let application_target = if self.selection_unbound(user) {
                None
            } else {
                focus
                    .as_ref()
                    .and_then(|surface| self.backend.index(surface.window.pid))
            };
            if let Some(previous) = self.users[user].application_target
                && Some(previous) != application_target
            {
                self.backend.cursor_observation(previous, 0, false);
            }
            self.users[user].application_target =
                application_target.filter(|_| self.users[user].settings.follow_application);
            let application = if let Some(index) = application_target {
                self.backend.cursor_observation(
                    index,
                    focus.as_ref().unwrap().window.hwnd,
                    self.users[user].settings.follow_application,
                )
            } else {
                mux_windows::app_cursor::AppCursorObservation {
                    state: if self.users[user].settings.follow_application {
                        "No selected application; using configured cursor".into()
                    } else {
                        "Disabled; using configured cursor".into()
                    },
                    capability: self.backend.cursor_capability(),
                    ..Default::default()
                }
            };
            let keyboard = self.keyboard_state(user);
            let u = &mut self.users[user];
            if let Some(overlay) = &mut u.overlay {
                let idle_hidden = u.settings.visible
                    && u.settings.idle_hidden(
                        u.last_mouse_activity
                            .elapsed()
                            .as_millis()
                            .min(u128::from(u64::MAX)) as u64,
                        mouse_held,
                    );
                let mut effective = u.settings.clone();
                effective.visible &= !idle_hidden && !self.forwarding_hidden[user];
                overlay.draw_application(
                    (u.cursor.x, u.cursor.y),
                    &self.seats[user].name,
                    &effective,
                    self.paused,
                    Some(&application),
                )?;
                overlay.status.idle_hidden = idle_hidden;
            }
            states.push(State {
                name: self.seats[user].name.clone(),
                pid: focus.as_ref().map_or(0, |s| s.window.pid),
                client: focus.as_ref().map_or((u.cursor.x, u.cursor.y), |s| {
                    (u.cursor.x - s.client.left, u.cursor.y - s.client.top)
                }),
                bounds: focus
                    .as_ref()
                    .map_or_else(targets::desktop_bounds, |s| s.client),
                movement_bounds,
                pending_releases: None,
                keyboard,
                buttons: self
                    .held
                    .values()
                    .filter(|h| h.user == user && h.packet.kind == 0)
                    .map(|h| h.packet.flags)
                    .collect(),
                keys: self
                    .held
                    .values()
                    .filter(|h| h.user == user && h.packet.kind == 1)
                    .map(|h| h.packet.data)
                    .collect(),
                overlay: u
                    .overlay
                    .as_ref()
                    .context("active user overlay is missing")?
                    .status
                    .clone(),
                title: focus.map_or_else(String::new, |s| s.title),
                status: u.status.clone(),
                root: self.policy.root == Some(user),
                captured: u.capture.is_some() || u.gesture.is_some(),
                keyboard_pid: if self.options.multi_keyboard {
                    self.keyboard[user]
                } else {
                    self.policy.active.and_then(|active| self.keyboard[active])
                }
                .map_or(0, |w| w.pid),
                mode: Some(self.policy.mode),
            });
        }
        Ok((stop, states))
    }
}
impl Drop for DesktopRouter {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod release_tests {
    use super::*;
    #[test]
    fn individually_paused_owner_keeps_pid_lease_without_blocking_peers_own_pid() {
        let mut policy = Policy::new(2, Mode::Concurrent);
        let a = Window { pid: 10, hwnd: 100 };
        let b = Window { pid: 20, hwnd: 200 };
        assert!(policy.select(0, a) && policy.select(1, b));
        policy.release(0); // Only the transient switched root is released.
        let busy = [pause_reserves_owner(true, false, false), false];
        assert!(matches!(policy.claim(1, a, &busy, true), Claim::Busy(0)));
        assert_eq!(policy.owner(10), Some(0));
        assert_eq!(policy.focus[0], Some(a));
        assert!(matches!(
            policy.claim(1, b, &busy, true),
            Claim::Granted { .. }
        ));
        assert!(matches!(
            policy.claim(1, a, &[false, false], true),
            Claim::Granted { evicted: Some(0) }
        ));
    }
    #[test]
    fn failed_paused_user_up_preserves_saved_recipient_and_peer_hold_until_retry() {
        let a = (0xf1000003, 65);
        let b = (0xf1000005, 66);
        let mut held = BTreeMap::new();
        for (user, token) in [a, b].into_iter().enumerate() {
            held.insert(
                token,
                Held {
                    packet: Packet::key_scanned(token.0, token.1, 30, 0, false),
                    target: user,
                    window: Window {
                        pid: 10 + user as u32,
                        hwnd: 100 + user,
                    },
                    user,
                },
            );
        }
        assert!(
            release_held_token(
                &mut held,
                a,
                |_| true,
                |saved| {
                    assert_eq!(saved.target, 0);
                    assert_eq!(saved.window, Window { pid: 10, hwnd: 100 });
                    anyhow::bail!("controlled UP failure")
                }
            )
            .is_err()
        );
        assert_eq!(held.len(), 2);
        release_held_token(
            &mut held,
            a,
            |_| true,
            |saved| {
                assert_eq!(saved.packet.data, 65);
                Ok(())
            },
        )
        .unwrap();
        assert!(!held.contains_key(&a));
        assert!(held.contains_key(&b));
        let mut calls = 0;
        release_held_token(
            &mut held,
            a,
            |_| true,
            |_| {
                calls += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            calls, 0,
            "a duplicate cleanup UP must not publish or flush anything"
        );
    }

    #[test]
    fn motion_failure_preserves_releases_and_unrelated_user_input() {
        let mouse = 0xf1000002;
        let keyboard = 0xf1000003;
        assert!(motion_failure_blocks_input(
            Packet::button(mouse, 0, false),
            true
        ));
        assert!(motion_failure_blocks_input(
            Packet::key(keyboard, 0x57, false),
            true
        ));
        assert!(motion_failure_blocks_input(Packet::wheel(mouse, 120), true));
        assert!(!motion_failure_blocks_input(
            Packet::key(keyboard, 0x57, true),
            true
        ));
        for button in 0..5 {
            assert!(!motion_failure_blocks_input(
                Packet::button(mouse, button, true),
                true
            ));
        }
        assert!(!motion_failure_blocks_input(
            Packet::button(mouse, 0, false),
            false
        ));
        assert!(!motion_failure_blocks_input(
            Packet::key(keyboard, 0x57, false),
            false
        ));
    }

    #[test]
    fn last_key_owner_survives_failed_release_and_retries() {
        let first = (0xf1000003, 118);
        let last = (0xf1000005, 118);
        let mut held = BTreeMap::new();
        for (user, token) in [first, last].into_iter().enumerate() {
            held.insert(
                token,
                Held {
                    packet: Packet {
                        device: token.0,
                        kind: 1,
                        flags: 0,
                        x: 65,
                        y: 0,
                        data: token.1,
                    },
                    target: 0,
                    window: Window { pid: 10, hwnd: 100 },
                    user,
                },
            );
        }
        let mut attempts = 0;
        release_held_token(
            &mut held,
            first,
            |_| true,
            |_| {
                attempts += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(attempts, 0, "another device still owns this target key");
        assert!(!held.contains_key(&first));
        assert!(held.contains_key(&last));

        let failed = release_held_token(
            &mut held,
            last,
            |_| true,
            |_| {
                attempts += 1;
                anyhow::bail!("target window vanished before process exit")
            },
        );
        assert!(failed.is_err());
        assert_eq!(attempts, 1);
        assert!(held.contains_key(&last), "failed release must remain owned");

        release_held_token(
            &mut held,
            last,
            |_| true,
            |owner| {
                attempts += 1;
                assert_eq!(owner.user, 1);
                let up = DesktopRouter::release_packet(owner.packet);
                assert_eq!(up.data, 118);
                assert_eq!(up.flags & 1, 1);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(attempts, 2, "last owner must emit its retry");
        assert!(held.is_empty());
    }
}
