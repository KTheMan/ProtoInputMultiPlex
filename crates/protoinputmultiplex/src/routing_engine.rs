use crate::{
    desktop_router::DesktopRouter,
    router::{Router, State},
};
use anyhow::Result;
use mux_core::{desktop::CursorSettings, emulated::Packet, routing::Mode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserPauseState {
    pub user: usize,
    pub supported: bool,
    pub paused: bool,
    pub pending_releases: usize,
    pub error: Option<String>,
}
impl UserPauseState {
    pub(crate) fn unsupported(user: usize, reason: &str) -> Self {
        Self {
            user,
            error: Some(reason.into()),
            ..Self::default()
        }
    }
    pub(crate) fn resume(&mut self, global_paused: bool, pending: usize) -> Result<()> {
        anyhow::ensure!(self.supported, "Per-user Pause is unavailable");
        anyhow::ensure!(!global_paused, "Global Pause must be resumed first");
        anyhow::ensure!(
            pending == 0 && self.error.is_none(),
            "Finish this user's Pause cleanup before Resume"
        );
        self.paused = false;
        Ok(())
    }
}
pub(crate) fn virtual_pause_devices(seat: &mux_core::Seat) -> Result<bool> {
    let mouse = mux_core::emulated::parse(&seat.mouse)?;
    let keyboard = seat
        .keyboard
        .as_deref()
        .map(mux_core::emulated::parse)
        .transpose()?;
    Ok(
        mouse.is_some_and(|device| device.kind == mux_core::DeviceKind::Mouse)
            && keyboard.is_some_and(|device| {
                device.is_some_and(|device| device.kind == mux_core::DeviceKind::Keyboard)
            }),
    )
}
pub(crate) fn automatic_pause_supported(mode: Mode, multi_keyboard: bool) -> bool {
    mode == Mode::Concurrent && multi_keyboard
}
pub(crate) fn validate_user_pause_mode_change(states: &[UserPauseState]) -> Result<()> {
    anyhow::ensure!(
        !states
            .iter()
            .any(|state| state.paused || state.pending_releases != 0),
        "Resume all individually paused users and finish their cleanup before changing routing mode"
    );
    Ok(())
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AgentStudioOwner {
    pub client: u64,
    pub user: usize,
    pub hwnd: usize,
    pub pid: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProgramOrigin {
    Manual(usize),
    Agent(u64, usize),
    AgentStudio(AgentStudioOwner),
    Composite,
    Studio(usize),
}
impl ProgramOrigin {
    pub fn agent(self) -> Option<(u64, usize)> {
        match self {
            Self::Agent(client, user) => Some((client, user)),
            Self::AgentStudio(owner) => Some((owner.client, owner.user)),
            _ => None,
        }
    }
}
pub trait Engine {
    fn user_pause_state(&self, user: usize) -> UserPauseState {
        UserPauseState::unsupported(
            user,
            "Per-user Pause is unavailable for this routing provider",
        )
    }
    fn pause_user(&mut self, _user: usize, _paused: bool) -> Result<()> {
        anyhow::bail!("Per-user Pause is unavailable for this routing provider")
    }
    /// Propagate mapper delivery faults rather than reducing them to status.
    /// Scoped by the mapper adapter; ordinary routing keeps its existing policy.
    fn mapper_delivery(&mut self, _active: bool) -> Result<()> {
        Ok(())
    }
    /// Retain a logical Text recipient before attachment or first-unit effects.
    fn begin_text_delivery(&mut self, _user: usize) -> Result<()> {
        Ok(())
    }
    /// Pure scope cleanup; no native release, focus change or input emission.
    fn end_text_delivery(&mut self, _user: usize) {}
    fn select_window(&mut self, _: &crate::window_selection::Selection) -> Result<()> {
        anyhow::bail!("Window selection requires automatic virtual desktop routing")
    }
    fn validate_mode_change(&self, _: bool) -> Result<()> {
        Ok(())
    }
    /// A lazy attachment is a separate target capability from the final input recipient.
    fn pending_attachment_window(&self, _: usize) -> Result<Option<mux_core::routing::Window>> {
        Ok(None)
    }

    fn add_virtual_user(&mut self, _: &mux_core::Seat, _: mux_core::ResolvedSeat) -> Result<usize> {
        anyhow::bail!("Live virtual-user changes are not supported by this router")
    }
    /// Release native ownership while leaving the slot available for retry.
    fn prepare_remove_virtual_user(&mut self, _: usize) -> Result<()> {
        anyhow::bail!("Live virtual-user changes are not supported by this router")
    }
    /// Called once, in the same worker command, after all fallible cleanup.
    fn commit_remove_virtual_user(&mut self, _: usize) {
        unreachable!("virtual user removal was not prepared")
    }
    fn program_origin(&self) -> ProgramOrigin {
        ProgramOrigin::Composite
    }
    fn forwarding_supported(&self) -> bool {
        false
    }
    fn input_replica(&mut self, _: ProgramOrigin, _: usize, _: &[usize], _: Packet) -> Result<()> {
        anyhow::bail!("Replication requires supervised input ownership")
    }
    /// Deliver one source packet through the forwarding graph. `raw` retains the
    /// original device identity; `output` names the terminal user's device.
    fn input_forwarded(
        &mut self,
        _: ProgramOrigin,
        _: usize,
        _: &[usize],
        _: Packet,
        _: Packet,
    ) -> Result<bool> {
        anyhow::bail!("Forwarding requires supervised input ownership")
    }
    fn forwarded_motion(&mut self, packet: Packet, source_buttons: u8) -> Result<bool> {
        self.input_with_source_buttons(packet, source_buttons)?;
        Ok(true)
    }
    fn release_forwarding(&mut self, _: Option<ProgramOrigin>) -> Result<()> {
        anyhow::bail!("Forwarding requires supervised input ownership")
    }
    fn set_forwarding_sources(&mut self, _: &[bool]) -> Result<()> {
        anyhow::bail!("Forwarding requires automatic desktop routing")
    }
    fn move_replica(
        &mut self,
        _: ProgramOrigin,
        _: usize,
        _: &[usize],
        _: usize,
        _: i32,
        _: i32,
    ) -> Result<()> {
        anyhow::bail!("Replication requires supervised input ownership")
    }
    fn release_replication(&mut self, _: Option<ProgramOrigin>) -> Result<()> {
        anyhow::bail!("Replication requires supervised input ownership")
    }
    fn release_program(&mut self, _: ProgramOrigin) -> Result<()> {
        anyhow::bail!("Automation requires supervised input ownership")
    }
    fn pointer(&self, _: usize) -> Result<(i32, i32)> {
        anyhow::bail!("Annotation requires automatic desktop or concurrent assigned routing")
    }
    fn selected_window(&self, _: usize) -> Result<Option<mux_core::routing::Window>> {
        anyhow::bail!("Selected target capture requires desktop cursor routing")
    }
    fn mouse_window(&self, user: usize) -> Result<Option<mux_core::routing::Window>> {
        self.selected_window(user)
    }
    /// Whether raw keyboard actions are accepted for this source user.
    fn keyboard_enabled(&self, _user: usize) -> bool {
        true
    }
    fn keyboard_window(&self, user: usize) -> Result<Option<mux_core::routing::Window>> {
        self.selected_window(user)
    }
    fn movement_window(
        &self,
        user: usize,
        _: i32,
        _: i32,
    ) -> Result<Option<mux_core::routing::Window>> {
        self.selected_window(user)
    }
    fn packet_window(
        &self,
        user: usize,
        packet: Packet,
    ) -> Result<Option<mux_core::routing::Window>> {
        if packet.kind == 0 && packet.flags == 0 {
            let (x, y) = self.pointer(user)?;
            self.movement_window(user, x.saturating_add(packet.x), y.saturating_add(packet.y))
        } else if packet.kind == 0 {
            self.mouse_window(user)
        } else {
            self.keyboard_window(user)
        }
    }
    fn input(&mut self, packet: Packet) -> Result<()>;
    /// Records authorized raw mouse activity before output filtering, for this user's overlay.
    fn note_mouse_activity(&mut self, _user: usize) -> Result<()> {
        Ok(())
    }
    /// Delivers an input packet with the source user's pre-translation held-button state.
    /// Routers that do not support conditional wheel conversion can ignore the mask.
    fn input_with_source_buttons(&mut self, packet: Packet, _source_buttons: u8) -> Result<()> {
        self.input(packet)
    }
    /// Reports an intentional legacy cursor-map suppression for this valid mouse button.
    /// This lets supervised ownership retain the raw source hold for conditional transforms.
    fn button_output_disabled(&self, _packet: Packet) -> bool {
        false
    }
    /// Whether this translated key-down would invoke the shared ink clear shortcut.
    fn annotation_clears_all(&self, _user: usize, _packet: Packet) -> bool {
        false
    }
    fn key_held(&self, device: u32, key: u32) -> bool;
    fn button_held(&self, device: u32, button: u32) -> bool;
    fn paused(&self) -> bool;
    fn release_all(&mut self) -> Result<()>;
    /// Release input and acknowledge target queues before removing native filters.
    fn shutdown(&mut self) -> Result<()> {
        self.release_all()
    }
    fn pause(&mut self, value: bool) -> Result<()>;
    fn settings(&mut self, user: usize, settings: CursorSettings) -> Result<()>;
    /// Publish motion whose deadline elapsed without requiring another input packet.
    fn poll_motion(&mut self) -> Result<()> {
        Ok(())
    }
    fn motion_deadline(&self) -> Option<std::time::Instant> {
        None
    }
    fn tick(&mut self) -> Result<(bool, Vec<State>)>;
    fn mode(&mut self, _: Mode, _: bool) -> Result<()> {
        anyhow::bail!("Start an automatic desktop session to change modes")
    }
    fn move_to(&mut self, _: usize, _: i32, _: i32) -> Result<()> {
        anyhow::bail!("Desktop coordinates require automatic targeting")
    }
}

/// Keep ordinary per-packet filters/permissions in use, while clearing the
/// recipient scope on success, delivery error and unwinding alike.
pub fn with_text_recipient<T>(
    engine: &mut dyn Engine,
    user: usize,
    operation: impl FnOnce(&mut dyn Engine) -> Result<T>,
) -> Result<T> {
    engine.begin_text_delivery(user)?;
    struct Scope<'a> {
        engine: &'a mut dyn Engine,
        user: usize,
    }
    impl Drop for Scope<'_> {
        fn drop(&mut self) {
            self.engine.end_text_delivery(self.user);
        }
    }
    let scope = Scope { engine, user };
    operation(&mut *scope.engine)
}
impl Engine for Router {
    fn user_pause_state(&self, user: usize) -> UserPauseState {
        self.user_pause_state(user)
    }
    fn pause_user(&mut self, user: usize, paused: bool) -> Result<()> {
        self.pause_user(user, paused)
    }
    fn validate_mode_change(&self, multi_keyboard: bool) -> Result<()> {
        self.validate_mode_change(multi_keyboard)
    }
    fn begin_text_delivery(&mut self, user: usize) -> Result<()> {
        self.begin_text_delivery(user)
    }
    fn end_text_delivery(&mut self, user: usize) {
        self.end_text_delivery(user);
    }
    fn keyboard_enabled(&self, user: usize) -> bool {
        self.keyboard_enabled(user)
    }
    fn poll_motion(&mut self) -> Result<()> {
        self.poll_motion()
    }
    fn motion_deadline(&self) -> Option<std::time::Instant> {
        self.motion_deadline()
    }
    fn note_mouse_activity(&mut self, user: usize) -> Result<()> {
        self.note_mouse_activity(user)
    }
    fn shutdown(&mut self) -> Result<()> {
        self.shutdown()
    }
    fn pointer(&self, user: usize) -> Result<(i32, i32)> {
        self.pointer(user)
    }
    fn selected_window(&self, user: usize) -> Result<Option<mux_core::routing::Window>> {
        self.selected_window(user)
    }
    fn mouse_window(&self, user: usize) -> Result<Option<mux_core::routing::Window>> {
        self.mouse_window(user)
    }
    fn keyboard_window(&self, user: usize) -> Result<Option<mux_core::routing::Window>> {
        self.keyboard_window(user)
    }
    fn movement_window(
        &self,
        user: usize,
        x: i32,
        y: i32,
    ) -> Result<Option<mux_core::routing::Window>> {
        self.movement_window(user, x, y)
    }
    fn packet_window(
        &self,
        user: usize,
        packet: Packet,
    ) -> Result<Option<mux_core::routing::Window>> {
        self.packet_window(user, packet)
    }
    fn input(&mut self, p: Packet) -> Result<()> {
        self.input(p)
    }
    fn input_with_source_buttons(&mut self, p: Packet, source_buttons: u8) -> Result<()> {
        self.input_with_source_buttons(p, source_buttons)
    }
    fn button_output_disabled(&self, packet: Packet) -> bool {
        self.button_output_disabled(packet)
    }
    fn key_held(&self, d: u32, k: u32) -> bool {
        self.key_held(d, k)
    }
    fn button_held(&self, d: u32, b: u32) -> bool {
        self.button_held(d, b)
    }
    fn paused(&self) -> bool {
        self.paused
    }
    fn release_all(&mut self) -> Result<()> {
        self.release_all()
    }
    fn pause(&mut self, p: bool) -> Result<()> {
        self.pause(p)
    }
    fn settings(&mut self, u: usize, s: CursorSettings) -> Result<()> {
        self.settings(u, s)
    }
    fn tick(&mut self) -> Result<(bool, Vec<State>)> {
        self.tick()
    }
    fn move_to(&mut self, user: usize, x: i32, y: i32) -> Result<()> {
        self.move_to(user, x, y)
    }
}
impl Engine for DesktopRouter {
    fn user_pause_state(&self, user: usize) -> UserPauseState {
        self.user_pause_state(user)
    }
    fn pause_user(&mut self, user: usize, paused: bool) -> Result<()> {
        self.pause_user(user, paused)
    }
    fn mapper_delivery(&mut self, active: bool) -> Result<()> {
        self.mapper_delivery(active)
    }
    fn forwarding_supported(&self) -> bool {
        true
    }
    fn set_forwarding_sources(&mut self, sources: &[bool]) -> Result<()> {
        self.set_forwarding_sources(sources)
    }
    fn forwarded_motion(&mut self, packet: Packet, source_buttons: u8) -> Result<bool> {
        self.forwarded_motion(packet, source_buttons)
    }
    fn begin_text_delivery(&mut self, user: usize) -> Result<()> {
        self.begin_text_delivery(user)
    }
    fn end_text_delivery(&mut self, user: usize) {
        self.end_text_delivery(user);
    }
    fn pending_attachment_window(&self, user: usize) -> Result<Option<mux_core::routing::Window>> {
        self.pending_attachment_window(user)
    }
    fn select_window(&mut self, selection: &crate::window_selection::Selection) -> Result<()> {
        self.select_window(selection)
    }
    fn validate_mode_change(&self, multi_keyboard: bool) -> Result<()> {
        self.validate_mode_change(multi_keyboard)
    }

    fn keyboard_enabled(&self, user: usize) -> bool {
        self.keyboard_enabled(user)
    }
    fn poll_motion(&mut self) -> Result<()> {
        self.poll_motion()
    }
    fn motion_deadline(&self) -> Option<std::time::Instant> {
        self.motion_deadline()
    }
    fn note_mouse_activity(&mut self, user: usize) -> Result<()> {
        self.note_mouse_activity(user)
    }
    fn shutdown(&mut self) -> Result<()> {
        self.shutdown()
    }
    fn add_virtual_user(
        &mut self,
        seat: &mux_core::Seat,
        resolved: mux_core::ResolvedSeat,
    ) -> Result<usize> {
        self.add_virtual_user(seat, resolved)
    }
    fn prepare_remove_virtual_user(&mut self, user: usize) -> Result<()> {
        self.prepare_remove_virtual_user(user)
    }
    fn commit_remove_virtual_user(&mut self, user: usize) {
        self.commit_remove_virtual_user(user);
    }
    fn pointer(&self, user: usize) -> Result<(i32, i32)> {
        self.pointer(user)
    }
    fn selected_window(&self, user: usize) -> Result<Option<mux_core::routing::Window>> {
        self.selected_window(user)
    }
    fn mouse_window(&self, user: usize) -> Result<Option<mux_core::routing::Window>> {
        self.mouse_window(user)
    }
    fn keyboard_window(&self, user: usize) -> Result<Option<mux_core::routing::Window>> {
        self.keyboard_window(user)
    }
    fn movement_window(
        &self,
        user: usize,
        x: i32,
        y: i32,
    ) -> Result<Option<mux_core::routing::Window>> {
        self.movement_window(user, x, y)
    }
    fn packet_window(
        &self,
        user: usize,
        packet: Packet,
    ) -> Result<Option<mux_core::routing::Window>> {
        self.packet_window(user, packet)
    }
    fn input(&mut self, p: Packet) -> Result<()> {
        self.input(p)
    }
    fn input_with_source_buttons(&mut self, p: Packet, source_buttons: u8) -> Result<()> {
        self.input_with_source_buttons(p, source_buttons)
    }
    fn button_output_disabled(&self, packet: Packet) -> bool {
        self.button_output_disabled(packet)
    }
    fn key_held(&self, d: u32, k: u32) -> bool {
        self.key_held(d, k)
    }
    fn button_held(&self, d: u32, b: u32) -> bool {
        self.button_held(d, b)
    }
    fn paused(&self) -> bool {
        self.paused()
    }
    fn release_all(&mut self) -> Result<()> {
        self.release_all()
    }
    fn pause(&mut self, p: bool) -> Result<()> {
        self.pause(p)
    }
    fn settings(&mut self, u: usize, s: CursorSettings) -> Result<()> {
        self.settings(u, s)
    }
    fn tick(&mut self) -> Result<(bool, Vec<State>)> {
        self.tick()
    }
    fn mode(&mut self, m: Mode, k: bool) -> Result<()> {
        self.mode(m, k)
    }
    fn move_to(&mut self, u: usize, x: i32, y: i32) -> Result<()> {
        self.move_to(u, x, y)
    }
}

#[cfg(test)]
mod text_scope_tests {
    use super::*;
    #[test]
    fn user_resume_is_blocked_by_global_pause_or_cleanup_without_changing_peers() {
        let mut states = [
            UserPauseState {
                user: 0,
                supported: true,
                paused: true,
                ..Default::default()
            },
            UserPauseState {
                user: 1,
                supported: true,
                ..Default::default()
            },
        ];
        let peer = serde_json::to_value(&states[1]).unwrap();
        assert!(states[0].resume(true, 0).is_err());
        assert!(states[0].paused);
        assert!(states[0].resume(false, 1).is_err());
        assert!(states[0].paused);
        states[0].error = Some("saved UP failed".into());
        assert!(states[0].resume(false, 0).is_err());
        assert!(states[0].paused);
        states[0].error = None;
        states[0].resume(false, 0).unwrap();
        assert!(!states[0].paused);
        assert_eq!(serde_json::to_value(&states[1]).unwrap(), peer);
        assert!(
            UserPauseState::unsupported(9, "Unsupported")
                .resume(false, 0)
                .is_err()
        );
    }
    #[test]
    fn pause_provider_capability_uses_persisted_paths_not_resolved_handle_bits() {
        let mut seat: mux_core::Seat = serde_json::from_value(serde_json::json!({"id":"a", "name":"A", "mouse":"virtual://mouse/1", "keyboard":"virtual://keyboard/1", "target_pid":10})).unwrap();
        assert!(virtual_pause_devices(&seat).unwrap());
        seat.mouse = "physical-mouse-interface-path".into();
        assert!(!virtual_pause_devices(&seat).unwrap());
        seat.mouse = "virtual://mouse/1".into();
        seat.keyboard = Some("physical-keyboard-interface-path".into());
        assert!(!virtual_pause_devices(&seat).unwrap());
        seat.keyboard = None;
        assert!(!virtual_pause_devices(&seat).unwrap());
    }
    #[test]
    fn automatic_pause_requires_independent_concurrent_routing_and_blocks_mode_changes_while_quarantined()
     {
        assert!(automatic_pause_supported(Mode::Concurrent, true));
        assert!(!automatic_pause_supported(Mode::Concurrent, false));
        assert!(!automatic_pause_supported(Mode::Switched, true));
        assert!(!automatic_pause_supported(Mode::Switched, false));
        let mut states = vec![UserPauseState {
            user: 0,
            supported: true,
            paused: true,
            ..Default::default()
        }];
        assert!(validate_user_pause_mode_change(&states).is_err());
        states[0].paused = false;
        states[0].pending_releases = 1;
        assert!(validate_user_pause_mode_change(&states).is_err());
        states[0].pending_releases = 0;
        assert!(validate_user_pause_mode_change(&states).is_ok());
    }

    #[derive(Default)]
    struct ScopeEngine {
        active: bool,
        begins: usize,
        ends: usize,
        packets: usize,
        reject_begin: bool,
    }
    impl Engine for ScopeEngine {
        fn begin_text_delivery(&mut self, _: usize) -> Result<()> {
            anyhow::ensure!(!self.reject_begin && !self.active, "Cannot begin Text");
            self.active = true;
            self.begins += 1;
            Ok(())
        }
        fn end_text_delivery(&mut self, _: usize) {
            assert!(self.active);
            self.active = false;
            self.ends += 1;
        }
        fn input(&mut self, _: Packet) -> Result<()> {
            assert!(self.active, "Text packet escaped its recipient scope");
            self.packets += 1;
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
        fn tick(&mut self) -> Result<(bool, Vec<State>)> {
            Ok((false, Vec::new()))
        }
    }

    #[test]
    fn failed_text_scope_clears_before_another_action() {
        let mut engine = ScopeEngine::default();
        let result: Result<()> = with_text_recipient(&mut engine, 0, |router| {
            router.input(Packet::text_unit(0xF1000001, b'A' as u16))?;
            anyhow::bail!("Injected second-unit delivery failure")
        });
        assert!(result.is_err());
        assert!(!engine.active);
        with_text_recipient(&mut engine, 1, |router| {
            router.input(Packet::text_unit(0xF1000003, b'B' as u16))
        })
        .unwrap();
        assert_eq!((engine.begins, engine.ends, engine.packets), (2, 2, 2));
        assert!(!engine.active);
    }

    #[test]
    fn unwinding_text_scope_clears_its_recipient() {
        let mut engine = ScopeEngine::default();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = with_text_recipient::<()>(&mut engine, 0, |_| {
                panic!("Injected Text dispatch panic")
            });
        }));
        assert!(panic.is_err());
        assert!(!engine.active);
        assert_eq!((engine.begins, engine.ends), (1, 1));
    }

    #[test]
    fn rejected_or_nested_text_begin_does_not_end_another_scope() {
        let mut engine = ScopeEngine {
            reject_begin: true,
            ..Default::default()
        };
        assert!(
            with_text_recipient::<()>(&mut engine, 0, |_| panic!("Rejected begin dispatched"))
                .is_err()
        );
        assert_eq!((engine.begins, engine.ends), (0, 0));
        engine.reject_begin = false;
        with_text_recipient(&mut engine, 0, |router| {
            assert!(
                with_text_recipient::<()>(router, 0, |_| panic!("Nested begin dispatched"))
                    .is_err()
            );
            router.input(Packet::text_unit(0xF1000001, b'C' as u16))
        })
        .unwrap();
        assert_eq!((engine.begins, engine.ends, engine.packets), (1, 1, 1));
        assert!(!engine.active);
    }
}
