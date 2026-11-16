//! Session-owned annotation policy decorates the same engine used by macros and replication.
use crate::{router::State as RoutingState, routing_engine::Engine};
use anyhow::{Context, Result, ensure};
use mux_core::{
    Profile, ResolvedSeat,
    annotation::{Document, Settings, Tool, UserStatus},
    desktop::{Bounds, CursorSettings},
    emulated::Packet,
    routing::{Mode, Window},
};
use mux_windows::{
    annotation_overlay::{AnnotationOverlay, AnnotationOverlayStatus},
    targets,
};
use rmcp::schemars;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExportKind {
    InkOnly,
    SelectedTarget,
    FullDesktop,
    DesktopRegion,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum InkScope {
    SelectedUser,
    #[default]
    AllUsers,
}

#[derive(Clone, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Inspect,
    Configure {
        user: usize,
        settings: Settings,
    },
    ClearUser {
        user: usize,
    },
    ClearAll,
    Export {
        user: usize,
        path: String,
        kind: ExportKind,
        #[serde(default)]
        ink_scope: InkScope,
        #[serde(default)]
        desktop_region: Option<Bounds>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportInfo {
    // Historical export metadata omitted these fields: 0/all_users/0 are unknown
    // request metadata fallbacks, rather than evidence of a user or revision.
    #[serde(default)]
    pub user: usize,
    #[serde(default)]
    pub ink_scope: InkScope,
    #[serde(default)]
    pub revision: u64,
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub kind: ExportKind,
    pub pid: Option<u32>,
    pub hwnd: Option<usize>,
    #[serde(default)]
    pub bounds: Option<Bounds>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportProgress {
    pub user: usize,
    pub kind: ExportKind,
    pub ink_scope: InkScope,
    pub phase: String,
}

/// Request-time windows plus the session's window-lifetime epoch, never persisted handles.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DesktopOverlayBinding {
    pub epoch: u64,
    pub hwnds: Vec<usize>,
}

/// Immutable request-time ink and capture authority. Later document edits are independent.
#[derive(Clone)]
pub struct ExportSnapshot {
    pub user: usize,
    pub kind: ExportKind,
    pub ink_scope: InkScope,
    pub revision: u64,
    pub bounds: Bounds,
    pub desktop_bounds: Option<Bounds>,
    pub window: Option<Window>,
    pub(crate) desktop_overlays: DesktopOverlayBinding,
    document: Document,
}

impl ExportSnapshot {
    /// Run on an export worker: render ink and optionally capture the authorized source.
    pub(crate) fn prepare(self, path: String) -> Result<crate::agent_export::Prepared<ExportInfo>> {
        crate::agent_export::validate_path(&path)?;
        let (width, height) = export_dimensions(self.bounds)?;
        let ink = match self.ink_scope {
            InkScope::SelectedUser => self.document.render_user(self.user, self.bounds)?,
            InkScope::AllUsers => self.document.render(self.bounds)?,
        };
        let rgba = match self.kind {
            ExportKind::InkOnly => {
                ensure!(
                    targets::desktop_bounds() == self.bounds,
                    "desktop bounds changed during annotation export; retry"
                );
                ink
            }
            ExportKind::SelectedTarget => {
                let window = self.window.context("annotation export target is missing")?;
                ensure!(
                    targets::client_bounds(window.hwnd, window.pid)? == self.bounds,
                    "target moved before annotation export; retry"
                );
                let (bounds, mut background) = mux_windows::target_capture::client(window)?;
                ensure!(
                    bounds == self.bounds,
                    "target moved during annotation export; retry"
                );
                composite_ink(&mut background, &ink)?;
                background
            }
            ExportKind::FullDesktop | ExportKind::DesktopRegion => {
                ensure!(
                    Some(targets::desktop_bounds()) == self.desktop_bounds,
                    "desktop bounds changed during annotation export; retry"
                );
                for &hwnd in &self.desktop_overlays.hwnds {
                    ensure!(
                        mux_windows::desktop_capture::exclude_window(hwnd)?,
                        "owned overlay cannot be excluded from desktop capture"
                    );
                }
                let (bounds, mut background) = mux_windows::desktop_capture::region(self.bounds)?;
                ensure!(
                    bounds == self.bounds,
                    "desktop capture bounds changed; retry"
                );
                composite_ink(&mut background, &ink)?;
                ensure!(
                    Some(targets::desktop_bounds()) == self.desktop_bounds,
                    "desktop bounds changed during annotation export; retry"
                );
                background
            }
        };
        Ok(crate::agent_export::Prepared {
            width,
            height,
            rgba: rgba.into(),
            metadata: ExportInfo {
                user: self.user,
                ink_scope: self.ink_scope,
                revision: self.revision,
                path,
                width,
                height,
                kind: self.kind,
                pid: self.window.map(|window| window.pid),
                hwnd: self.window.map(|window| window.hwnd),
                bounds: Some(self.bounds),
            },
        })
    }
}

fn composite_ink(background: &mut [u8], ink: &[u8]) -> Result<()> {
    ensure!(
        background.len() == ink.len() && background.len().is_multiple_of(4),
        "annotation capture pixel count changed"
    );
    for (dst, src) in background
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(ink.as_chunks::<4>().0.iter())
    {
        let alpha = u32::from(src[3]);
        for channel in 0..3 {
            dst[channel] =
                ((u32::from(src[channel]) * alpha + u32::from(dst[channel]) * (255 - alpha) + 127)
                    / 255) as u8;
        }
        dst[3] = 255;
    }
    Ok(())
}

fn export_dimensions(bounds: Bounds) -> Result<(u32, u32)> {
    ensure!(
        bounds.valid_region(),
        "annotation export bounds are invalid"
    );
    let width = u32::try_from(i64::from(bounds.right) - i64::from(bounds.left))?;
    let height = u32::try_from(i64::from(bounds.bottom) - i64::from(bounds.top))?;
    ensure!(
        u64::from(width) * u64::from(height) <= mux_core::annotation::MAX_EXPORT_PIXELS,
        "annotation export exceeds the 16 million pixel limit"
    );
    Ok((width, height))
}

fn resolve_desktop_export_bounds(
    kind: ExportKind,
    requested: Option<Bounds>,
    desktop: Bounds,
) -> Result<Option<Bounds>> {
    match kind {
        ExportKind::InkOnly | ExportKind::SelectedTarget => {
            ensure!(
                requested.is_none(),
                "desktop_region is only valid for desktop_region exports"
            );
            Ok(None)
        }
        ExportKind::FullDesktop => {
            ensure!(
                requested.is_none(),
                "desktop_region is only valid for desktop_region exports"
            );
            export_dimensions(desktop)?;
            Ok(Some(desktop))
        }
        ExportKind::DesktopRegion => {
            let region =
                requested.context("desktop_region is required for desktop_region exports")?;
            ensure!(
                desktop.intersection(region) == Some(region),
                "desktop_region lies outside the current physical desktop"
            );
            export_dimensions(region)?;
            Ok(Some(region))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub acknowledged: bool,
    pub users: Vec<UserStatus>,
    pub revision: u64,
    pub overlay: Option<AnnotationOverlayStatus>,
    pub export: Option<ExportInfo>,
    #[serde(default)]
    pub export_pending: Option<ExportProgress>,
    pub error: Option<String>,
}
pub struct Runtime {
    inner: Box<dyn Engine>,
    seats: Vec<ResolvedSeat>,
    cursors: Vec<CursorSettings>,
    document: Document,
    consumed_buttons: Vec<Option<u32>>,
    consumed_keys: Vec<BTreeSet<u32>>,
    overlay: Option<AnnotationOverlay>,
    painted_revision: Option<u64>,
    painted_bounds: Option<Bounds>,
    stroke_windows: Vec<Option<Option<Window>>>,
    export_pending: Option<ExportProgress>,
}
impl Runtime {
    pub fn new(
        inner: Box<dyn Engine>,
        profile: &Profile,
        seats: Vec<ResolvedSeat>,
    ) -> Result<Self> {
        let mut document = Document::new(seats.len())?;
        for (user, seat) in profile.seats.iter().enumerate() {
            seat.annotation.validate()?;
            ensure!(
                seat.annotation.clear_key.is_none() || seats[user].keyboard.is_some(),
                "annotation clear shortcut requires a paired keyboard"
            );
            if seat.annotation.tool != Tool::Off {
                inner.pointer(user)?;
            }
            document.configure(user, seat.annotation.clone())?;
        }
        let count = seats.len();
        Ok(Self {
            inner,
            seats,
            cursors: profile.seats.iter().map(|s| s.cursor.clone()).collect(),
            document,
            consumed_buttons: vec![None; count],
            consumed_keys: vec![BTreeSet::new(); count],
            overlay: None,
            painted_revision: None,
            painted_bounds: None,
            stroke_windows: vec![None; count],
            export_pending: None,
        })
    }
    pub fn annotation_pixels(&self, bounds: Bounds) -> Result<Option<Vec<u8>>> {
        if self.document.status().iter().any(|user| user.segments > 0) {
            Ok(Some(self.document.render(bounds)?))
        } else {
            Ok(None)
        }
    }
    pub fn annotation_state(&self) -> State {
        State {
            acknowledged: false,
            users: self.document.status(),
            revision: self.document.revision(),
            overlay: self.overlay.as_ref().map(|o| o.status().clone()),
            export: None,
            export_pending: self.export_pending.clone(),
            error: None,
        }
    }
    pub fn export_snapshot(
        &mut self,
        user: usize,
        kind: ExportKind,
        ink_scope: InkScope,
        desktop_region: Option<Bounds>,
    ) -> Result<ExportSnapshot> {
        self.document.settings(user)?;
        let current_desktop = targets::desktop_bounds();
        let desktop_export_bounds =
            resolve_desktop_export_bounds(kind, desktop_region, current_desktop)?;
        let (bounds, window, desktop_bounds) = match kind {
            ExportKind::InkOnly => (current_desktop, None, Some(current_desktop)),
            ExportKind::SelectedTarget => {
                let window = self
                    .inner
                    .selected_window(user)?
                    .context("select an eligible target window before exporting")?;
                (
                    targets::client_bounds(window.hwnd, window.pid)?,
                    Some(window),
                    None,
                )
            }
            ExportKind::FullDesktop => {
                self.validate_desktop_export(current_desktop)?;
                (
                    desktop_export_bounds.context("desktop bounds missing")?,
                    None,
                    Some(current_desktop),
                )
            }
            ExportKind::DesktopRegion => {
                self.validate_desktop_export(current_desktop)?;
                (
                    desktop_export_bounds.context("desktop region missing")?,
                    None,
                    Some(current_desktop),
                )
            }
        };
        export_dimensions(bounds)?;
        Ok(ExportSnapshot {
            user,
            kind,
            ink_scope,
            revision: self.document.revision(),
            bounds,
            desktop_bounds,
            window,
            desktop_overlays: DesktopOverlayBinding {
                epoch: 0,
                hwnds: self
                    .overlay
                    .iter()
                    .map(|overlay| overlay.status().hwnd)
                    .collect(),
            },
            document: self.document.clone(),
        })
    }

    /// Exclude hidden layers too: they may become visible while a capture is preparing.
    pub fn validate_desktop_export(&mut self, expected_extent: Bounds) -> Result<()> {
        ensure!(
            expected_extent.valid_region() && targets::desktop_bounds() == expected_extent,
            "desktop bounds changed before annotation export; retry"
        );
        if let Some(overlay) = self.overlay.as_mut() {
            ensure!(
                overlay.refresh_capture_exclusion(),
                "annotation overlay cannot be excluded from desktop capture"
            );
        }
        Ok(())
    }

    pub fn set_export_pending(&mut self, pending: Option<ExportProgress>) {
        self.export_pending = pending;
    }

    fn paint(&mut self) -> Result<()> {
        let bounds = targets::desktop_bounds();
        if self.painted_revision == Some(self.document.revision())
            && self.painted_bounds == Some(bounds)
        {
            return Ok(());
        }
        if !self.document.status().iter().any(|u| u.segments > 0) {
            if let Some(overlay) = self.overlay.as_mut() {
                overlay.hide()?;
            }
        } else {
            let rgba = self.document.render(bounds)?;
            let overlay = match self.overlay.as_mut() {
                Some(overlay) => overlay,
                None => {
                    self.overlay = Some(AnnotationOverlay::new(bounds)?);
                    self.overlay.as_mut().unwrap()
                }
            };
            overlay.draw(bounds, &rgba)?;
        }
        self.painted_revision = Some(self.document.revision());
        self.painted_bounds = Some(bounds);
        Ok(())
    }
    /// A consumed activator/shortcut remains owned until its matching release,
    /// even if Clear has already ended the visible stroke.
    pub fn configuration_idle(&self, user: usize) -> Result<bool> {
        self.document.settings(user)?;
        Ok(!self.document.is_open(user)?
            && self.consumed_buttons[user].is_none()
            && self.consumed_keys[user].is_empty())
    }
    pub fn annotation_command(&mut self, command: Command) -> Result<State> {
        let mut export = None;
        match command {
            Command::Inspect => (),
            Command::Configure { user, settings } => {
                settings.validate()?;
                self.document.settings(user)?;
                ensure!(
                    settings.clear_key.is_none() || self.seats[user].keyboard.is_some(),
                    "annotation clear shortcut requires a paired keyboard"
                );
                if settings.tool != Tool::Off {
                    self.inner.pointer(user)?;
                    let bounds = targets::desktop_bounds();
                    ensure!(
                        bounds.valid_region()
                            && (i64::from(bounds.right) - i64::from(bounds.left))
                                * (i64::from(bounds.bottom) - i64::from(bounds.top))
                                <= 16_000_000,
                        "desktop annotation exceeds the image budget"
                    );
                }
                // End target holds for only this user before changing what consumes their buttons.
                self.inner.settings(user, self.cursors[user].clone())?;
                self.document.configure(user, settings)?;
                self.stroke_windows[user] = None;
            }
            Command::ClearUser { user } => self.document.clear(user, false)?,
            Command::ClearAll => self.document.clear(0, true)?,
            Command::Export {
                user,
                path,
                kind,
                ink_scope,
                desktop_region,
            } => {
                let prepared = self
                    .export_snapshot(user, kind, ink_scope, desktop_region)?
                    .prepare(path.clone())?;
                image::save_buffer_with_format(
                    &path,
                    &prepared.rgba,
                    prepared.width,
                    prepared.height,
                    image::ColorType::Rgba8,
                    image::ImageFormat::Png,
                )
                .context("save annotation PNG")?;
                export = Some(prepared.metadata);
            }
        }
        self.paint()?;
        let mut state = self.annotation_state();
        state.acknowledged = true;
        state.export = export;
        Ok(state)
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
            .context("unassigned virtual device")
    }
    fn sample(&mut self, user: usize) -> Result<()> {
        if self.document.is_open(user)? {
            if self.stroke_windows[user]
                .is_some_and(|window| self.inner.selected_window(user).ok() != Some(window))
            {
                self.document.end(user)?;
                self.stroke_windows[user] = None;
                return Ok(());
            }
            let point = self.inner.pointer(user)?;
            self.document.sample(user, point)?;
        }
        Ok(())
    }
    fn input_with_mask(&mut self, packet: Packet, source_buttons: Option<u8>) -> Result<()> {
        packet.validate()?;
        let user = self.user(packet)?;
        let mut source_buttons = source_buttons.unwrap_or_else(|| {
            (0..5).fold(0, |mask, button| {
                if self.inner.button_held(packet.device, button) {
                    mask | (1 << button)
                } else {
                    mask
                }
            })
        });
        if let Some(flag) = self.consumed_buttons[user] {
            source_buttons |= 1 << (flag.trailing_zeros() / 2);
        }
        if self.inner.paused() || self.inner.user_pause_state(user).paused {
            return self.inner.input_with_source_buttons(packet, source_buttons);
        }
        let settings = self.document.settings(user)?.clone();
        if packet.kind == 0 {
            if self.consumed_buttons[user]
                .is_some_and(|down| packet.flags == down || packet.flags == down * 2)
            {
                if packet.flags == self.consumed_buttons[user].unwrap() * 2 {
                    self.document.end(user)?;
                    self.consumed_buttons[user] = None;
                    self.stroke_windows[user] = None;
                }
                return Ok(());
            }
            if settings.tool != Tool::Off && packet.flags == settings.activator.down_flag() {
                let point = self.inner.pointer(user)?;
                let window = self.inner.selected_window(user)?;
                self.document.begin(user, point)?;
                self.stroke_windows[user] = Some(window);
                self.consumed_buttons[user] = Some(packet.flags);
                return Ok(());
            }
            self.inner
                .input_with_source_buttons(packet, source_buttons)?;
            if packet.flags == 0 {
                self.sample(user)?;
            }
        } else if packet.kind == 1 {
            if self.consumed_keys[user].contains(&packet.data) {
                if packet.flags & 1 != 0 {
                    self.consumed_keys[user].remove(&packet.data);
                }
                return Ok(());
            }
            if settings.tool != Tool::Off
                && settings.clear_key.map(u32::from) == Some(packet.data)
                && packet.flags & 1 == 0
            {
                self.document.clear(user, settings.clear_all)?;
                self.consumed_keys[user].insert(packet.data);
                return Ok(());
            }
            self.inner
                .input_with_source_buttons(packet, source_buttons)?;
        } else {
            self.inner
                .input_with_source_buttons(packet, source_buttons)?;
        }
        Ok(())
    }
    fn end_all(&mut self) {
        self.document.end_all();
        self.consumed_buttons.fill(None);
        self.stroke_windows.fill(None);
        for keys in &mut self.consumed_keys {
            keys.clear();
        }
    }
}
impl Engine for Runtime {
    fn desktop_observation(
        &mut self,
        operation: crate::desktop_router::observation::Operation,
    ) -> Vec<crate::desktop_router::observation::Delivery> {
        self.inner.desktop_observation(operation)
    }
    fn user_pause_state(&self, user: usize) -> crate::routing_engine::UserPauseState {
        self.inner.user_pause_state(user)
    }
    fn pause_user(&mut self, user: usize, paused: bool) -> Result<()> {
        let result = self.inner.pause_user(user, paused);
        if paused && self.inner.user_pause_state(user).paused {
            let ended = self.document.end(user);
            self.consumed_buttons[user] = None;
            self.stroke_windows[user] = None;
            self.consumed_keys[user].clear();
            result.and(ended)
        } else {
            result
        }
    }
    fn mapper_delivery(&mut self, active: bool) -> Result<()> {
        self.inner.mapper_delivery(active)
    }
    fn forwarding_supported(&self) -> bool {
        self.inner.forwarding_supported()
    }
    fn set_forwarding_sources(&mut self, sources: &[bool]) -> Result<()> {
        self.inner.set_forwarding_sources(sources)
    }
    fn forwarded_motion(&mut self, packet: Packet, source_buttons: u8) -> Result<bool> {
        ensure!(
            packet.kind == 0 && packet.flags == 0,
            "expected pointer motion"
        );
        let user = self.user(packet)?;
        let mut source_buttons = source_buttons;
        if let Some(flag) = self.consumed_buttons[user] {
            source_buttons |= 1 << (flag.trailing_zeros() / 2);
        }
        let accepted = self.inner.forwarded_motion(packet, source_buttons)?;
        if accepted {
            self.sample(user)?;
        }
        Ok(accepted)
    }
    fn begin_text_delivery(&mut self, user: usize) -> Result<()> {
        self.inner.begin_text_delivery(user)
    }
    fn end_text_delivery(&mut self, user: usize) {
        self.inner.end_text_delivery(user)
    }
    fn pending_attachment_window(&self, user: usize) -> Result<Option<Window>> {
        self.inner.pending_attachment_window(user)
    }
    fn select_window(&mut self, selection: &crate::window_selection::Selection) -> Result<()> {
        ensure!(
            self.configuration_idle(selection.user)?,
            "Release annotation input before selecting a window"
        );
        self.inner.select_window(selection)
    }
    fn validate_mode_change(&self, multi_keyboard: bool) -> Result<()> {
        self.inner.validate_mode_change(multi_keyboard)
    }

    fn keyboard_enabled(&self, user: usize) -> bool {
        self.inner.keyboard_enabled(user)
    }
    fn poll_motion(&mut self) -> Result<()> {
        self.inner.poll_motion()
    }
    fn motion_deadline(&self) -> Option<std::time::Instant> {
        self.inner.motion_deadline()
    }
    fn annotation_clears_all(&self, user: usize, packet: Packet) -> bool {
        packet.kind == 1
            && packet.flags & 1 == 0
            && self.document.settings(user).is_ok_and(|settings| {
                settings.tool != Tool::Off
                    && settings.clear_all
                    && settings.clear_key.map(u32::from) == Some(packet.data)
            })
    }
    fn note_mouse_activity(&mut self, user: usize) -> Result<()> {
        self.inner.note_mouse_activity(user)
    }
    fn shutdown(&mut self) -> Result<()> {
        self.end_all();
        self.inner.shutdown()
    }
    fn add_virtual_user(&mut self, seat: &mux_core::Seat, resolved: ResolvedSeat) -> Result<usize> {
        ensure!(
            seat.annotation.tool == Tool::Off,
            "Generated users start with annotation disabled"
        );
        self.document.reserve_user()?;
        self.seats.try_reserve(1)?;
        self.cursors.try_reserve(1)?;
        self.consumed_buttons.try_reserve(1)?;
        self.consumed_keys.try_reserve(1)?;
        self.stroke_windows.try_reserve(1)?;
        let user = self.inner.add_virtual_user(seat, resolved.clone())?;
        debug_assert_eq!(user, self.seats.len());
        self.document.append_user();
        self.seats.push(resolved);
        self.cursors.push(seat.cursor.clone());
        self.consumed_buttons.push(None);
        self.consumed_keys.push(BTreeSet::new());
        self.stroke_windows.push(None);
        Ok(user)
    }
    fn prepare_remove_virtual_user(&mut self, user: usize) -> Result<()> {
        self.document.is_open(user)?;
        self.inner.prepare_remove_virtual_user(user)
    }
    fn commit_remove_virtual_user(&mut self, user: usize) {
        self.document
            .configure(user, Settings::default())
            .expect("prepared annotation user and valid defaults");
        self.document
            .clear(user, false)
            .expect("prepared annotation user");
        self.consumed_buttons[user] = None;
        self.consumed_keys[user].clear();
        self.stroke_windows[user] = None;
        self.inner.commit_remove_virtual_user(user);
    }
    fn input(&mut self, packet: Packet) -> Result<()> {
        self.input_with_mask(packet, None)
    }
    fn input_with_source_buttons(&mut self, packet: Packet, source_buttons: u8) -> Result<()> {
        ensure!(source_buttons & !0x1f == 0, "invalid source button mask");
        self.input_with_mask(packet, Some(source_buttons))
    }
    fn button_output_disabled(&self, packet: Packet) -> bool {
        self.inner.button_output_disabled(packet)
    }
    fn key_held(&self, device: u32, key: u32) -> bool {
        self.inner.key_held(device, key)
            || self
                .seats
                .iter()
                .position(|s| s.keyboard == Some(device))
                .is_some_and(|u| self.consumed_keys[u].contains(&key))
    }
    fn paused(&self) -> bool {
        self.inner.paused()
    }
    fn button_held(&self, device: u32, button: u32) -> bool {
        self.inner.button_held(device, button)
            || (button < 5
                && self
                    .seats
                    .iter()
                    .position(|s| s.mouse == device)
                    .is_some_and(|u| self.consumed_buttons[u] == Some(1 << (button * 2))))
    }
    fn release_all(&mut self) -> Result<()> {
        self.end_all();
        self.inner.release_all()
    }
    fn pause(&mut self, value: bool) -> Result<()> {
        if value {
            self.end_all();
        }
        self.inner.pause(value)
    }
    fn settings(&mut self, user: usize, settings: CursorSettings) -> Result<()> {
        self.inner.settings(user, settings.clone())?;
        self.cursors[user] = settings;
        self.document.end(user)?;
        Ok(())
    }
    fn tick(&mut self) -> Result<(bool, Vec<RoutingState>)> {
        let result = self.inner.tick()?;
        for user in 0..self.seats.len() {
            if self.document.is_open(user)?
                && self.stroke_windows[user]
                    .is_some_and(|window| self.inner.selected_window(user).ok() != Some(window))
            {
                self.document.end(user)?;
                self.stroke_windows[user] = None;
            }
        }
        self.paint()?;
        Ok(result)
    }
    fn mode(&mut self, mode: Mode, multi_keyboard: bool) -> Result<()> {
        self.inner.mode(mode, multi_keyboard)?;
        self.end_all();
        Ok(())
    }
    fn move_to(&mut self, user: usize, x: i32, y: i32) -> Result<()> {
        self.inner.move_to(user, x, y)?;
        if !self.inner.paused() && !self.inner.user_pause_state(user).paused {
            self.sample(user)?;
        }
        Ok(())
    }
    fn pointer(&self, user: usize) -> Result<(i32, i32)> {
        self.inner.pointer(user)
    }
    fn selected_window(&self, user: usize) -> Result<Option<Window>> {
        self.inner.selected_window(user)
    }
    fn mouse_window(&self, user: usize) -> Result<Option<Window>> {
        self.inner.mouse_window(user)
    }
    fn keyboard_window(&self, user: usize) -> Result<Option<Window>> {
        self.inner.keyboard_window(user)
    }
    fn movement_window(&self, user: usize, x: i32, y: i32) -> Result<Option<Window>> {
        self.inner.movement_window(user, x, y)
    }
    fn packet_window(&self, user: usize, packet: Packet) -> Result<Option<Window>> {
        self.inner.packet_window(user, packet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    struct SelectionEngine(Rc<RefCell<Vec<usize>>>, [bool; 2], bool);

    impl Engine for SelectionEngine {
        fn user_pause_state(&self, user: usize) -> crate::routing_engine::UserPauseState {
            crate::routing_engine::UserPauseState {
                user,
                supported: user < 2,
                paused: self.1.get(user).copied().unwrap_or(false),
                ..Default::default()
            }
        }
        fn pause_user(&mut self, user: usize, paused: bool) -> Result<()> {
            self.1[user] = paused;
            ensure!(!self.2, "controlled cleanup failure after quarantine");
            Ok(())
        }
        fn select_window(&mut self, selection: &crate::window_selection::Selection) -> Result<()> {
            self.0.borrow_mut().push(selection.user);
            Ok(())
        }
        fn input(&mut self, _: Packet) -> Result<()> {
            panic!("selection must not emit input")
        }
        fn key_held(&self, _: u32, _: u32) -> bool {
            false
        }
        fn button_held(&self, _: u32, _: u32) -> bool {
            false
        }
        fn paused(&self) -> bool {
            false
        }
        fn release_all(&mut self) -> Result<()> {
            panic!("selection must not release input")
        }
        fn pause(&mut self, _: bool) -> Result<()> {
            panic!("selection must not pause")
        }
        fn settings(&mut self, _: usize, _: CursorSettings) -> Result<()> {
            panic!("selection must not change settings")
        }
        fn tick(&mut self) -> Result<(bool, Vec<RoutingState>)> {
            panic!("selection must not tick")
        }
        fn pointer(&self, _: usize) -> Result<(i32, i32)> {
            Ok((10, 20))
        }
        fn selected_window(&self, _: usize) -> Result<Option<Window>> {
            Ok(None)
        }
    }

    fn selection(user: usize) -> crate::window_selection::Selection {
        crate::window_selection::Selection {
            request_id: "annotation-test".into(),
            user,
            pid: 101,
            hwnd: 10,
            creation_token: "100".into(),
        }
    }

    fn runtime() -> (Runtime, Rc<RefCell<Vec<usize>>>) {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let seats = (0..2)
            .map(|user| ResolvedSeat {
                name: format!("annotation-{user}"),
                mouse: mux_core::emulated::device(mux_core::DeviceKind::Mouse, user + 1)
                    .unwrap()
                    .handle as u32,
                keyboard: Some(
                    mux_core::emulated::device(mux_core::DeviceKind::Keyboard, user + 1)
                        .unwrap()
                        .handle as u32,
                ),
                target_pid: 0,
            })
            .collect();
        let mut document = Document::new(2).unwrap();
        document
            .configure(
                0,
                Settings {
                    tool: Tool::Brush,
                    clear_key: Some(0x41),
                    ..Settings::default()
                },
            )
            .unwrap();
        (
            Runtime {
                inner: Box::new(SelectionEngine(calls.clone(), [false; 2], false)),
                seats,
                cursors: vec![CursorSettings::default(); 2],
                document,
                consumed_buttons: vec![None; 2],
                consumed_keys: vec![BTreeSet::new(); 2],
                overlay: None,
                painted_revision: None,
                painted_bounds: None,
                stroke_windows: vec![None; 2],
                export_pending: None,
            },
            calls,
        )
    }

    #[test]
    fn user_pause_ends_only_its_stroke_and_consumed_state_even_when_cleanup_fails() {
        for fail in [false, true] {
            let (mut runtime, calls) = runtime();
            runtime.inner = Box::new(SelectionEngine(calls, [false; 2], fail));
            runtime
                .document
                .configure(
                    1,
                    Settings {
                        tool: Tool::Brush,
                        ..Settings::default()
                    },
                )
                .unwrap();
            for user in 0..2 {
                runtime
                    .input(Packet::button(runtime.seats[user].mouse, 0, false))
                    .unwrap();
                runtime.consumed_keys[user].insert(65);
            }
            let peer_button = runtime.consumed_buttons[1];
            let result = runtime.pause_user(0, true);
            assert_eq!(result.is_err(), fail);
            assert!(runtime.user_pause_state(0).paused);
            assert!(!runtime.document.is_open(0).unwrap());
            assert!(runtime.consumed_buttons[0].is_none() && runtime.consumed_keys[0].is_empty());
            assert!(runtime.document.is_open(1).unwrap());
            assert_eq!(runtime.consumed_buttons[1], peer_button);
            assert!(runtime.consumed_keys[1].contains(&65));
            assert!(!runtime.user_pause_state(1).paused);
        }
    }
    #[test]
    fn selection_preserves_stroke_and_consumed_activator_until_real_release() {
        let (mut runtime, calls) = runtime();
        let mouse = runtime.seats[0].mouse;
        runtime.input(Packet::button(mouse, 0, false)).unwrap();
        assert!(runtime.document.is_open(0).unwrap());
        let before = serde_json::to_value(runtime.annotation_state()).unwrap();
        assert!(runtime.select_window(&selection(0)).is_err());
        assert_eq!(
            serde_json::to_value(runtime.annotation_state()).unwrap(),
            before
        );
        assert!(calls.borrow().is_empty());
        runtime.select_window(&selection(1)).unwrap();
        runtime.document.clear(0, false).unwrap();
        assert!(!runtime.document.is_open(0).unwrap());
        assert!(runtime.select_window(&selection(0)).is_err());
        assert_eq!(runtime.consumed_buttons[0], Some(1));
        runtime.input(Packet::button(mouse, 0, true)).unwrap();
        runtime.select_window(&selection(0)).unwrap();
        assert_eq!(*calls.borrow(), vec![1, 0]);
    }

    #[test]
    fn selection_preserves_consumed_clear_shortcut_until_real_release() {
        let (mut runtime, calls) = runtime();
        let keyboard = runtime.seats[0].keyboard.unwrap();
        runtime.input(Packet::key(keyboard, 0x41, false)).unwrap();
        let revision = runtime.document.revision();
        assert!(runtime.select_window(&selection(0)).is_err());
        runtime.select_window(&selection(1)).unwrap();
        assert!(runtime.consumed_keys[0].contains(&0x41));
        assert_eq!(runtime.document.revision(), revision);
        runtime.input(Packet::key(keyboard, 0x41, true)).unwrap();
        runtime.select_window(&selection(0)).unwrap();
        assert_eq!(*calls.borrow(), vec![1, 0]);
    }
}

#[cfg(test)]
mod export_tests {
    use super::*;

    #[test]
    fn existing_export_json_keeps_all_users_and_explicit_scope_round_trips() {
        let command: Command = serde_json::from_str(
            r#"{"operation":"export","user":1,"path":"ink.png","kind":"ink_only"}"#,
        )
        .unwrap();
        assert!(matches!(
            command,
            Command::Export {
                ink_scope: InkScope::AllUsers,
                ..
            }
        ));
        assert!(
            serde_json::from_str::<Command>(
                r#"{"operation":"export","user":1,"path":"desktop.png","kind":"full_desktop"}"#,
            )
            .is_ok()
        );
        assert!(serde_json::from_str::<Command>(
            r#"{"operation":"export","user":1,"path":"region.png","kind":"desktop_region","desktop_region":{"left":0,"top":0,"right":2,"bottom":2}}"#,
        ).is_ok());
        assert!(serde_json::from_str::<Command>(
            r#"{"operation":"export","user":1,"path":"ink.png","kind":"ink_only","ink_scope":null}"#,
        ).is_err());
        let historical: ExportInfo = serde_json::from_str(
            r#"{"path":"ink.png","width":2,"height":1,"kind":"ink_only","pid":null,"hwnd":null}"#,
        )
        .unwrap();
        assert_eq!(historical.user, 0);
        assert_eq!(historical.ink_scope, InkScope::AllUsers);
        assert_eq!(historical.revision, 0);
        assert_eq!(historical.bounds, None);
        let command = Command::Export {
            user: 1,
            path: "ink.png".into(),
            kind: ExportKind::SelectedTarget,
            ink_scope: InkScope::SelectedUser,
            desktop_region: None,
        };
        let json = serde_json::to_value(command).unwrap();
        assert_eq!(json["ink_scope"], "selected_user");
        assert_eq!(json["desktop_region"], serde_json::Value::Null);
        let command: Command = serde_json::from_value(json).unwrap();
        assert!(matches!(
            command,
            Command::Export {
                ink_scope: InkScope::SelectedUser,
                kind: ExportKind::SelectedTarget,
                ..
            }
        ));
    }

    #[test]
    fn desktop_export_kinds_require_explicit_valid_bounded_scope() {
        let desktop = Bounds {
            left: -10,
            top: 0,
            right: 30,
            bottom: 40,
        };
        let region = Bounds {
            left: 0,
            top: 5,
            right: 20,
            bottom: 25,
        };
        assert_eq!(
            resolve_desktop_export_bounds(ExportKind::FullDesktop, None, desktop).unwrap(),
            Some(desktop)
        );
        assert_eq!(
            resolve_desktop_export_bounds(ExportKind::DesktopRegion, Some(region), desktop)
                .unwrap(),
            Some(region)
        );
        assert!(resolve_desktop_export_bounds(ExportKind::DesktopRegion, None, desktop).is_err());
        assert!(
            resolve_desktop_export_bounds(
                ExportKind::DesktopRegion,
                Some(Bounds {
                    left: -11,
                    ..region
                }),
                desktop
            )
            .is_err()
        );
        assert!(
            resolve_desktop_export_bounds(ExportKind::FullDesktop, Some(region), desktop).is_err()
        );
        assert!(resolve_desktop_export_bounds(ExportKind::InkOnly, Some(region), desktop).is_err());
        assert!(
            resolve_desktop_export_bounds(
                ExportKind::DesktopRegion,
                Some(Bounds {
                    left: 0,
                    top: 0,
                    right: 5000,
                    bottom: 5000
                }),
                Bounds {
                    left: 0,
                    top: 0,
                    right: 6000,
                    bottom: 6000
                },
            )
            .is_err()
        );
    }

    #[test]
    fn desktop_composition_blends_ink_once_over_opaque_capture() {
        let mut pixels = [10, 20, 30, 255];
        composite_ink(&mut pixels, &[110, 120, 130, 128]).unwrap();
        assert_eq!(pixels, [60, 70, 80, 255]);
        assert!(composite_ink(&mut pixels, &[0, 0, 0]).is_err());
    }
}
