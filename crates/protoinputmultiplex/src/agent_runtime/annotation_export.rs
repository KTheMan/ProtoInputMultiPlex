//! One retained annotation export shared by local and connected callers.
use super::*;
use crate::annotation_runtime::{
    Command as AnnotationCommand, ExportInfo, ExportKind, ExportProgress, InkScope,
};
use mux_core::{desktop::Bounds, routing::Window};

const PREPARATION_BUDGET: Duration = Duration::from_secs(5);
const PUBLICATION_BUDGET: Duration = Duration::from_secs(4);
// Leave a margin before the ordinary session supervisor's ten-second command limit.
const REQUEST_BUDGET: Duration = Duration::from_secs(9);

#[derive(Clone, PartialEq, Eq)]
struct Guard {
    user: usize,
    user_id: String,
    ink_users: BTreeSet<String>,
    kind: ExportKind,
    ink_scope: InkScope,
    revision: u64,
    bounds: Bounds,
    desktop_bounds: Option<Bounds>,
    desktop_overlays: crate::annotation_runtime::DesktopOverlayBinding,
    window: Option<Window>,
}

#[derive(Clone)]
struct PreparedExport {
    info: ExportInfo,
    guard: Guard,
}

pub(super) struct Pending {
    tag: (u64, u64),
    guard: Guard,
    policy_generation: u64,
    process: Option<mux_windows::targets::ProcessWatch>,
    path: String,
    started: Instant,
    replied: bool,
    decided: bool,
    approved_at: Option<Instant>,
    progress_sent: bool,
    job: crate::agent_export::ExportJob<PreparedExport>,
}

fn ink_users(users: &virtual_users::Runtime) -> BTreeSet<String> {
    users
        .state()
        .slots
        .iter()
        .filter(|slot| slot.active)
        .map(|slot| slot.seat.identity())
        .collect()
}

impl Runtime {
    fn authorize_annotation_export_scope(
        &self,
        user: usize,
        kind: ExportKind,
        scope: InkScope,
        users: &virtual_users::Runtime,
    ) -> Result<()> {
        if matches!(kind, ExportKind::FullDesktop | ExportKind::DesktopRegion) {
            ensure!(
                self.policy.desktop_capture,
                "Desktop screenshots are disabled in Agent Access"
            );
        } else {
            ensure!(
                self.policy.capture,
                "PNG export is disabled; allow target capture in Agent Access"
            );
        }
        self.user(user, users)?;
        if scope == InkScope::AllUsers {
            self.require_all_users(users)?;
        }
        Ok(())
    }

    pub fn begin_annotation_export(
        &mut self,
        tag: (u64, u64),
        command: AnnotationCommand,
        users: &virtual_users::Runtime,
        router: &mut crate::magnifier_runtime::Runtime,
    ) -> Result<()> {
        ensure!(
            self.annotation_export.is_none(),
            "An annotation export is still in progress"
        );
        let AnnotationCommand::Export {
            user,
            path,
            kind,
            ink_scope,
            desktop_region,
        } = command
        else {
            bail!("Expected an annotation Export command");
        };
        users.require_active(user)?;
        if tag.0 != 0 {
            self.require(tag.0)?;
            self.authorize_annotation_export_scope(user, kind, ink_scope, users)?;
            if kind == ExportKind::SelectedTarget {
                self.target(user, router)?;
            }
        }
        let snapshot = router.annotation_export_snapshot(user, kind, ink_scope, desktop_region)?;
        let guard = Guard {
            user,
            user_id: users.require_active(user)?.seat.identity(),
            ink_users: if ink_scope == InkScope::AllUsers {
                ink_users(users)
            } else {
                BTreeSet::new()
            },
            kind,
            ink_scope,
            revision: snapshot.revision,
            bounds: snapshot.bounds,
            desktop_bounds: snapshot.desktop_bounds,
            desktop_overlays: snapshot.desktop_overlays.clone(),
            window: snapshot.window,
        };
        let process = guard
            .window
            .map(|w| mux_windows::targets::ProcessWatch::open([w.pid]))
            .transpose()?;
        let prepared_guard = guard.clone();
        let requested_path = path.clone();
        let job =
            crate::agent_export::ExportJob::start_annotation_prepared(path.clone(), move || {
                let prepared = snapshot.prepare(requested_path)?;
                Ok(crate::agent_export::Prepared {
                    width: prepared.width,
                    height: prepared.height,
                    rgba: prepared.rgba,
                    metadata: PreparedExport {
                        info: prepared.metadata,
                        guard: prepared_guard,
                    },
                })
            })?;
        self.annotation_export = Some(Pending {
            tag,
            guard,
            policy_generation: self.generation,
            process,
            path,
            started: Instant::now(),
            replied: false,
            decided: false,
            approved_at: None,
            progress_sent: false,
            job,
        });
        self.annotation_export_progress(router, "preparing");
        Ok(())
    }

