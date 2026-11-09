//! Session-owned magnifiers. Native capture and pixel rendering never run on the input thread.
use crate::{annotation_runtime, router::State as RoutingState, routing_engine::Engine};
use anyhow::{Context, Result, ensure};
use mux_core::{
    Profile, ResolvedSeat,
    desktop::{Bounds, BoundsMode, CursorSettings},
    emulated::Packet,
    magnifier::{self, Mode, Settings, Source},
    routing::{Mode as RoutingMode, Window},
};
use mux_windows::{
    magnifier_overlay::{MagnifierOverlay, MagnifierOverlayStatus},
    targets,
};
use rmcp::schemars;
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Inspect,
    Configure { user: usize, settings: Settings },
    Refresh { user: usize },
    Export { user: usize, path: String },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportInfo {
    pub user: usize,
    pub path: String,
    pub width: u32,
    pub height: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserState {
    pub user: usize,
    pub settings: Settings,
    pub status: String,
    pub source_bounds: Option<Bounds>,
    pub source_hwnd: Option<usize>,
    pub source_pid: Option<u32>,
    pub cursor: Option<(i32, i32)>,
    pub frame_cursor: Option<(i32, i32)>,
    pub capture_sequence: u64,
    pub frame_age_ms: Option<u64>,
    pub in_flight: bool,
    pub overlay: Option<MagnifierOverlayStatus>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub acknowledged: bool,
    pub users: Vec<UserState>,
    pub export: Option<ExportInfo>,
    pub error: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceKey {
    Window(Window, Bounds),
    Desktop(Bounds),
}
impl SourceKey {
    fn bounds(self) -> Bounds {
        match self {
            Self::Window(_, b) | Self::Desktop(b) => b,
        }
    }
    fn window(self) -> Option<Window> {
        match self {
            Self::Window(w, _) => Some(w),
            _ => None,
        }
    }
}
struct Job {
    generation: u64,
    source: SourceKey,
    cursor: (i32, i32),
    output: Bounds,
    settings: Settings,
    capture: bool,
    ink: Option<(Bounds, Vec<u8>)>,
}
struct Frame {
    source_bounds: Bounds,
    cursor: (i32, i32),
    output: Bounds,
    rgba: Arc<Vec<u8>>,
}
/// Immutable completed pixels, shared with the connected-agent export worker.
pub(crate) struct ExportFrame {
    pub user: usize,
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
}
type CachedSource = (SourceKey, Bounds, Vec<u8>, (i32, i32));
struct Completed {
    generation: u64,
    source: SourceKey,
    capture: bool,
    result: Result<Frame>,
}
struct CaptureWorker {
    send: Sender<Job>,
    receive: Receiver<Completed>,
    started: Option<Instant>,
}
impl CaptureWorker {
    fn new(user: usize) -> Result<Self> {
        let (send, jobs) = mpsc::channel::<Job>();
        let (results, receive) = mpsc::channel();
        thread::Builder::new()
            .name(format!("magnifier-{user}"))
            .spawn(move || {
                let mut cached: Option<CachedSource> = None;
                while let Ok(job) = jobs.recv() {
                    let result = (|| -> Result<Frame> {
                        if job.capture {
                            let (bounds, mut rgba) = match job.source {
                                SourceKey::Window(window, expected) => {
                                    let frame = mux_windows::target_capture::client(window)?;
                                    ensure!(
                                        frame.0 == expected,
                                        "target moved during magnifier capture"
                                    );
                                    frame
                                }
                                SourceKey::Desktop(authority) => {
                                    mux_windows::desktop_capture::region(
                                        magnifier::capture_region(
                                            &job.settings,
                                            authority,
                                            job.cursor,
                                            job.output,
                                        )?,
                                    )?
                                }
                            };
                            if let Some((ink_bounds, ink)) = &job.ink {
                                composite(&mut rgba, bounds, ink, *ink_bounds);
                            }
                            cached = Some((job.source, bounds, rgba, job.cursor));
                        }
                        let (key, bounds, rgba, anchor) = cached
                            .as_ref()
                            .context("refresh the magnifier to capture a frame")?;
                        ensure!(
                            *key == job.source,
                            "magnifier source changed; refresh required"
                        );
                        // Manual desktop mode preserves the captured ROI. Window mode can pan its cached client.
                        let cursor = if matches!(key, SourceKey::Desktop(_)) && !job.capture {
                            *anchor
                        } else {
                            job.cursor
                        };
                        Ok(Frame {
                            source_bounds: *bounds,
                            cursor,
                            output: job.output,
                            rgba: magnifier::render(
                                rgba,
                                *bounds,
                                cursor,
                                job.output,
                                &job.settings,
                            )?
                            .into(),
                        })
                    })();
                    if results
                        .send(Completed {
                            generation: job.generation,
                            source: job.source,
                            capture: job.capture,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        Ok(Self {
            send,
            receive,
            started: None,
        })
    }
}
fn composite(background: &mut [u8], bounds: Bounds, ink: &[u8], ink_bounds: Bounds) {
    let Some(intersection) = bounds.intersection(ink_bounds) else {
        return;
    };
    let width = (bounds.right - bounds.left) as usize;
    let ink_width = (ink_bounds.right - ink_bounds.left) as usize;
    for y in intersection.top..intersection.bottom {
        for x in intersection.left..intersection.right {
            let dst = (((y - bounds.top) as usize * width) + (x - bounds.left) as usize) * 4;
            let src =
                (((y - ink_bounds.top) as usize * ink_width) + (x - ink_bounds.left) as usize) * 4;
            let alpha = u32::from(ink[src + 3]);
            for channel in 0..3 {
                background[dst + channel] = ((u32::from(ink[src + channel]) * alpha
                    + u32::from(background[dst + channel]) * (255 - alpha)
                    + 127)
                    / 255) as u8;
            }
            background[dst + 3] = 255;
        }
    }
}
struct User {
    settings: Settings,
    generation: u64,
    source: Option<SourceKey>,
    cursor: Option<(i32, i32)>,
    frame: Option<Frame>,
    captured: Option<Instant>,
    sequence: u64,
    worker: Option<CaptureWorker>,
    overlay: Option<MagnifierOverlay>,
    refresh: bool,
    status: String,
    activity: u64,
    frame_revision: u64,
    painted_revision: Option<u64>,
}
pub struct Runtime {
    inner: annotation_runtime::Runtime,
    users: Vec<User>,
    states: Vec<RoutingState>,
    activity: u64,
    seats: Vec<ResolvedSeat>,
    owned_overlay_epoch: u64,
}

pub fn validate_settings(settings: &Settings) -> Result<()> {
    settings.validate()?;
    if settings.mode == Mode::Off {
        return Ok(());
    }
    magnifier::placement(settings, placement_area(settings)?, (0, 0))?;
    if settings.source == Source::Desktop {
        desktop_authority(settings)?;
    }
    Ok(())
}
fn placement_area(settings: &Settings) -> Result<Bounds> {
    if settings.monitor_id.is_some() {
        mux_windows::monitors::resolve(
            &CursorSettings {
                bounds_mode: BoundsMode::Monitor,
                monitor_id: settings.monitor_id.clone(),
                monitor_work_area: settings.work_area,
                ..Default::default()
            },
            None,
        )
    } else {
        Ok(targets::desktop_bounds())
    }
}
fn desktop_authority(settings: &Settings) -> Result<Bounds> {
    let desktop = targets::desktop_bounds();
    let region = settings.desktop_region.unwrap_or(desktop);
    ensure!(
        region.valid_region() && desktop.intersection(region) == Some(region),
        "magnifier desktop region lies outside the physical virtual desktop"
    );
    Ok(region)
}
impl Runtime {
    pub fn new(
        inner: annotation_runtime::Runtime,
        profile: &Profile,
        seats: Vec<ResolvedSeat>,
    ) -> Result<Self> {
        let mut runtime = Self {
            inner,
            users: profile
                .seats
                .iter()
                .map(|seat| User {
                    settings: seat.magnifier.clone(),
                    generation: 0,
                    source: None,
                    cursor: None,
                    frame: None,
                    captured: None,
                    sequence: 0,
                    worker: None,
                    overlay: None,
                    refresh: true,
                    status: "Disabled".into(),
                    activity: 0,
                    frame_revision: 0,
                    painted_revision: None,
                })
                .collect(),
            states: Vec::new(),
            activity: 0,
            seats,
            owned_overlay_epoch: 0,
        };
        for user in 0..runtime.users.len() {
            let settings = runtime.users[user].settings.clone();
            runtime.configure(user, settings)?;
        }
        Ok(runtime)
    }
    pub fn as_mut(&mut self) -> &mut dyn Engine {
        self
    }
    pub(crate) fn export_generation(&self, user: usize) -> Option<u64> {
        self.users.get(user).map(|state| state.generation)
    }
    pub(crate) fn export_frame(&self, user: usize, expected: Window) -> Result<ExportFrame> {
        let state = self.users.get(user).context("Unknown magnifier user")?;
        ensure!(
            state.settings.mode != Mode::Off && state.settings.source == Source::SelectedTarget,
            "Agent export requires an enabled selected-target magnifier"
        );
        ensure!(
            self.inner.selected_window(user)? == Some(expected)
                && state.source.and_then(SourceKey::window) == Some(expected),
            "Magnifier frame no longer matches the selected target"
        );
        let frame = state
            .frame
            .as_ref()
            .context("Refresh the magnifier before exporting")?;
        Ok(ExportFrame {
            user,
            generation: state.generation,
            width: (frame.output.right - frame.output.left) as u32,
            height: (frame.output.bottom - frame.output.top) as u32,
            rgba: frame.rgba.clone(),
        })
    }
    pub fn annotation_state(&self) -> annotation_runtime::State {
        self.inner.annotation_state()
    }
    pub fn annotation_export_snapshot(
        &mut self,
        user: usize,
        kind: annotation_runtime::ExportKind,
        ink_scope: annotation_runtime::InkScope,
        desktop_region: Option<Bounds>,
    ) -> Result<annotation_runtime::ExportSnapshot> {
        if matches!(
            kind,
            annotation_runtime::ExportKind::FullDesktop
                | annotation_runtime::ExportKind::DesktopRegion
        ) {
            self.states = self.inner.tick()?.1;
            self.annotation_validate_desktop_export(targets::desktop_bounds())?;
        }
        let mut snapshot = self
            .inner
            .export_snapshot(user, kind, ink_scope, desktop_region)?;
        snapshot.desktop_overlays = self.annotation_desktop_overlay_binding();
        Ok(snapshot)
    }
    pub(crate) fn annotation_desktop_overlay_binding(
        &self,
    ) -> annotation_runtime::DesktopOverlayBinding {
        let mut hwnds: Vec<_> = self
            .states
            .iter()
            .map(|state| state.overlay.hwnd)
            .chain(
                self.inner
                    .annotation_state()
                    .overlay
                    .map(|overlay| overlay.hwnd),
            )
            .chain(
                self.users
                    .iter()
                    .filter_map(|user| user.overlay.as_ref().map(|overlay| overlay.status().hwnd)),
            )
            .filter(|hwnd| *hwnd != 0)
            .collect();
        hwnds.sort_unstable();
        hwnds.dedup();
        annotation_runtime::DesktopOverlayBinding {
            epoch: self.owned_overlay_epoch,
            hwnds,
        }
    }
    pub(crate) fn annotation_validate_overlay_binding(
        &mut self,
        expected: &annotation_runtime::DesktopOverlayBinding,
    ) -> Result<()> {
        // Refresh cursor windows before comparing; a cached routing frame may predate a clone.
        self.states = self.inner.tick()?.1;
        ensure!(
            self.annotation_desktop_overlay_binding() == *expected,
            "Owned overlay lifetime changed during desktop screenshot preparation; retry"
        );
        Ok(())
    }
    pub fn annotation_validate_desktop_export(&mut self, expected_extent: Bounds) -> Result<()> {
        self.exclude_owned(None)?;
        self.inner.validate_desktop_export(expected_extent)
    }
    pub fn set_annotation_export_pending(
        &mut self,
        pending: Option<annotation_runtime::ExportProgress>,
    ) {
        self.inner.set_export_pending(pending);
    }
    pub fn annotation_selected_window(&self, user: usize) -> Result<Option<Window>> {
        self.inner.selected_window(user)
    }
    pub fn annotation_configuration_idle(&self, user: usize) -> Result<bool> {
        self.inner.configuration_idle(user)
    }
    pub fn annotation_command(
        &mut self,
        command: annotation_runtime::Command,
    ) -> Result<annotation_runtime::State> {
        if matches!(
            &command,
            annotation_runtime::Command::Export {
                kind: annotation_runtime::ExportKind::FullDesktop
                    | annotation_runtime::ExportKind::DesktopRegion,
                ..
            }
        ) {
            self.states = self.inner.tick()?.1;
            self.annotation_validate_desktop_export(targets::desktop_bounds())?;
        }
        self.inner.annotation_command(command)
    }
    pub fn magnifier_state(&self) -> State {
        State {
            acknowledged: false,
            export: None,
            error: None,
            users: self
                .users
                .iter()
                .enumerate()
                .map(|(user, state)| UserState {
                    user,
                    settings: state.settings.clone(),
                    status: state.status.clone(),
                    source_bounds: state
                        .frame
                        .as_ref()
                        .map(|f| f.source_bounds)
                        .or(state.source.map(SourceKey::bounds)),
                    source_hwnd: state.source.and_then(SourceKey::window).map(|w| w.hwnd),
                    source_pid: state.source.and_then(SourceKey::window).map(|w| w.pid),
                    cursor: state.cursor,
                    frame_cursor: state.frame.as_ref().map(|f| f.cursor),
                    capture_sequence: state.sequence,
                    frame_age_ms: state
                        .captured
                        .map(|time| time.elapsed().as_millis().min(u128::from(u64::MAX)) as u64),
                    in_flight: state.worker.as_ref().is_some_and(|w| w.started.is_some()),
                    overlay: state.overlay.as_ref().map(|o| o.status().clone()),
                })
                .collect(),
        }
    }
    fn exclude_owned(&self, candidate: Option<&MagnifierOverlay>) -> Result<()> {
        for hwnd in self
            .annotation_desktop_overlay_binding()
            .hwnds
            .into_iter()
            .chain(candidate.map(|o| o.status().hwnd))
        {
            ensure!(
                mux_windows::desktop_capture::exclude_window(hwnd)?,
                "this Windows session cannot exclude owned overlays from desktop capture"
            );
        }
        Ok(())
    }
    fn configure(&mut self, user: usize, settings: Settings) -> Result<()> {
        self.users.get(user).context("Unknown magnifier user")?;
        validate_settings(&settings)?;
        let candidate = if settings.mode != Mode::Off {
            let cursor = self.inner.pointer(user)?;
            let candidate = MagnifierOverlay::new(magnifier::placement(
                &settings,
                placement_area(&settings)?,
                cursor,
            )?)?;
            if settings.source == Source::Desktop {
                if self.states.is_empty() {
                    self.states = self.inner.tick()?.1;
                }
                self.exclude_owned(Some(&candidate))?;
            }
            Some(candidate)
        } else {
            None
        };
        let next_overlay_epoch = self
            .owned_overlay_epoch
            .checked_add(1)
            .context("owned overlay lifetime counter exhausted")?;
        self.activity += 1;
        let state = &mut self.users[user];
        state.settings = settings;
        state.generation = state.generation.wrapping_add(1);
        state.source = None;
        state.frame = None;
        state.captured = None;
        state.overlay = candidate;
        self.owned_overlay_epoch = next_overlay_epoch;
        state.painted_revision = None;
        state.refresh = true;
        state.activity = self.activity;
        state.status = if state.settings.mode == Mode::Off {
            "Disabled"
        } else {
            "Waiting for source"
        }
        .into();
        Ok(())
    }
    pub fn magnifier_command(&mut self, command: Command) -> Result<State> {
        let mut export = None;
        match command {
            Command::Inspect => (),
            Command::Configure { user, settings } => self.configure(user, settings)?,
            Command::Refresh { user } => {
                let state = self.users.get_mut(user).context("Unknown magnifier user")?;
                ensure!(state.settings.mode != Mode::Off, "magnifier is disabled");
                ensure!(
                    !self.inner.paused() && !self.inner.user_pause_state(user).paused,
                    "resume routing before refreshing the magnifier"
                );
                state.refresh = true;
            }
            Command::Export { user, path } => {
                let state = self.users.get(user).context("Unknown magnifier user")?;
                ensure!(
                    Path::new(&path)
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("png")),
                    "magnifier export needs a .png path"
                );
                let frame = state
                    .frame
                    .as_ref()
                    .context("refresh the magnifier before exporting")?;
                let width = (frame.output.right - frame.output.left) as u32;
                let height = (frame.output.bottom - frame.output.top) as u32;
                image::save_buffer_with_format(
                    &path,
                    &frame.rgba,
                    width,
                    height,
                    image::ColorType::Rgba8,
                    image::ImageFormat::Png,
                )
                .context("save magnifier PNG")?;
                export = Some(ExportInfo {
                    user,
                    path,
                    width,
                    height,
                });
            }
        }
        let mut state = self.magnifier_state();
        state.acknowledged = true;
        state.export = export;
        Ok(state)
    }
    fn invalidate(&mut self, user: usize, status: &str) -> Result<()> {
        let state = &mut self.users[user];
        if state.source.is_some() || state.frame.is_some() {
            state.generation = state.generation.wrapping_add(1);
        }
        state.source = None;
        state.frame = None;
        state.captured = None;
        state.refresh = true;
        state.status = status.into();
        discard_completed(state);
        if let Some(overlay) = &mut state.overlay {
            overlay.hide()?;
        }
        Ok(())
    }
    fn update_user(&mut self, user: usize) -> Result<()> {
        let settings = self.users[user].settings.clone();
        if settings.mode == Mode::Off {
            discard_completed(&mut self.users[user]);
            return Ok(());
        }
        if self.inner.paused() || self.inner.user_pause_state(user).paused {
            return self.invalidate(user, "Paused");
        }
        let cursor = self.inner.pointer(user)?;
        let output = magnifier::placement(&settings, placement_area(&settings)?, cursor)?;
        let source = match settings.source {
            Source::SelectedTarget => {
                let Some(window) = self.inner.selected_window(user)? else {
                    return self.invalidate(user, "Waiting for source");
                };
                use windows_sys::Win32::UI::WindowsAndMessaging::{GA_ROOT, GetAncestor, IsIconic};
                ensure!(
                    unsafe { IsIconic(GetAncestor(window.hwnd as _, GA_ROOT)) } == 0,
                    "selected window is minimized"
                );
                SourceKey::Window(window, targets::client_bounds(window.hwnd, window.pid)?)
            }
            Source::Desktop => {
                self.exclude_owned(None)?;
                SourceKey::Desktop(desktop_authority(&settings)?)
            }
        };
        let state = &mut self.users[user];
        if state.cursor != Some(cursor) {
            self.activity += 1;
            state.activity = self.activity;
        }
        state.cursor = Some(cursor);
        if state.source != Some(source) {
            state.generation = state.generation.wrapping_add(1);
            state.source = Some(source);
            state.frame = None;
            state.captured = None;
            state.refresh = true;
            if let Some(overlay) = &mut state.overlay {
                overlay.hide()?;
            }
        }
        if let Some(worker) = &mut state.worker {
            while let Ok(done) = worker.receive.try_recv() {
                worker.started = None;
                if done.generation != state.generation || done.source != source {
                    continue;
                }
                match done.result {
                    Ok(frame) => {
                        if done.capture {
                            state.sequence += 1;
                            state.captured = Some(Instant::now());
                        }
                        state.frame = Some(frame);
                        state.frame_revision = state.frame_revision.wrapping_add(1);
                        state.status = "Ready".into();
                    }
                    Err(error) => {
                        state.status = format!("Capture failed: {error:#}");
                        state.frame = None;
                        if let Some(overlay) = &mut state.overlay {
                            overlay.hide()?;
                        }
                    }
                }
            }
            if worker
                .started
                .is_some_and(|time| time.elapsed() > Duration::from_secs(2))
            {
                state.status = "Capture delayed".into();
                if let Some(overlay) = &mut state.overlay {
                    overlay.hide()?;
                }
            }
            if worker.started.is_some() {
                return Ok(());
            }
        }
        let capture = state.refresh
            || (settings.auto_refresh
                && state.captured.is_none_or(|time| {
                    time.elapsed() >= Duration::from_secs_f64(1.0 / f64::from(settings.refresh_hz))
                }));
        let render = state.frame.as_ref().is_some_and(|f| {
            f.output != output || (settings.source == Source::SelectedTarget && f.cursor != cursor)
        });
        if capture || render {
            let ink_bounds = if capture {
                Some(if settings.source == Source::SelectedTarget {
                    source.bounds()
                } else {
                    magnifier::capture_region(&settings, source.bounds(), cursor, output)?
                })
            } else {
                None
            };
            let ink = if let Some(bounds) = ink_bounds {
                self.inner
                    .annotation_pixels(bounds)?
                    .map(|pixels| (bounds, pixels))
            } else {
                None
            };
            let state = &mut self.users[user];
            if state.worker.is_none() {
                state.worker = Some(CaptureWorker::new(user)?);
            }
            let worker = state.worker.as_mut().unwrap();
            worker
                .send
                .send(Job {
                    generation: state.generation,
                    source,
                    cursor,
                    output,
                    settings,
                    capture,
                    ink,
                })
                .context("magnifier capture worker stopped")?;
            worker.started = Some(Instant::now());
            state.refresh = false;
            if state.frame.is_none() {
                state.status = "Capturing".into();
            }
        }
        Ok(())
    }
    fn paint(&mut self) -> Result<()> {
        for user in 0..self.users.len() {
            let state = &self.users[user];
            let waiting = state.settings.mode == Mode::Fullscreen
                && state.frame.as_ref().is_some_and(|frame| {
                    self.users.iter().enumerate().any(|(other, candidate)| {
                        other != user
                            && candidate.settings.mode == Mode::Fullscreen
                            && candidate.activity > state.activity
                            && candidate
                                .frame
                                .as_ref()
                                .is_some_and(|f| f.output.intersection(frame.output).is_some())
                    })
                });
            let state = &mut self.users[user];
            if waiting {
                state.status = "Waiting for display; another fullscreen user is active".into();
            } else if state.status.starts_with("Waiting for display") {
                state.status = "Ready".into();
            }
            if let Some(overlay) = &mut state.overlay {
                if waiting || state.status == "Capture delayed" || self.inner.paused() {
                    if overlay.status().visible {
                        overlay.hide()?;
                    }
                    state.painted_revision = None;
                } else if let Some(frame) = &state.frame
                    && state.painted_revision != Some(state.frame_revision)
                {
                    overlay.draw(frame.output, &frame.rgba)?;
                    state.painted_revision = Some(state.frame_revision);
                }
            }
        }
        // Reposition only our rendered cursor HWNDs through the visible fullscreen camera.
        // Logical routing coordinates and the operating system pointer remain untouched.
        for routing in &mut self.states {
            // An opaque lens or dock must never cover another user's cursor.
            // Only HWNDs produced by the inner router are raised, without activation.
            if routing.overlay.visible
                && self.users.iter().any(|user| {
                    user.overlay
                        .as_ref()
                        .is_some_and(|overlay| overlay.status().visible)
                })
            {
                use windows_sys::Win32::UI::WindowsAndMessaging::*;
                ensure!(
                    unsafe {
                        SetWindowPos(
                            routing.overlay.hwnd as _,
                            HWND_TOPMOST,
                            0,
                            0,
                            0,
                            0,
                            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
                        )
                    } != 0,
                    "raise virtual cursor above magnifier"
                );
            }
            let point = (routing.overlay.x, routing.overlay.y);
            let camera = self
                .users
                .iter()
                .filter(|u| {
                    u.settings.mode == Mode::Fullscreen
                        && u.frame
                            .as_ref()
                            .is_some_and(|f| f.output.contains(point.0, point.1))
                        && u.overlay.as_ref().is_some_and(|o| o.status().visible)
                })
                .max_by_key(|u| u.activity);
            let Some(camera) = camera else {
                continue;
            };
            let Some(frame) = &camera.frame else {
                continue;
            };
            if !frame.output.contains(point.0, point.1) {
                continue;
            }
            let anchor = frame.cursor;
            let x = ((frame.output.left + frame.output.right) as f64 / 2.0
                + f64::from(point.0 - anchor.0) * camera.settings.zoom)
                .round() as i32;
            let y = ((frame.output.top + frame.output.bottom) as f64 / 2.0
                + f64::from(point.1 - anchor.1) * camera.settings.zoom)
                .round() as i32;
            let origin = (x - routing.overlay.hotspot.0, y - routing.overlay.hotspot.1);
            use windows_sys::Win32::UI::WindowsAndMessaging::*;
            unsafe {
                if routing.overlay.visible && frame.output.contains(x, y) {
                    ensure!(
                        SetWindowPos(
                            routing.overlay.hwnd as _,
                            HWND_TOPMOST,
                            origin.0,
                            origin.1,
                            0,
                            0,
                            SWP_NOACTIVATE | SWP_NOSIZE | SWP_SHOWWINDOW
                        ) != 0,
                        "place magnified virtual cursor"
                    );
                    routing.overlay.render_origin = origin;
                } else {
                    ShowWindow(routing.overlay.hwnd as _, SW_HIDE);
                    routing.overlay.visible = false;
                }
            }
        }
        Ok(())
    }
}
fn discard_completed(state: &mut User) {
    if let Some(worker) = &mut state.worker {
        while worker.receive.try_recv().is_ok() {
            worker.started = None;
        }
    }
}
impl Engine for Runtime {
    fn desktop_observation(&mut self, operation: crate::desktop_router::observation::Operation) -> Vec<crate::desktop_router::observation::Delivery> { self.inner.desktop_observation(operation) }
    fn user_pause_state(&self, user: usize) -> crate::routing_engine::UserPauseState {
        self.inner.user_pause_state(user)
    }
    fn pause_user(&mut self, user: usize, paused: bool) -> Result<()> {
        let result = self.inner.pause_user(user, paused);
        if paused && self.inner.user_pause_state(user).paused {
            result.and(self.invalidate(user, "Paused"))
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
        let accepted = self.inner.forwarded_motion(packet, source_buttons)?;
        if accepted
            && let Some(user) = self
                .seats
                .iter()
                .position(|s| s.mouse == packet.device || s.keyboard == Some(packet.device))
        {
            self.activity += 1;
            self.users[user].activity = self.activity;
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
        self.inner.annotation_clears_all(user, packet)
    }
    fn note_mouse_activity(&mut self, user: usize) -> Result<()> {
        self.inner.note_mouse_activity(user)
    }
    fn shutdown(&mut self) -> Result<()> {
        self.inner.shutdown()
    }
    fn add_virtual_user(&mut self, seat: &mux_core::Seat, resolved: ResolvedSeat) -> Result<usize> {
        ensure!(
            seat.magnifier.mode == Mode::Off,
            "Generated users start with magnification disabled"
        );
        self.users.try_reserve(1)?;
        self.seats.try_reserve(1)?;
        let next_overlay_epoch = self
            .owned_overlay_epoch
            .checked_add(1)
            .context("owned overlay lifetime counter exhausted")?;
        let state = User {
            settings: Settings::default(),
            generation: 0,
            source: None,
            cursor: None,
            frame: None,
            captured: None,
            sequence: 0,
            worker: None,
            overlay: None,
            refresh: false,
            status: "Disabled".into(),
            activity: 0,
            frame_revision: 0,
            painted_revision: None,
        };
        let user = self.inner.add_virtual_user(seat, resolved.clone())?;
        self.owned_overlay_epoch = next_overlay_epoch;
        debug_assert_eq!(user, self.users.len());
        self.users.push(state);
        self.seats.push(resolved);
        Ok(user)
    }
    fn prepare_remove_virtual_user(&mut self, user: usize) -> Result<()> {
        self.users.get(user).context("unknown magnifier user")?;
        self.owned_overlay_epoch
            .checked_add(1)
            .context("owned overlay lifetime counter exhausted")?;
        self.inner.prepare_remove_virtual_user(user)
    }
    fn commit_remove_virtual_user(&mut self, user: usize) {
        self.owned_overlay_epoch += 1;
        // Dropping the owned window destroys it. There is no window left to hide.
        self.users[user].overlay = None;
        self.invalidate(user, "Virtual user removed")
            .expect("removed magnifier overlay");
        let state = &mut self.users[user];
        state.settings = Settings::default();
        state.worker = None;
        self.inner.commit_remove_virtual_user(user);
    }
    fn pointer(&self, user: usize) -> Result<(i32, i32)> {
        self.inner.pointer(user)
    }
    fn selected_window(&self, user: usize) -> Result<Option<Window>> {
        self.inner.selected_window(user)
    }
    fn input(&mut self, packet: Packet) -> Result<()> {
        self.inner.input(packet)?;
        if let Some(user) = self
            .seats
            .iter()
            .position(|s| s.mouse == packet.device || s.keyboard == Some(packet.device))
        {
            self.activity += 1;
            self.users[user].activity = self.activity;
        }
        Ok(())
    }
    fn input_with_source_buttons(&mut self, packet: Packet, source_buttons: u8) -> Result<()> {
        self.inner
            .input_with_source_buttons(packet, source_buttons)?;
        if let Some(user) = self
            .seats
            .iter()
            .position(|s| s.mouse == packet.device || s.keyboard == Some(packet.device))
        {
            self.activity += 1;
            self.users[user].activity = self.activity;
        }
        Ok(())
    }
    fn button_output_disabled(&self, packet: Packet) -> bool {
        self.inner.button_output_disabled(packet)
    }
    fn key_held(&self, device: u32, key: u32) -> bool {
        self.inner.key_held(device, key)
    }
    fn paused(&self) -> bool {
        self.inner.paused()
    }
    fn button_held(&self, device: u32, button: u32) -> bool {
        self.inner.button_held(device, button)
    }
    fn release_all(&mut self) -> Result<()> {
        self.inner.release_all()
    }
    fn pause(&mut self, value: bool) -> Result<()> {
        self.inner.pause(value)?;
        if value {
            for user in 0..self.users.len() {
                self.invalidate(user, "Paused")?;
            }
        }
        Ok(())
    }
    fn settings(&mut self, user: usize, settings: CursorSettings) -> Result<()> {
        self.inner.settings(user, settings)
    }
    fn mode(&mut self, mode: RoutingMode, keyboard: bool) -> Result<()> {
        self.inner.mode(mode, keyboard)
    }
    fn move_to(&mut self, user: usize, x: i32, y: i32) -> Result<()> {
        self.inner.move_to(user, x, y)?;
        self.activity += 1;
        self.users[user].activity = self.activity;
        Ok(())
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
    fn tick(&mut self) -> Result<(bool, Vec<RoutingState>)> {
        let (stop, states) = self.inner.tick()?;
        self.states = states;
        if !stop {
            for user in 0..self.users.len() {
                if let Err(error) = self.update_user(user) {
                    self.invalidate(user, &format!("Source unavailable: {error:#}"))?;
                }
            }
            self.paint()?;
        }
        Ok((stop, self.states.clone()))
    }
}