    fn annotation_export_progress(
        &self,
        router: &mut crate::magnifier_runtime::Runtime,
        phase: &str,
    ) {
        router.set_annotation_export_pending(self.annotation_export.as_ref().map(|job| {
            ExportProgress {
                user: job.guard.user,
                kind: job.guard.kind,
                ink_scope: job.guard.ink_scope,
                phase: phase.into(),
            }
        }));
    }

    fn annotation_export_authorized(
        &self,
        pending: &Pending,
        users: &virtual_users::Runtime,
        router: &mut crate::magnifier_runtime::Runtime,
    ) -> Result<()> {
        let guard = &pending.guard;
        ensure!(
            users.require_active(guard.user)?.seat.identity() == guard.user_id,
            "Annotation export user changed or was removed"
        );
        if guard.ink_scope == InkScope::AllUsers {
            ensure!(
                ink_users(users) == guard.ink_users,
                "Annotation export user roster changed"
            );
        }
        if pending.tag.0 != 0 {
            self.require(pending.tag.0)?;
            ensure!(
                self.generation == pending.policy_generation,
                "Agent Access policy changed during annotation export"
            );
            self.authorize_annotation_export_scope(guard.user, guard.kind, guard.ink_scope, users)?;
        }
        if let Some(window) = guard.window {
            ensure!(
                pending
                    .process
                    .as_ref()
                    .is_some_and(|watch| watch.exited().is_none()),
                "Original annotation capture process exited"
            );
            ensure!(
                router.annotation_selected_window(guard.user)? == Some(window),
                "Selected annotation target changed"
            );
            if pending.tag.0 != 0 {
                ensure!(
                    self.target(guard.user, router)? == window,
                    "Annotation target permission changed"
                );
            }
            ensure!(
                mux_windows::targets::client_bounds(window.hwnd, window.pid)? == guard.bounds,
                "Annotation target bounds changed"
            );
        } else if matches!(
            guard.kind,
            ExportKind::FullDesktop | ExportKind::DesktopRegion
        ) {
            let desktop = guard
                .desktop_bounds
                .context("Desktop screenshot extent is missing")?;
            ensure!(
                guard.bounds.valid_region()
                    && desktop.intersection(guard.bounds) == Some(guard.bounds)
                    && (guard.kind != ExportKind::FullDesktop || guard.bounds == desktop),
                "Desktop screenshot region changed"
            );
            // A later successful exclusion cannot sanitize pixels captured while a
            // newly created/replaced overlay was visible. Reject its lifetime first.
            router.annotation_validate_overlay_binding(&guard.desktop_overlays)?;
            router.annotation_validate_desktop_export(desktop)?;
        } else {
            ensure!(
                mux_windows::targets::desktop_bounds() == guard.bounds,
                "Annotation canvas bounds changed"
            );
        }
        Ok(())
    }

    fn annotation_ready_matches(pending: &Pending, ready: &PreparedExport) -> Result<()> {
        let info = &ready.info;
        let guard = &pending.guard;
        ensure!(
            ready.guard == *guard
                && info.user == guard.user
                && info.kind == guard.kind
                && info.ink_scope == guard.ink_scope
                && info.revision == guard.revision
                && info.bounds == Some(guard.bounds)
                && info.path == pending.path
                && i64::from(info.width)
                    == i64::from(guard.bounds.right) - i64::from(guard.bounds.left)
                && i64::from(info.height)
                    == i64::from(guard.bounds.bottom) - i64::from(guard.bounds.top)
                && info.pid == guard.window.map(|w| w.pid)
                && info.hwnd == guard.window.map(|w| w.hwnd),
            "Annotation export metadata changed during preparation"
        );
        Ok(())
    }

    pub fn poll_annotation_export(
        &mut self,
        users: &virtual_users::Runtime,
        automation: &macro_studio::Runtime,
        router: &mut crate::magnifier_runtime::Runtime,
        base: &Profile,
    ) -> Result<Option<Event>> {
        self.poll_annotation_export_at(Instant::now(), users, automation, router, base)
    }

    fn poll_annotation_export_at(
        &mut self,
        now: Instant,
        users: &virtual_users::Runtime,
        automation: &macro_studio::Runtime,
        router: &mut crate::magnifier_runtime::Runtime,
        base: &Profile,
    ) -> Result<Option<Event>> {
        let Some(mut pending) = self.annotation_export.take() else {
            return Ok(None);
        };
        let mut error = None;
        if !pending.decided {
            // Revocation wins even if a timely ready result is already queued.
            let authority = self.annotation_export_authorized(&pending, users, router);
            if let Err(reason) = authority {
                let _ = pending.job.approve(false);
                pending.decided = true;
                error = Some(format!(
                    "Annotation export cancelled before authorization: {reason:#}"
                ));
            } else if let Some(ready) = pending.job.poll_ready_at() {
                let authorization = ready.and_then(|(prepared_at, ready)| {
                    ensure!(
                        prepared_at.saturating_duration_since(pending.started)
                            <= PREPARATION_BUDGET,
                        "Annotation export preparation exceeded its five-second budget"
                    );
                    Self::annotation_ready_matches(&pending, &ready)?;
                    self.annotation_export_authorized(&pending, users, router)?;
                    ensure!(
                        now.max(Instant::now())
                            .saturating_duration_since(pending.started)
                            < REQUEST_BUDGET,
                        "Annotation export request timed out before authorization"
                    );
                    Ok(())
                });
                match authorization {
                    Ok(()) => match pending.job.approve(true) {
                        Ok(()) => pending.approved_at = Some(now.max(Instant::now())),
                        Err(e) => error = Some(format!("Annotation export approval failed: {e:#}")),
                    },
                    Err(e) => {
                        let _ = pending.job.approve(false);
                        error = Some(format!(
                            "Annotation export preparation/authorization failed: {e:#}"
                        ));
                    }
                }
                pending.decided = true;
                pending.progress_sent = false;
            } else if now
                .max(Instant::now())
                .saturating_duration_since(pending.started)
                >= PREPARATION_BUDGET
            {
                let _ = pending.job.approve(false);
                pending.decided = true;
                error = Some("Annotation export cancelled before authorization: Annotation export timed out before authorization".into());
            }
        }

        if let Some(result) = pending.job.poll_done() {
            router.set_annotation_export_pending(None);
            if pending.replied {
                // A late completion is telemetry, never another command's ACK.
                return Ok(Some(Event::Annotation(router.annotation_state())));
            }
            let result = result.and_then(|ready| {
                Self::annotation_ready_matches(&pending, &ready)?;
                self.annotation_export_authorized(&pending, users, router).map_err(|e| anyhow::anyhow!("Annotation export completed after authorization changed: {e:#}; an already committed file cannot be rolled back"))?;
                Ok(ready.info)
            });
            let (export, error) = match result {
                Ok(info) => (Some(info), None),
                Err(e) => (None, Some(error.unwrap_or_else(|| format!("{e:#}")))),
            };
            return self
                .annotation_export_event(
                    pending.tag,
                    (export, error),
                    users,
                    automation,
                    router,
                    base,
                )
                .map(Some);
        }
        if error.is_none()
            && pending.approved_at.is_some_and(|approved| {
                let now = now.max(Instant::now());
                now.saturating_duration_since(approved) >= PUBLICATION_BUDGET
                    || now.saturating_duration_since(pending.started) >= REQUEST_BUDGET
            })
            && !pending.replied
        {
            error = Some("Annotation export exceeded the completion timeout after authorization; the file may still be published. Inspect the destination before another export.".into());
        }
        let reply = if error.is_some() && !pending.replied {
            pending.replied = true;
            Some((pending.tag, error))
        } else {
            None
        };
        let progress = !pending.progress_sent;
        pending.progress_sent = true;
        let phase = if pending.approved_at.is_some() {
            "publishing"
        } else if pending.decided {
            "cancelling"
        } else {
            "preparing"
        };
        self.annotation_export = Some(pending);
        self.annotation_export_progress(router, phase);
        if let Some((tag, error)) = reply {
            return self
                .annotation_export_event(tag, (None, error), users, automation, router, base)
                .map(Some);
        }
        Ok(progress.then(|| Event::Annotation(router.annotation_state())))
    }

    pub fn cancel_annotation_export(&mut self) {
        // Whole-session shutdown admits no subsequent jobs. Dropping an unapproved
        // permit denies publication even if the provider finishes during cleanup.
        self.annotation_export = None;
    }

    fn annotation_export_event(
        &self,
        tag: (u64, u64),
        outcome: (Option<ExportInfo>, Option<String>),
        users: &virtual_users::Runtime,
        automation: &macro_studio::Runtime,
        router: &mut crate::magnifier_runtime::Runtime,
        base: &Profile,
    ) -> Result<Event> {
        let (export, error) = outcome;
        let mut annotation = router.annotation_state();
        annotation.acknowledged = true;
        annotation.export = export;
        annotation.error.clone_from(&error);
        if tag.0 == 0 {
            return Ok(Event::Annotation(annotation));
        }
        let mut state = self.snapshot(tag.0, tag.1, users, automation, router, base)?;
        state.events.push(Event::Annotation(annotation));
        state.error = error;
        Ok(Event::Agent(state))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mux_core::{desktop::CursorSettings, emulated::Packet};

    struct QuietEngine;
    impl Engine for QuietEngine {
        fn input(&mut self, _: Packet) -> Result<()> {
            Ok(())
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
            Ok(())
        }
        fn pause(&mut self, _: bool) -> Result<()> {
            Ok(())
        }
        fn settings(&mut self, _: usize, _: CursorSettings) -> Result<()> {
            Ok(())
        }
        fn tick(&mut self) -> Result<(bool, Vec<crate::router::State>)> {
            Ok((false, vec![]))
        }
    }

    struct Fixture {
        profile: Profile,
        users: virtual_users::Runtime,
        automation: macro_studio::Runtime,
        router: crate::magnifier_runtime::Runtime,
    }
    fn fixture(ids: &[&str]) -> Fixture {
        let seats: Vec<_> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| {
                serde_json::json!({
                    "id":id,"name":id,"mouse":format!("virtual://mouse/{}",i+1),
                    "keyboard":format!("virtual://keyboard/{}",i+1),"target_pid":i+10
                })
            })
            .collect();
        let profile: Profile = serde_json::from_value(
            serde_json::json!({"version":1,"concurrent":true,"seats":seats}),
        )
        .unwrap();
        let resolved = profile.resolve(&[]).unwrap();
        let annotation = crate::annotation_runtime::Runtime::new(
            Box::new(QuietEngine),
            &profile,
            resolved.clone(),
        )
        .unwrap();
        let router =
            crate::magnifier_runtime::Runtime::new(annotation, &profile, resolved.clone()).unwrap();
        let users = virtual_users::Runtime::new(&profile, resolved.clone(), 99).unwrap();
        let automation = macro_studio::Runtime::new_with_ids(
            resolved,
            ids.iter().map(|id| id.to_string()).collect(),
        );
        Fixture {
            profile,
            users,
            automation,
            router,
        }
    }

    #[test]
    fn export_uses_capture_grant_and_full_ink_scope_without_session_control() {
        let f = fixture(&["a", "b"]);
        let mut runtime = Runtime::new();
        runtime.policy.user_ids = vec!["a".into()];
        assert!(
            runtime
                .authorize_annotation_export_scope(
                    0,
                    ExportKind::InkOnly,
                    InkScope::SelectedUser,
                    &f.users
                )
                .is_err()
        );
        runtime.policy.capture = true;
        assert!(!runtime.policy.session_control);
        assert!(
            runtime
                .authorize_annotation_export_scope(
                    0,
                    ExportKind::InkOnly,
                    InkScope::SelectedUser,
                    &f.users
                )
                .is_ok()
        );
        assert!(
            runtime
                .authorize_annotation_export_scope(
                    1,
                    ExportKind::InkOnly,
                    InkScope::SelectedUser,
                    &f.users
                )
                .is_err()
        );
        assert!(
            runtime
                .authorize_annotation_export_scope(
                    0,
                    ExportKind::InkOnly,
                    InkScope::AllUsers,
                    &f.users
                )
                .is_err()
        );
        runtime.policy.user_ids.push("b".into());
        assert!(
            runtime
                .authorize_annotation_export_scope(
                    0,
                    ExportKind::InkOnly,
                    InkScope::AllUsers,
                    &f.users
                )
                .is_ok()
        );
    }

    #[test]
    fn desktop_and_target_export_grants_are_independent_and_ink_stays_scoped() {
        let f = fixture(&["a", "b"]);
        let mut runtime = Runtime::new();
        runtime.policy.user_ids = vec!["a".into()];
        runtime.policy.allowed_pids = vec![10, 11];
        runtime.policy.capture = true;
        for kind in [ExportKind::FullDesktop, ExportKind::DesktopRegion] {
            assert!(
                runtime
                    .authorize_annotation_export_scope(0, kind, InkScope::SelectedUser, &f.users)
                    .is_err()
            );
        }
        runtime.policy.capture = false;
        runtime.policy.desktop_capture = true;
        for kind in [ExportKind::FullDesktop, ExportKind::DesktopRegion] {
            assert!(
                runtime
                    .authorize_annotation_export_scope(0, kind, InkScope::SelectedUser, &f.users)
                    .is_ok()
            );
            assert!(
                runtime
                    .authorize_annotation_export_scope(0, kind, InkScope::AllUsers, &f.users)
                    .is_err()
            );
        }
        assert!(
            runtime
                .authorize_annotation_export_scope(
                    0,
                    ExportKind::InkOnly,
                    InkScope::SelectedUser,
                    &f.users
                )
                .is_err()
        );
        runtime.policy.user_ids.push("b".into());
        assert!(
            runtime
                .authorize_annotation_export_scope(
                    0,
                    ExportKind::DesktopRegion,
                    InkScope::AllUsers,
                    &f.users
                )
                .is_ok()
        );
    }

    fn blocked_pending(f: &Fixture, path: String) -> (Pending, mpsc::SyncSender<()>) {
        let guard = Guard {
            user: 0,
            user_id: "a".into(),
            ink_users: ink_users(&f.users),
            kind: ExportKind::InkOnly,
            ink_scope: InkScope::AllUsers,
            revision: 7,
            bounds: mux_windows::targets::desktop_bounds(),
            desktop_bounds: Some(mux_windows::targets::desktop_bounds()),
            desktop_overlays: f.router.annotation_desktop_overlay_binding(),
            window: None,
        };
        let prepared_guard = guard.clone();
        let width = (guard.bounds.right - guard.bounds.left) as u32;
        let height = (guard.bounds.bottom - guard.bounds.top) as u32;
        let output = path.clone();
        let (release, wait) = mpsc::sync_channel(1);
        let job =
            crate::agent_export::ExportJob::start_annotation_prepared(path.clone(), move || {
                wait.recv()?;
                Ok(crate::agent_export::Prepared {
                    width,
                    height,
                    rgba: std::sync::Arc::new(vec![0; width as usize * height as usize * 4]),
                    metadata: PreparedExport {
                        info: ExportInfo {
                            user: 0,
                            ink_scope: InkScope::AllUsers,
                            revision: 7,
                            path: output,
                            width,
                            height,
                            kind: ExportKind::InkOnly,
                            pid: None,
                            hwnd: None,
                            bounds: Some(prepared_guard.bounds),
                        },
                        guard: prepared_guard,
                    },
                })
            })
            .unwrap();
        (
            Pending {
                tag: (0, 0),
                guard,
                policy_generation: 0,
                process: None,
                path,
                started: Instant::now(),
                replied: false,
                decided: false,
                approved_at: None,
                progress_sent: false,
                job,
            },
            release,
        )
    }

    fn ready_pending(
        f: &Fixture,
        started: Instant,
        prepared_at: Instant,
    ) -> (
        Pending,
        mpsc::Receiver<bool>,
        mpsc::SyncSender<Result<PreparedExport>>,
        PreparedExport,
    ) {
        let bounds = mux_windows::targets::desktop_bounds();
        let guard = Guard {
            user: 0,
            user_id: "a".into(),
            ink_users: ink_users(&f.users),
            kind: ExportKind::InkOnly,
            ink_scope: InkScope::AllUsers,
            revision: 7,
            bounds,
            desktop_bounds: Some(bounds),
            desktop_overlays: f.router.annotation_desktop_overlay_binding(),
            window: None,
        };
        let info = ExportInfo {
            user: 0,
            ink_scope: InkScope::AllUsers,
            revision: 7,
            path: "fake-worker.png".into(),
            width: (bounds.right - bounds.left) as u32,
            height: (bounds.bottom - bounds.top) as u32,
            kind: ExportKind::InkOnly,
            pid: None,
            hwnd: None,
            bounds: Some(bounds),
        };
        let ready = PreparedExport {
            info,
            guard: guard.clone(),
        };
        let (job, decision, done) =
            crate::agent_export::ExportJob::ready_fixture(prepared_at, ready.clone());
        (
            Pending {
                tag: (0, 0),
                guard,
                policy_generation: 0,
                process: None,
                path: ready.info.path.clone(),
                started,
                replied: false,
                decided: false,
                approved_at: None,
                progress_sent: false,
                job,
            },
            decision,
            done,
            ready,
        )
    }

    #[test]
    fn timely_ready_observed_after_five_seconds_is_approved_then_acknowledged() {
        let mut f = fixture(&["a", "b"]);
        let mut runtime = Runtime::new();
        let now = Instant::now();
        let started = now - Duration::from_secs(6);
        let (pending, decision, done, ready) =
            ready_pending(&f, started, started + Duration::from_secs(4));
        runtime.annotation_export = Some(pending);
        let event = runtime
            .poll_annotation_export_at(now, &f.users, &f.automation, &mut f.router, &f.profile)
            .unwrap();
        assert!(
            matches!(event, Some(Event::Annotation(state)) if !state.acknowledged && state.error.is_none())
        );
        assert!(decision.try_recv().unwrap());
        assert!(
            runtime
                .annotation_export
                .as_ref()
                .unwrap()
                .approved_at
                .is_some()
        );
        done.send(Ok(ready))
            .unwrap_or_else(|_| panic!("completion receiver disappeared"));
        let event = runtime
            .poll_annotation_export_at(now, &f.users, &f.automation, &mut f.router, &f.profile)
            .unwrap();
        assert!(
            matches!(event, Some(Event::Annotation(state)) if state.acknowledged && state.error.is_none() && state.export.is_some())
        );
        assert!(runtime.annotation_export.is_none());
    }

    #[test]
    fn late_preparation_and_expired_request_are_denied_and_retained_until_done() {
        for (prepared_seconds, observed_seconds, reason) in
            [(6, 7, "five-second budget"), (4, 9, "request timed out")]
        {
            let mut f = fixture(&["a", "b"]);
            let mut runtime = Runtime::new();
            let started = Instant::now();
            let (pending, decision, done, _) =
                ready_pending(&f, started, started + Duration::from_secs(prepared_seconds));
            runtime.annotation_export = Some(pending);
            let now = started + Duration::from_secs(observed_seconds);
            let event = runtime
                .poll_annotation_export_at(now, &f.users, &f.automation, &mut f.router, &f.profile)
                .unwrap();
            assert!(
                matches!(event, Some(Event::Annotation(state)) if state.acknowledged && state.export.is_none() && state.error.as_ref().is_some_and(|error| error.contains(reason)))
            );
            assert!(!decision.try_recv().unwrap());
            assert!(runtime.annotation_export.is_some());
            done.send(Err(anyhow::anyhow!("fake worker denied publication")))
                .unwrap_or_else(|_| panic!("completion receiver disappeared"));
            let event = runtime
                .poll_annotation_export_at(now, &f.users, &f.automation, &mut f.router, &f.profile)
                .unwrap();
            assert!(matches!(event, Some(Event::Annotation(state)) if !state.acknowledged));
            assert!(runtime.annotation_export.is_none());
        }
    }

    #[test]
    fn desktop_overlay_lifetime_change_denies_queued_preparation() {
        for change in ["unchanged", "reconfigured", "different_windows"] {
            let mut f = fixture(&["a", "b"]);
            let mut runtime = Runtime::new();
            let now = Instant::now();
            let (mut pending, _, _, mut ready) = ready_pending(&f, now, now);
            pending.guard.kind = ExportKind::FullDesktop;
            if change == "different_windows" {
                // Original window disappeared. No HWND is dereferenced by this test.
                pending.guard.desktop_overlays.hwnds.push(1);
            }
            ready.guard = pending.guard.clone();
            ready.info.kind = ExportKind::FullDesktop;
            let (job, decision, done) = crate::agent_export::ExportJob::ready_fixture(now, ready);
            pending.job = job;
            runtime.annotation_export = Some(pending);
            if change == "reconfigured" {
                // Even an unchanged HWND set cannot conceal a magnifier lifetime
                // transition or HWND reuse. Off creates no native window here.
                f.router
                    .magnifier_command(crate::magnifier_runtime::Command::Configure {
                        user: 0,
                        settings: mux_core::magnifier::Settings::default(),
                    })
                    .unwrap();
            }
            let event = runtime
                .poll_annotation_export_at(now, &f.users, &f.automation, &mut f.router, &f.profile)
                .unwrap();
            let approved = decision.try_recv().unwrap();
            assert_eq!(approved, change == "unchanged");
            if !approved {
                assert!(matches!(event, Some(Event::Annotation(state))
                    if state.error.as_ref().is_some_and(|error| error.contains("Owned overlay lifetime changed"))));
            }
            assert!(runtime.annotation_export.is_some());
            done.send(Err(anyhow::anyhow!("fixture publication finished")))
                .unwrap_or_else(|_| panic!("completion receiver disappeared"));
            runtime
                .poll_annotation_export_at(now, &f.users, &f.automation, &mut f.router, &f.profile)
                .unwrap();
            assert!(runtime.annotation_export.is_none());
        }
    }

    #[test]
    fn desktop_grant_or_generation_revocation_denies_queued_preparation() {
        for replace_policy in [false, true] {
            let mut f = fixture(&["a", "b"]);
            let mut runtime = Runtime::new();
            runtime.policy.enabled = true;
            runtime.policy.user_ids = vec!["a".into(), "b".into()];
            runtime.policy.desktop_capture = true;
            runtime.clients.insert(1);
            let now = Instant::now();
            let (mut pending, _, _, mut ready) = ready_pending(&f, now, now);
            pending.tag = (1, 42);
            pending.guard.kind = ExportKind::DesktopRegion;
            ready.guard = pending.guard.clone();
            ready.info.kind = ExportKind::DesktopRegion;
            let (job, decision, done) = crate::agent_export::ExportJob::ready_fixture(now, ready);
            pending.job = job;
            runtime.annotation_export = Some(pending);
            if replace_policy {
                runtime.generation = 1;
            } else {
                runtime.policy.desktop_capture = false;
            }
            let event = runtime
                .poll_annotation_export_at(now, &f.users, &f.automation, &mut f.router, &f.profile)
                .unwrap();
            assert!(matches!(event, Some(Event::Agent(state)) if state.error.is_some()));
            assert!(!decision.try_recv().unwrap());
            assert!(runtime.annotation_export.is_some());
            done.send(Err(anyhow::anyhow!("publication denied")))
                .unwrap_or_else(|_| panic!("completion receiver disappeared"));
            runtime
                .poll_annotation_export_at(now, &f.users, &f.automation, &mut f.router, &f.profile)
                .unwrap();
            assert!(runtime.annotation_export.is_none());
        }
    }

    #[test]
    fn revoked_identity_wins_over_queued_timely_preparation() {
        let mut f = fixture(&["a", "b"]);
        let renamed = fixture(&["changed", "b"]);
        let mut runtime = Runtime::new();
        let started = Instant::now();
        let (pending, decision, done, _) = ready_pending(&f, started, started);
        runtime.annotation_export = Some(pending);
        let event = runtime
            .poll_annotation_export_at(
                started + Duration::from_secs(6),
                &renamed.users,
                &f.automation,
                &mut f.router,
                &f.profile,
            )
            .unwrap();
        assert!(
            matches!(event, Some(Event::Annotation(state)) if state.acknowledged && state.export.is_none() && state.error.as_ref().is_some_and(|error| error.contains("user changed")))
        );
        assert!(!decision.try_recv().unwrap());
        assert!(runtime.annotation_export.is_some());
        done.send(Err(anyhow::anyhow!("fake worker denied publication")))
            .unwrap_or_else(|_| panic!("completion receiver disappeared"));
        let event = runtime
            .poll_annotation_export_at(
                started + Duration::from_secs(6),
                &renamed.users,
                &f.automation,
                &mut f.router,
                &f.profile,
            )
            .unwrap();
        assert!(matches!(event, Some(Event::Annotation(state)) if !state.acknowledged));
        assert!(runtime.annotation_export.is_none());
    }

    #[test]
    fn approved_publication_timeout_keeps_capacity_and_late_completion_is_telemetry() {
        for (request_age, publication_age) in [(0, 4), (6, 3)] {
            let mut f = fixture(&["a", "b"]);
            let mut runtime = Runtime::new();
            let now = Instant::now();
            let started = now - Duration::from_secs(request_age);
            let (pending, decision, done, ready) = ready_pending(&f, started, started);
            runtime.annotation_export = Some(pending);
            runtime
                .poll_annotation_export_at(now, &f.users, &f.automation, &mut f.router, &f.profile)
                .unwrap();
            assert!(decision.try_recv().unwrap());
            let approved_at = runtime
                .annotation_export
                .as_ref()
                .unwrap()
                .approved_at
                .unwrap();
            let later = if request_age == 0 {
                approved_at + Duration::from_secs(publication_age)
            } else {
                started + REQUEST_BUDGET
            };
            let event = runtime
                .poll_annotation_export_at(
                    later,
                    &f.users,
                    &f.automation,
                    &mut f.router,
                    &f.profile,
                )
                .unwrap();
            assert!(
                matches!(event, Some(Event::Annotation(state)) if state.acknowledged && state.error.as_ref().is_some_and(|error| error.contains("file may still be published")))
            );
            assert!(runtime.annotation_export.is_some());
            done.send(Ok(ready))
                .unwrap_or_else(|_| panic!("completion receiver disappeared"));
            let event = runtime
                .poll_annotation_export_at(
                    later,
                    &f.users,
                    &f.automation,
                    &mut f.router,
                    &f.profile,
                )
                .unwrap();
            assert!(matches!(event, Some(Event::Annotation(state)) if !state.acknowledged));
            assert!(runtime.annotation_export.is_none());
        }
    }

    #[test]
    fn original_user_and_all_user_roster_changes_invalidate_precommit_authority() {
        let mut f = fixture(&["a", "b"]);
        let mut runtime = Runtime::new();
        let path = std::env::temp_dir()
            .join(format!(
                "mux-annotation-identity-{}.png",
                std::process::id()
            ))
            .display()
            .to_string();
        let (pending, release) = blocked_pending(&f, path);
        let renamed = fixture(&["changed", "b"]);
        let added = fixture(&["a", "b", "new"]);
        assert!(
            runtime
                .annotation_export_authorized(&pending, &renamed.users, &mut f.router)
                .unwrap_err()
                .to_string()
                .contains("user changed")
        );
        assert!(
            runtime
                .annotation_export_authorized(&pending, &added.users, &mut f.router)
                .unwrap_err()
                .to_string()
                .contains("roster changed")
        );
        runtime.annotation_export = Some(pending);
        runtime.cancel_annotation_export();
        release.send(()).unwrap();
    }

    #[test]
    fn timeout_retains_capacity_then_finishes_as_unacknowledged_telemetry_without_file() {
        let mut f = fixture(&["a", "b"]);
        let mut runtime = Runtime::new();
        let path = std::env::temp_dir().join(format!(
            "mux-annotation-timeout-{}-{}.png",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let (mut pending, release) = blocked_pending(&f, path.display().to_string());
        pending.started = Instant::now() - Duration::from_secs(6);
        runtime.annotation_export = Some(pending);
        let event = runtime
            .poll_annotation_export(&f.users, &f.automation, &mut f.router, &f.profile)
            .unwrap()
            .unwrap();
        let Event::Annotation(state) = event else {
            panic!("Expected local timeout ACK")
        };
        assert!(
            state.acknowledged
                && state
                    .error
                    .unwrap()
                    .contains("timed out before authorization")
        );
        assert!(state.export_pending.is_some());
        let request = AnnotationCommand::Export {
            user: 0,
            path: path.display().to_string(),
            kind: ExportKind::InkOnly,
            ink_scope: InkScope::SelectedUser,
            desktop_region: None,
        };
        assert!(
            runtime
                .begin_annotation_export((0, 0), request, &f.users, &mut f.router)
                .unwrap_err()
                .to_string()
                .contains("still in progress")
        );
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while runtime.annotation_export.is_some() {
            if let Some(Event::Annotation(state)) = runtime
                .poll_annotation_export(&f.users, &f.automation, &mut f.router, &f.profile)
                .unwrap()
            {
                assert!(
                    !state.acknowledged,
                    "Late completion must not consume a later command"
                );
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(!path.exists());
        assert!(f.router.annotation_state().export_pending.is_none());
    }
}
